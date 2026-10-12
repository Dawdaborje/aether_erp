//! Sales orders, deliveries, invoices.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, company_currency_code, id_of, line_amount, money_of, price_of, qty_of, require, require_customer,
    require_reader, require_user, text, today, Record, MONEY_SCALE, PRICE_SCALE, QTY_SCALE,
};

#[derive(Deserialize)]
struct CreateSo {
    company: String,
    customer: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    order_date: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    deal: Option<String>,
}

#[derive(Deserialize)]
struct CreateSoFromDeal {
    deal: String,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct AddSoLine {
    so: String,
    item: String,
    qty: Decimal,
    unit_price: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct DeliverLine {
    so_line: String,
    qty: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct DeliverSo {
    so: String,
    #[serde(default)]
    delivery_date: Option<String>,
    lines: Vec<DeliverLine>,
}

#[derive(Deserialize)]
struct InvoiceLineIn {
    so_line: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    delivery_line: Option<String>,
}

#[derive(Deserialize)]
struct CreateInvoice {
    so: String,
    #[serde(default)]
    invoice_date: Option<String>,
    lines: Vec<InvoiceLineIn>,
}

fn so_lines(so_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("so_line").filter("so", so_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn refresh_so_state(so_id: &str) -> Result<Record> {
    let so = require("so", so_id, "sales order")?;
    if matches!(text(&so, "state"), Some("cancelled" | "closed" | "draft")) {
        return Ok(so);
    }
    let lines = so_lines(so_id)?;
    if lines.is_empty() {
        return Ok(so);
    }
    let mut any = false;
    let mut all = true;
    for line in &lines {
        let qty = qty_of(line, "qty")?;
        let delivered = qty_of(line, "qty_delivered")?;
        if !delivered.is_zero() {
            any = true;
        }
        if delivered < qty {
            all = false;
        }
    }
    let state = if all && any {
        "delivered"
    } else if any {
        "partially_delivered"
    } else {
        "confirmed"
    };
    db::update("so", so_id, &json!({ "state": state }))?.ok_or_else(|| Error::msg("the order is gone"))
}

fn do_create_so(input: CreateSo) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let customer = require_customer(&input.customer)?;
    let currency = match input.currency.filter(|c| !c.is_empty()) {
        Some(c) => c.to_uppercase(),
        None => company_currency_code(&company_id)?,
    };
    let order_date = match input.order_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let mut data = json!({
        "company": company_id,
        "customer": id_of(&customer)?,
        "currency": currency,
        "state": "draft",
        "order_date": order_date,
        "total": Decimal::zero(MONEY_SCALE),
    });
    if let Some(wh) = input.warehouse {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    if let Some(notes) = input.notes.filter(|s| !s.is_empty()) {
        data["notes"] = json!(notes);
    }
    if let Some(deal) = input.deal.filter(|d| !d.is_empty()) {
        data["deal"] = json!(deal.trim());
    }
    db::create("so", &data).map_err(|e| e.or("could not create the sales order"))
}

fn party_for_deal(deal: &Record) -> Result<String> {
    if let Some(customer_id) = text(deal, "customer") {
        let customer: Record = plugins::call("crm_customer", "get_customer", &json!({ "id": customer_id }))?;
        if let Some(party) = text(&customer, "party") {
            return Ok(party.to_string());
        }
    }
    if let Some(org) = text(deal, "org_party") {
        return Ok(org.to_string());
    }
    if let Some(contact) = text(deal, "contact_party") {
        return Ok(contact.to_string());
    }
    Err(Error::msg("the deal needs a customer party before a sales order can be created"))
}

fn do_create_so_from_deal(input: CreateSoFromDeal) -> Result<Record> {
    require_user()?;
    let deal: Record = plugins::call("crm_deal", "get_deal", &json!({ "id": input.deal }))?;
    let company = text(&deal, "company").ok_or_else(|| Error::msg("deal has no company"))?;
    let customer = party_for_deal(&deal)?;
    let currency = match text(&deal, "currency") {
        Some(id) => match require("currency.currency", id, "currency") {
            Ok(row) => text(&row, "code").map(str::to_string),
            Err(_) => Some(id.to_string()), // already a code
        },
        None => None,
    };
    let notes = input.notes.or_else(|| text(&deal, "title").map(|t| format!("From deal: {t}")));
    // Multiple SOs per deal are allowed; win stays on crm_deal.
    do_create_so(CreateSo {
        company: company.to_string(),
        customer,
        currency,
        order_date: None,
        warehouse: input.warehouse,
        notes,
        deal: Some(id_of(&deal)?.to_string()),
    })
}

fn do_add_so_line(input: AddSoLine) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if text(&so, "state") != Some("draft") {
        return Err(Error::msg("lines can only be added while the order is a draft"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    let unit_price = input.unit_price.with_scale(PRICE_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    if unit_price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let uom = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?;
    let amount = line_amount(qty, unit_price)?;
    let existing = so_lines(id_of(&so)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "so": id_of(&so)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": uom,
        "unit_price": unit_price,
        "amount": amount,
        "qty_delivered": Decimal::zero(QTY_SCALE),
        "qty_invoiced": Decimal::zero(QTY_SCALE),
    });
    if let Some(wh) = input.warehouse.or_else(|| text(&so, "warehouse").map(str::to_string)) {
        data["warehouse"] = json!(wh);
    }
    let line = db::create("so_line", &data).map_err(|e| e.or("could not add the line"))?;
    let total = existing
        .iter()
        .try_fold(amount, |acc, r| acc.checked_add(money_of(r, "amount")?))?;
    let _: Option<Record> = db::update("so", id_of(&so)?, &json!({ "total": total }))?;
    Ok(line)
}

fn do_confirm_so(input: Id) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    if text(&so, "state") != Some("draft") {
        return Err(Error::msg("only a draft order can be confirmed"));
    }
    if so_lines(&input.id)?.is_empty() {
        return Err(Error::msg("add at least one line before confirming"));
    }
    let company_id = text(&so, "company").unwrap_or_default();
    let date = text(&so, "order_date").unwrap_or_default();
    let (number, next, counter_id) = allocate_number(company_id, "SO", date)?;
    db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("so", &input.id, &json!({ "state": "confirmed", "number": number }))
        .run()?;
    require("so", &input.id, "sales order")
}

fn do_deliver_so(input: DeliverSo) -> Result<Value> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if matches!(text(&so, "state"), Some("draft" | "cancelled" | "closed")) {
        return Err(Error::msg("deliver a confirmed order"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to deliver"));
    }
    let company_id = text(&so, "company").unwrap_or_default().to_string();
    let customer_id = text(&so, "customer").unwrap_or_default().to_string();
    let delivery_date = match input.delivery_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };

    let stock_move: Record = plugins::call(
        "erp_inventory",
        "draft_issue",
        &json!({
            "company": company_id,
            "posting_date": delivery_date,
            "currency": text(&so, "currency"),
            "source_kind": "erp_selling.so",
            "source_id": input.so,
            "source_version": 1,
        }),
    )?;
    let move_id = id_of(&stock_move)?.to_string();
    let (number, next, counter_id) = allocate_number(&company_id, "SD", &delivery_date)?;
    let delivery: Record = db::create(
        "sales_delivery",
        &json!({
            "company": company_id,
            "so": input.so,
            "customer": customer_id,
            "stock_move": move_id,
            "state": "draft",
            "delivery_date": delivery_date,
            "number": number,
        }),
    )?;
    let delivery_id = id_of(&delivery)?.to_string();

    struct Planned {
        so_line_id: String,
        qty: Decimal,
        already: Decimal,
    }
    let mut planned = Vec::new();
    let mut line_no = 0i64;
    for spec in &input.lines {
        let so_line = require("so_line", &spec.so_line, "order line")?;
        if text(&so_line, "so") != Some(input.so.as_str()) {
            return Err(Error::msg("order line belongs to another sales order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("deliver quantity must be greater than zero"));
        }
        let ordered = qty_of(&so_line, "qty")?;
        let already = qty_of(&so_line, "qty_delivered")?;
        let open = ordered.checked_sub(already)?;
        if qty > open {
            return Err(Error::msg(format!("cannot deliver {qty}: only {open} open on the line")));
        }
        let warehouse = spec
            .warehouse
            .clone()
            .or_else(|| text(&so_line, "warehouse").map(str::to_string))
            .or_else(|| text(&so, "warehouse").map(str::to_string))
            .ok_or_else(|| Error::msg("name a warehouse to deliver from"))?;
        let item_id = text(&so_line, "item").unwrap_or_default();
        let _: Record = plugins::call(
            "erp_inventory",
            "add_line",
            &json!({
                "move": move_id,
                "item": item_id,
                "qty": qty,
                "warehouse": warehouse,
            }),
        )?;
        line_no += 1;
        let _: Record = db::create(
            "sales_delivery_line",
            &json!({
                "delivery": delivery_id,
                "line_no": line_no,
                "so_line": id_of(&so_line)?,
                "item": item_id,
                "qty": qty,
                "warehouse": warehouse,
            }),
        )?;
        planned.push(Planned {
            so_line_id: id_of(&so_line)?.to_string(),
            qty,
            already,
        });
    }

    let posted: Record = plugins::call("erp_inventory", "post_move", &json!({ "id": move_id }))?;
    let mut tx = db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("sales_delivery", &delivery_id, &json!({ "state": "posted" }));
    for row in &planned {
        let new_d = row.already.checked_add(row.qty)?;
        tx = tx.update("so_line", &row.so_line_id, &json!({ "qty_delivered": new_d }));
    }
    tx.run().map_err(|e| e.or("could not finish the delivery"))?;
    let so = refresh_so_state(&input.so)?;
    Ok(json!({
        "delivery": require("sales_delivery", &delivery_id, "delivery")?,
        "stock_move": posted,
        "so": so,
    }))
}

fn do_create_invoice(input: CreateInvoice) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if matches!(text(&so, "state"), Some("draft" | "cancelled")) {
        return Err(Error::msg("invoice a confirmed order"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to invoice"));
    }
    let company_id = text(&so, "company").unwrap_or_default().to_string();
    let invoice_date = match input.invoice_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let (number, next, counter_id) = allocate_number(&company_id, "SINV", &invoice_date)?;
    let invoice: Record = db::create(
        "sales_invoice",
        &json!({
            "company": company_id,
            "customer": text(&so, "customer"),
            "so": input.so,
            "currency": text(&so, "currency"),
            "state": "draft",
            "invoice_date": invoice_date,
            "total": Decimal::zero(MONEY_SCALE),
            "source_version": 1,
            "number": number,
        }),
    )?;
    let invoice_id = id_of(&invoice)?.to_string();
    let mut total = Decimal::zero(MONEY_SCALE);
    let mut line_no = 0i64;
    for spec in &input.lines {
        let so_line = require("so_line", &spec.so_line, "order line")?;
        if text(&so_line, "so") != Some(input.so.as_str()) {
            return Err(Error::msg("order line belongs to another sales order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("invoice quantity must be greater than zero"));
        }
        let delivered = qty_of(&so_line, "qty_delivered")?;
        let invoiced = qty_of(&so_line, "qty_invoiced")?;
        let open = delivered.checked_sub(invoiced)?;
        if qty > open {
            return Err(Error::msg(format!(
                "cannot invoice {qty}: only {open} delivered and not yet invoiced"
            )));
        }
        let unit_price = spec.unit_price.unwrap_or(price_of(&so_line, "unit_price")?).with_scale(PRICE_SCALE)?;
        let amount = line_amount(qty, unit_price)?;
        total = total.checked_add(amount)?;
        line_no += 1;
        let mut data = json!({
            "invoice": invoice_id,
            "line_no": line_no,
            "so_line": id_of(&so_line)?,
            "item": text(&so_line, "item"),
            "qty": qty,
            "unit_price": unit_price,
            "amount": amount,
        });
        if let Some(dl) = &spec.delivery_line {
            let _ = require("sales_delivery_line", dl, "delivery line")?;
            data["delivery_line"] = json!(dl);
        }
        let _: Record = db::create("sales_invoice_line", &data)?;
        let new_inv = invoiced.checked_add(qty)?;
        let _: Option<Record> = db::update("so_line", id_of(&so_line)?, &json!({ "qty_invoiced": new_inv }))?;
    }
    db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("sales_invoice", &invoice_id, &json!({ "total": total }))
        .run()?;
    require("sales_invoice", &invoice_id, "invoice")
}

fn do_approve_invoice(input: Id) -> Result<Record> {
    require_user()?;
    let invoice = require("sales_invoice", &input.id, "invoice")?;
    if text(&invoice, "state") != Some("draft") {
        return Err(Error::msg("only a draft invoice can be approved"));
    }
    let approved = db::update("sales_invoice", &input.id, &json!({ "state": "approved" }))?
        .ok_or_else(|| Error::msg("the invoice is gone"))?;
    let total = money_of(&approved, "total")?;
    events::emit(
        "sales_invoice_approved",
        &json!({
            "invoice": input.id,
            "number": text(&approved, "number"),
            "company": text(&approved, "company"),
            "customer": text(&approved, "customer"),
            "currency": text(&approved, "currency"),
            "invoice_date": text(&approved, "invoice_date"),
            "total": approved.get("total"),
            "ar_total": total,
            "revenue": total,
            "so": text(&approved, "so"),
            "source_version": approved.get("source_version").and_then(Value::as_i64).unwrap_or(1),
        }),
    )?;
    Ok(approved)
}

fn do_get_so(input: Id) -> Result<Value> {
    require_reader()?;
    let so = require("so", &input.id, "sales order")?;
    let lines = so_lines(&input.id)?;
    Ok(json!({ "so": so, "lines": lines }))
}

fn do_cancel_so(input: Id) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    if text(&so, "state") == Some("delivered") {
        return Err(Error::msg("a fully delivered order is closed, not cancelled"));
    }
    let lines = so_lines(&input.id)?;
    if lines.iter().any(|l| qty_of(l, "qty_delivered").map(|q| !q.is_zero()).unwrap_or(false)) {
        return Err(Error::msg("cancel only when nothing has been delivered"));
    }
    db::update("so", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the order is gone"))
}

handler! {
    fn create_so(input: CreateSo) -> Record { do_create_so(input) }
    fn create_so_from_deal(input: CreateSoFromDeal) -> Record { do_create_so_from_deal(input) }
    fn add_so_line(input: AddSoLine) -> Record { do_add_so_line(input) }
    fn confirm_so(input: Id) -> Record { do_confirm_so(input) }
    fn cancel_so(input: Id) -> Record { do_cancel_so(input) }
    fn deliver_so(input: DeliverSo) -> Value { do_deliver_so(input) }
    fn create_invoice(input: CreateInvoice) -> Record { do_create_invoice(input) }
    fn approve_invoice(input: Id) -> Record { do_approve_invoice(input) }
    fn get_so(input: Id) -> Value { do_get_so(input) }
}
