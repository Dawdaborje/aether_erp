//! Quotations, customer credit limits and sales settings.

use aether_sdk::dates::{format_date, parse_date, Duration};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, company_currency_code, dec, id_of, money_of, price_line, qty_of, require, require_admin, require_customer,
    require_reader, require_user, resolve_sell_price, text, today, Record, MONEY_SCALE, QTY_SCALE,
};
use crate::orders::{api_add_so_line, api_create_so};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct CreateQuotation {
    company: String,
    customer: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    quote_date: Option<String>,
    #[serde(default)]
    valid_until: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    payment_term: Option<String>,
    #[serde(default)]
    price_list: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct AddQuoteLine {
    #[serde(alias = "id")]
    quotation: String,
    item: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    tax: Option<String>,
    #[serde(default)]
    optional: Option<bool>,
}

#[derive(Deserialize)]
struct Convert {
    id: String,
    /// Optional lines the customer took.
    #[serde(default)]
    take_optional: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SetCredit {
    company: String,
    customer: String,
    credit_limit: Decimal,
    #[serde(default)]
    on_hold: Option<bool>,
}

#[derive(Deserialize)]
struct SetSetting {
    company: String,
    #[serde(default)]
    enforce_credit_limit: Option<bool>,
    #[serde(default)]
    default_payment_term: Option<String>,
    #[serde(default)]
    default_price_list: Option<String>,
    #[serde(default)]
    quote_valid_days: Option<i64>,
    #[serde(default)]
    require_inspection: Option<bool>,
}

#[derive(Deserialize)]
struct CheckCredit {
    company: String,
    customer: String,
    /// What the new order would add (default 0).
    #[serde(default)]
    amount: Option<Decimal>,
}

fn setting_for(company: &str) -> Result<Option<Record>> {
    db::find("sales_setting").filter("company", company).first()
}

fn quote_lines(id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("quotation_line").filter("quotation", id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn do_create_quotation(input: CreateQuotation) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let customer = require_customer(&input.customer)?;
    let setting = setting_for(&company_id)?;
    let currency = match input.currency.filter(|c| !c.is_empty()) {
        Some(c) => c.to_uppercase(),
        None => company_currency_code(&company_id)?,
    };
    let quote_date = match input.quote_date.filter(|d| !d.is_empty()) {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let valid_until = match input.valid_until.filter(|d| !d.is_empty()) {
        Some(d) => {
            if parse_date(&d)? < parse_date(&quote_date)? {
                return Err(Error::msg("the quotation expires before it is dated"));
            }
            Some(d)
        }
        None => {
            let days = setting.as_ref().and_then(|s| s.get("quote_valid_days")).and_then(Value::as_i64).unwrap_or(30);
            Some(format_date(parse_date(&quote_date)? + Duration::days(days)))
        }
    };
    let mut data = json!({
        "company": company_id,
        "customer": id_of(&customer)?,
        "currency": currency,
        "state": "draft",
        "quote_date": quote_date,
        "subtotal": Decimal::zero(MONEY_SCALE),
        "tax_total": Decimal::zero(MONEY_SCALE),
        "total": Decimal::zero(MONEY_SCALE),
    });
    if let Some(v) = valid_until {
        data["valid_until"] = json!(v);
    }
    if let Some(w) = input.warehouse.filter(|w| !w.is_empty()) {
        let _ = require("erp_inventory.stock_warehouse", &w, "warehouse")?;
        data["warehouse"] = json!(w);
    }
    let term = input
        .payment_term
        .filter(|t| !t.is_empty())
        .or_else(|| setting.as_ref().and_then(|s| text(s, "default_payment_term").map(str::to_string)));
    if let Some(t) = term {
        let _ = require("erp_pricing.payment_term", &t, "payment term")?;
        data["payment_term"] = json!(t);
    }
    let list = input
        .price_list
        .filter(|l| !l.is_empty())
        .or_else(|| setting.as_ref().and_then(|s| text(s, "default_price_list").map(str::to_string)));
    if let Some(l) = list {
        let _ = require("erp_pricing.price_list", &l, "price list")?;
        data["price_list"] = json!(l);
    }
    if let Some(n) = input.notes.filter(|n| !n.trim().is_empty()) {
        data["notes"] = json!(n.trim());
    }
    db::create("quotation", &data).map_err(|e| e.or("could not create the quotation"))
}

fn do_add_quote_line(input: AddQuoteLine) -> Result<Record> {
    require_user()?;
    let quote = require("quotation", &input.quotation, "quotation")?;
    if text(&quote, "state") != Some("draft") {
        return Err(Error::msg("lines can only be added while the quotation is a draft"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let (unit_price, list_discount) = resolve_sell_price(&quote, "quote_date", id_of(&item)?, qty, input.unit_price)?;
    if unit_price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    let discount_pct = input.discount_pct.unwrap_or(list_discount).with_scale(4)?;
    let tax = input.tax.filter(|t| !t.is_empty());
    let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
    let existing = quote_lines(id_of(&quote)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "quotation": id_of(&quote)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": text(&item, "uom"),
        "unit_price": unit_price,
        "discount_pct": discount_pct,
        "amount": priced.net,
        "tax_amount": priced.tax,
        "optional": input.optional.unwrap_or(false),
    });
    if let Some(t) = &tax {
        data["tax"] = json!(t);
    }
    let line: Record = db::create("quotation_line", &data).map_err(|e| e.or("could not add the line"))?;
    // Totals cover the firm lines only; optional lines are priced but not summed.
    let mut subtotal = Decimal::zero(MONEY_SCALE);
    let mut tax_total = Decimal::zero(MONEY_SCALE);
    for l in quote_lines(id_of(&quote)?)?.iter().filter(|l| l.get("optional") != Some(&json!(true))) {
        subtotal = subtotal.checked_add(money_of(l, "amount")?)?;
        tax_total = tax_total.checked_add(money_of(l, "tax_amount")?)?;
    }
    let _: Option<Record> = db::update(
        "quotation",
        id_of(&quote)?,
        &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
    )?;
    Ok(line)
}

fn do_send_quotation(input: Id) -> Result<Record> {
    require_user()?;
    let quote = require("quotation", &input.id, "quotation")?;
    if text(&quote, "state") != Some("draft") {
        return Err(Error::msg("only a draft quotation can be sent"));
    }
    if quote_lines(&input.id)?.is_empty() {
        return Err(Error::msg("add at least one line before sending"));
    }
    let (number, next, counter_id) =
        allocate_number(text(&quote, "company").unwrap_or_default(), "QT", text(&quote, "quote_date").unwrap_or_default())?;
    db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("quotation", &input.id, &json!({ "state": "sent", "number": number }))
        .run()?;
    require("quotation", &input.id, "quotation")
}

fn set_state(id: &str, from: &[&str], to: &str) -> Result<Record> {
    require_user()?;
    let quote = require("quotation", id, "quotation")?;
    if !from.contains(&text(&quote, "state").unwrap_or_default()) {
        return Err(Error::msg(format!("a quotation in state `{}` cannot become {to}", text(&quote, "state").unwrap_or("?"))));
    }
    db::update("quotation", id, &json!({ "state": to }))?.ok_or_else(|| Error::msg("the quotation is gone"))
}

fn do_convert(input: Convert) -> Result<Value> {
    require_user()?;
    let quote = require("quotation", &input.id, "quotation")?;
    if !matches!(text(&quote, "state"), Some("sent" | "accepted")) {
        return Err(Error::msg("only a sent or accepted quotation can become an order"));
    }
    if let Some(until) = text(&quote, "valid_until") {
        if until < today()?.as_str() {
            let _: Option<Record> = db::update("quotation", &input.id, &json!({ "state": "expired" }))?;
            return Err(Error::msg(format!("the quotation expired on {until}")));
        }
    }
    let taken = input.take_optional.unwrap_or_default();
    let lines: Vec<Record> = quote_lines(&input.id)?
        .into_iter()
        .filter(|l| l.get("optional") != Some(&json!(true)) || taken.iter().any(|t| Some(t.as_str()) == text(l, "id")))
        .collect();
    if lines.is_empty() {
        return Err(Error::msg("the quotation has no lines to order"));
    }
    let so = api_create_so(json!({
        "company": text(&quote, "company"),
        "customer": text(&quote, "customer"),
        "currency": text(&quote, "currency"),
        "warehouse": text(&quote, "warehouse"),
        "notes": format!("From quotation {}", text(&quote, "number").unwrap_or("")),
        "payment_term": text(&quote, "payment_term"),
        "price_list": text(&quote, "price_list"),
    }))?;
    let so_id = id_of(&so)?.to_string();
    for line in &lines {
        // The quoted price and discount are honoured exactly, whatever the price list says today.
        api_add_so_line(json!({
            "so": so_id,
            "item": text(line, "item"),
            "qty": qty_of(line, "qty")?,
            "unit_price": dec(line, "unit_price", 6)?,
            "discount_pct": dec(line, "discount_pct", 4)?,
            "tax": text(line, "tax"),
        }))?;
    }
    let done = db::update::<Record>("quotation", &input.id, &json!({ "state": "converted", "so": so_id }))?
        .ok_or_else(|| Error::msg("the quotation is gone"))?;
    Ok(json!({ "quotation": done, "so": require("so", &so_id, "sales order")? }))
}

fn do_set_credit(input: SetCredit) -> Result<Record> {
    require_admin()?;
    let company = require("company.company", &input.company, "company")?;
    let customer = require_customer(&input.customer)?;
    let limit = input.credit_limit.with_scale(MONEY_SCALE)?;
    if limit.is_negative() {
        return Err(Error::msg("the credit limit must not be negative"));
    }
    let existing: Option<Record> = db::find("customer_credit")
        .filter("company", id_of(&company)?)
        .filter("customer", id_of(&customer)?)
        .first()?;
    let mut data = json!({ "credit_limit": limit });
    if let Some(h) = input.on_hold {
        data["on_hold"] = json!(h);
    }
    match existing {
        Some(row) => db::update("customer_credit", id_of(&row)?, &data)?.ok_or_else(|| Error::msg("the credit record is gone")),
        None => {
            data["company"] = json!(id_of(&company)?);
            data["customer"] = json!(id_of(&customer)?);
            db::create("customer_credit", &data).map_err(|e| e.or("could not save the credit limit"))
        }
    }
}

fn do_set_setting(input: SetSetting) -> Result<Record> {
    require_admin()?;
    let company = require("company.company", &input.company, "company")?;
    let mut data = json!({});
    if let Some(v) = input.enforce_credit_limit {
        data["enforce_credit_limit"] = json!(v);
    }
    if let Some(v) = input.require_inspection {
        data["require_inspection"] = json!(v);
    }
    if let Some(t) = input.default_payment_term {
        if !t.is_empty() {
            let _ = require("erp_pricing.payment_term", &t, "payment term")?;
        }
        data["default_payment_term"] = if t.is_empty() { Value::Null } else { json!(t) };
    }
    if let Some(l) = input.default_price_list {
        if !l.is_empty() {
            let _ = require("erp_pricing.price_list", &l, "price list")?;
        }
        data["default_price_list"] = if l.is_empty() { Value::Null } else { json!(l) };
    }
    if let Some(d) = input.quote_valid_days {
        if d <= 0 {
            return Err(Error::msg("quotations stay valid for at least one day"));
        }
        data["quote_valid_days"] = json!(d);
    }
    match setting_for(id_of(&company)?)? {
        Some(row) => db::update("sales_setting", id_of(&row)?, &data)?.ok_or_else(|| Error::msg("the setting is gone")),
        None => {
            data["company"] = json!(id_of(&company)?);
            db::create("sales_setting", &data).map_err(|e| e.or("could not save the setting"))
        }
    }
}

/// What the customer already owes or is about to owe: uninvoiced value of open orders plus open receivables.
fn exposure(company: &str, customer: &str) -> Result<Decimal> {
    let mut total = Decimal::zero(MONEY_SCALE);
    let orders: Vec<Record> = db::find("so").filter("company", company).filter("customer", customer).limit(2000).all()?;
    for so in orders
        .iter()
        .filter(|o| matches!(text(o, "state"), Some("confirmed" | "partially_delivered" | "delivered")))
    {
        for line in db::find::<Record>("so_line").filter("so", id_of(so)?).limit(500).all()? {
            let qty = qty_of(&line, "qty")?;
            let open = qty.checked_sub(qty_of(&line, "qty_invoiced")?)?;
            if qty.is_zero() || open.is_zero() || open.is_negative() {
                continue;
            }
            let value = money_of(&line, "amount")?.checked_add(money_of(&line, "tax_amount")?)?;
            total = total.checked_add(value.times(open, 6)?.div(qty, MONEY_SCALE)?)?;
        }
    }
    // Open receivables from the payments plugin, when it is installed (documents are not netted by part payments).
    if let Ok(docs) = db::find::<Record>("gl_payment.pay_document")
        .filter("party", customer)
        .filter("state", "open")
        .filter("side", "receivable")
        .limit(2000)
        .all()
    {
        for d in docs {
            total = total.checked_add(money_of(&d, "total")?)?;
        }
    }
    Ok(total)
}

fn do_check_credit(input: CheckCredit) -> Result<Value> {
    require_reader()?;
    let company = require("company.company", &input.company, "company")?;
    let customer = require_customer(&input.customer)?;
    let row: Option<Record> = db::find("customer_credit")
        .filter("company", id_of(&company)?)
        .filter("customer", id_of(&customer)?)
        .first()?;
    let owed = exposure(id_of(&company)?, id_of(&customer)?)?;
    let adding = input.amount.unwrap_or_else(|| Decimal::zero(MONEY_SCALE)).with_scale(MONEY_SCALE)?;
    let after = owed.checked_add(adding)?;
    let (limit, on_hold) = match &row {
        Some(r) => (Some(money_of(r, "credit_limit")?), r.get("on_hold") == Some(&json!(true))),
        None => (None, false),
    };
    let ok = !on_hold && limit.map_or(true, |l| after <= l);
    Ok(json!({ "exposure": owed, "after": after, "credit_limit": limit, "on_hold": on_hold, "ok": ok }))
}

/// With the quality plugin installed and `require_inspection` on for the company, goods leave only
/// against an accepted inspection of the order. Without the plugin there is nothing to ask.
pub(crate) fn require_accepted_inspection(so: &Record) -> Result<()> {
    let company = text(so, "company").unwrap_or_default();
    let on = setting_for(company)?.map_or(false, |s| s.get("require_inspection") == Some(&json!(true)));
    if !on || !plugins::available("erp_quality")? {
        return Ok(());
    }
    let status: Value = plugins::call(
        "erp_quality",
        "inspection_status",
        &json!({ "source_kind": "erp_selling.so", "source_id": id_of(so)? }),
    )?;
    match status.get("state").and_then(Value::as_str) {
        Some("accepted") => Ok(()),
        Some("rejected") => Err(Error::msg("the inspection of this order was rejected")),
        Some("pending") => Err(Error::msg("finish the inspection of this order first")),
        _ => Err(Error::msg("this order needs an accepted inspection before goods are delivered")),
    }
}

/// Refuse to confirm an order that takes a customer past their limit or on hold, unless an administrator does it.
pub(crate) fn enforce_credit(so: &Record) -> Result<()> {
    let company = text(so, "company").unwrap_or_default();
    let customer = text(so, "customer").unwrap_or_default();
    let on = setting_for(company)?.map_or(false, |s| s.get("enforce_credit_limit") == Some(&json!(true)));
    if !on || context::current()?.has_role(crate::common::ADMIN_ROLE) {
        return Ok(());
    }
    let row: Option<Record> = db::find("customer_credit").filter("company", company).filter("customer", customer).first()?;
    let Some(row) = row else { return Ok(()) };
    if row.get("on_hold") == Some(&json!(true)) {
        return Err(Error::msg("this customer is on credit hold"));
    }
    // Exposure counts confirmed orders only, so the order being confirmed is added on top.
    let after = exposure(company, customer)?.checked_add(money_of(so, "total")?)?;
    let limit = money_of(&row, "credit_limit")?;
    if after > limit {
        return Err(Error::msg(format!("this order takes the customer to {after}, over their credit limit of {limit}")));
    }
    Ok(())
}

handler! {
    fn create_quotation(input: CreateQuotation) -> Record { do_create_quotation(input) }
    fn add_quotation_line(input: AddQuoteLine) -> Record { do_add_quote_line(input) }
    fn send_quotation(input: Id) -> Record { do_send_quotation(input) }
    fn accept_quotation(input: Id) -> Record { set_state(&input.id, &["sent"], "accepted") }
    fn reject_quotation(input: Id) -> Record { set_state(&input.id, &["sent", "accepted"], "rejected") }
    fn convert_quotation_to_so(input: Convert) -> Value { do_convert(input) }
    fn set_credit_limit(input: SetCredit) -> Record { do_set_credit(input) }
    fn set_sales_setting(input: SetSetting) -> Record { do_set_setting(input) }
    fn check_credit(input: CheckCredit) -> Value { do_check_credit(input) }
}
