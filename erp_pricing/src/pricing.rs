//! Taxes, payment terms, price lists and the line calculator.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{dec, id_of, require, require_admin, require_reader, text, today, Record, PCT_SCALE, PRICE_SCALE, QTY_SCALE};
use crate::rules::{add_days, compute_line_multi, pick_price, Component, PriceRow, TaxKind, TaxSpec};

const APPLIES: &[&str] = &["both", "sales", "purchase"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct TaxInput {
    code: String,
    name: String,
    #[serde(default)]
    kind: Option<String>,
    rate: Decimal,
    #[serde(default)]
    applies_to: Option<String>,
    #[serde(default)]
    price_includes_tax: Option<bool>,
    #[serde(default)]
    account: Option<String>,
    /// Taken off what is paid (a withholding) instead of added to it.
    #[serde(default)]
    is_withholding: Option<bool>,
}

#[derive(Deserialize)]
struct ComponentIn {
    tax: String,
    #[serde(default)]
    compound: Option<bool>,
}

#[derive(Deserialize)]
struct TaxGroupInput {
    code: String,
    name: String,
    #[serde(default)]
    applies_to: Option<String>,
    components: Vec<ComponentIn>,
}

#[derive(Deserialize)]
struct SetTax {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    rate: Option<Decimal>,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct ListTaxes {
    #[serde(default)]
    applies_to: Option<String>,
}

#[derive(Deserialize)]
struct TermInput {
    code: String,
    name: String,
    net_days: i64,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    discount_days: Option<i64>,
}

#[derive(Deserialize)]
struct DueDate {
    term: String,
    date: String,
}

#[derive(Deserialize)]
struct PriceListInput {
    code: String,
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    is_default: Option<bool>,
}

#[derive(Deserialize)]
struct SetItemPrice {
    #[serde(alias = "id")]
    price_list: String,
    item: String,
    unit_price: Decimal,
    #[serde(default)]
    min_qty: Option<Decimal>,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    party: Option<String>,
    #[serde(default)]
    valid_from: Option<String>,
    #[serde(default)]
    valid_to: Option<String>,
}

#[derive(Deserialize)]
struct PriceFor {
    item: String,
    /// `sell` or `buy`.
    kind: String,
    qty: Decimal,
    #[serde(default)]
    price_list: Option<String>,
    #[serde(default)]
    party: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    company: Option<String>,
}

#[derive(Deserialize)]
struct ComputeLine {
    qty: Decimal,
    unit_price: Decimal,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    tax: Option<String>,
    /// `sales` or `purchase`: refuses a tax that does not apply to this side.
    #[serde(default)]
    side: Option<String>,
}

fn tax_spec(tax: &Record) -> Result<TaxSpec> {
    let kind = match text(tax, "kind") {
        Some("fixed") => TaxKind::Fixed,
        _ => TaxKind::Percent,
    };
    Ok(TaxSpec {
        kind,
        rate: dec(tax, "rate", PCT_SCALE)?,
        included: tax.get("price_includes_tax") == Some(&json!(true)),
    })
}

fn do_create_tax(input: TaxInput) -> Result<Record> {
    require_admin()?;
    let kind = input.kind.as_deref().unwrap_or("percent");
    if !matches!(kind, "percent" | "fixed") {
        return Err(Error::msg("kind is percent or fixed"));
    }
    let applies = input.applies_to.as_deref().unwrap_or("both");
    if !APPLIES.contains(&applies) {
        return Err(Error::msg("applies_to is both, sales or purchase"));
    }
    let rate = input.rate.with_scale(PCT_SCALE)?;
    if rate.is_negative() {
        return Err(Error::msg("the rate must not be negative"));
    }
    let included = input.price_includes_tax.unwrap_or(false);
    if included && kind == "fixed" {
        return Err(Error::msg("a fixed tax cannot be included in the price"));
    }
    let withholding = input.is_withholding.unwrap_or(false);
    if withholding && included {
        return Err(Error::msg("a withholding cannot be included in the price"));
    }
    let mut data = json!({
        "code": input.code.trim().to_uppercase(),
        "name": input.name.trim(),
        "kind": kind,
        "rate": rate,
        "applies_to": applies,
        "price_includes_tax": included,
        "is_withholding": withholding,
        "active": true,
    });
    if let Some(account) = input.account.filter(|a| !a.trim().is_empty()) {
        data["account"] = json!(account.trim());
    }
    db::create("tax", &data).map_err(|e| e.or("could not create the tax (the code may be taken)"))
}

fn do_set_tax(input: SetTax) -> Result<Record> {
    require_admin()?;
    let _ = require("tax", &input.id, "tax")?;
    let mut changes = json!({});
    if let Some(name) = input.name {
        changes["name"] = json!(name.trim());
    }
    if let Some(rate) = input.rate {
        let rate = rate.with_scale(PCT_SCALE)?;
        if rate.is_negative() {
            return Err(Error::msg("the rate must not be negative"));
        }
        changes["rate"] = json!(rate);
    }
    if let Some(account) = input.account {
        changes["account"] = json!(account.trim());
    }
    if let Some(active) = input.active {
        changes["active"] = json!(active);
    }
    db::update("tax", &input.id, &changes)?.ok_or_else(|| Error::msg("the tax is gone"))
}

fn do_list_taxes(input: ListTaxes) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find("tax").filter("active", true).limit(200).all()?;
    if let Some(side) = input.applies_to.filter(|s| !s.is_empty()) {
        rows.retain(|r| matches!(text(r, "applies_to"), Some("both")) || text(r, "applies_to") == Some(side.as_str()));
    }
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}

fn do_get_tax(input: Id) -> Result<Record> {
    require_reader()?;
    require("tax", &input.id, "tax")
}

fn do_create_payment_term(input: TermInput) -> Result<Record> {
    require_admin()?;
    if input.net_days < 0 {
        return Err(Error::msg("net days must not be negative"));
    }
    let pct = input.discount_pct.unwrap_or_else(|| Decimal::zero(PCT_SCALE)).with_scale(PCT_SCALE)?;
    let days = input.discount_days.unwrap_or(0);
    if pct.is_negative() || days < 0 || days > input.net_days {
        return Err(Error::msg("the discount window must lie within the net days"));
    }
    db::create(
        "payment_term",
        &json!({
            "code": input.code.trim().to_uppercase(),
            "name": input.name.trim(),
            "net_days": input.net_days,
            "discount_pct": pct,
            "discount_days": days,
            "active": true,
        }),
    )
    .map_err(|e| e.or("could not create the payment term (the code may be taken)"))
}

/// Due date (and early-payment discount window) a term gives a document dated `date`.
fn do_due_date_for(input: DueDate) -> Result<Value> {
    require_reader()?;
    let term = require("payment_term", &input.term, "payment term")?;
    let date = parse_date(&input.date)?;
    let net = term.get("net_days").and_then(Value::as_i64).unwrap_or(0);
    let discount_days = term.get("discount_days").and_then(Value::as_i64).unwrap_or(0);
    let pct = dec(&term, "discount_pct", PCT_SCALE)?;
    let discount_until = if pct.is_zero() { Value::Null } else { json!(format_date(add_days(date, discount_days))) };
    Ok(json!({
        "due_date": format_date(add_days(date, net)),
        "discount_pct": pct,
        "discount_until": discount_until,
    }))
}

fn do_create_price_list(input: PriceListInput) -> Result<Record> {
    require_admin()?;
    let kind = input.kind.as_deref().unwrap_or("sell");
    if !matches!(kind, "sell" | "buy") {
        return Err(Error::msg("kind is sell or buy"));
    }
    let make_default = input.is_default.unwrap_or(false);
    if make_default {
        let rows: Vec<Record> = db::find("price_list").filter("kind", kind).filter("is_default", true).limit(50).all()?;
        for row in rows {
            let _: Option<Record> = db::update("price_list", id_of(&row)?, &json!({ "is_default": false }))?;
        }
    }
    let mut data = json!({
        "code": input.code.trim().to_uppercase(),
        "name": input.name.trim(),
        "kind": kind,
        "is_default": make_default,
        "active": true,
    });
    if let Some(currency) = input.currency.filter(|c| !c.trim().is_empty()) {
        data["currency"] = json!(currency.trim().to_uppercase());
    }
    if let Some(company) = input.company.filter(|c| !c.trim().is_empty()) {
        data["company"] = json!(company.trim());
    }
    db::create("price_list", &data).map_err(|e| e.or("could not create the price list (the code may be taken)"))
}

fn do_set_item_price(input: SetItemPrice) -> Result<Record> {
    require_admin()?;
    let list = require("price_list", &input.price_list, "price list")?;
    let item = require("erp_item.item", &input.item, "item")?;
    let unit_price = input.unit_price.with_scale(PRICE_SCALE)?;
    if unit_price.is_negative() {
        return Err(Error::msg("the price must not be negative"));
    }
    let min_qty = input.min_qty.unwrap_or_else(|| Decimal::zero(QTY_SCALE)).with_scale(QTY_SCALE)?;
    if min_qty.is_negative() {
        return Err(Error::msg("the minimum quantity must not be negative"));
    }
    let from = input.valid_from.filter(|d| !d.is_empty());
    let to = input.valid_to.filter(|d| !d.is_empty());
    if let Some(d) = &from {
        parse_date(d)?;
    }
    if let Some(d) = &to {
        parse_date(d)?;
    }
    if let (Some(f), Some(t)) = (&from, &to) {
        if parse_date(t)? < parse_date(f)? {
            return Err(Error::msg("valid_to is before valid_from"));
        }
    }
    let mut data = json!({
        "price_list": id_of(&list)?,
        "item": id_of(&item)?,
        "min_qty": min_qty,
        "unit_price": unit_price,
        "discount_pct": input.discount_pct.unwrap_or_else(|| Decimal::zero(PCT_SCALE)).with_scale(PCT_SCALE)?,
        "active": true,
    });
    if let Some(party) = input.party.filter(|p| !p.is_empty()) {
        let party = require("party.party", &party, "party")?;
        data["party"] = json!(id_of(&party)?);
    }
    if let Some(d) = from {
        data["valid_from"] = json!(d);
    }
    if let Some(d) = to {
        data["valid_to"] = json!(d);
    }
    db::create("price_list_item", &data).map_err(|e| e.or("could not save the price"))
}

/// Best price for an item: the named list, else the kind's default list. Returns `found: false` (not an
/// error) when no list or row applies so the caller can fall back to a typed price.
fn do_price_for(input: PriceFor) -> Result<Value> {
    require_reader()?;
    if !matches!(input.kind.as_str(), "sell" | "buy") {
        return Err(Error::msg("kind is sell or buy"));
    }
    let on = match input.date.filter(|d| !d.is_empty()) {
        Some(d) => parse_date(&d)?,
        None => parse_date(&today()?)?,
    };
    let list = match input.price_list.filter(|l| !l.is_empty()) {
        Some(id) => Some(require("price_list", &id, "price list")?),
        None => db::find::<Record>("price_list")
            .filter("kind", input.kind.as_str())
            .filter("is_default", true)
            .filter("active", true)
            .first()?,
    };
    let Some(list) = list else {
        return Ok(json!({ "found": false, "reason": "no price list" }));
    };
    if text(&list, "kind") != Some(input.kind.as_str()) {
        return Err(Error::msg("the price list is for the other side (sell vs buy)"));
    }
    if list.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the price list is not active"));
    }
    if let (Some(want), Some(have)) = (input.currency.as_deref(), text(&list, "currency")) {
        if !want.eq_ignore_ascii_case(have) {
            return Ok(json!({ "found": false, "reason": "currency mismatch" }));
        }
    }
    if let (Some(want), Some(have)) = (input.company.as_deref(), text(&list, "company")) {
        if want != have {
            return Ok(json!({ "found": false, "reason": "company mismatch" }));
        }
    }
    let list_id = id_of(&list)?.to_string();
    let rows: Vec<Record> = db::find("price_list_item")
        .filter("price_list", list_id.as_str())
        .filter("item", input.item.as_str())
        .filter("active", true)
        .limit(500)
        .all()?;
    let mut candidates = Vec::new();
    for row in &rows {
        let party = text(row, "party");
        if party.is_some() && party != input.party.as_deref() {
            continue;
        }
        candidates.push(PriceRow {
            id: id_of(row)?.to_string(),
            min_qty: dec(row, "min_qty", QTY_SCALE)?,
            unit_price: dec(row, "unit_price", PRICE_SCALE)?,
            discount_pct: dec(row, "discount_pct", PCT_SCALE)?,
            valid_from: text(row, "valid_from").map(parse_date).transpose()?,
            valid_to: text(row, "valid_to").map(parse_date).transpose()?,
            party_specific: party.is_some(),
        });
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    match pick_price(&candidates, qty, on) {
        Some(row) => Ok(json!({
            "found": true,
            "price_list": list_id,
            "row": row.id,
            "unit_price": row.unit_price,
            "discount_pct": row.discount_pct,
            "min_qty": row.min_qty,
        })),
        None => Ok(json!({ "found": false, "reason": "no price for this item and quantity" })),
    }
}

/// Amounts for one document line: gross, discount, net (taxable base), tax and total.
/// The taxes a line is priced with, in order: the tax itself, or a group's components. The record
/// of each (for its code and ledger account) comes with it.
fn components_of(tax: &Record, side: Option<&str>) -> Result<Vec<(Record, Component)>> {
    let check_side = |t: &Record| -> Result<()> {
        if t.get("active") == Some(&json!(false)) {
            return Err(Error::msg(format!("the tax `{}` is not active", text(t, "code").unwrap_or("?"))));
        }
        if let Some(side) = side {
            let applies = text(t, "applies_to").unwrap_or("both");
            if applies != "both" && applies != side {
                return Err(Error::msg(format!("the tax `{}` does not apply to {side}", text(t, "code").unwrap_or("?"))));
            }
        }
        Ok(())
    };
    let one = |t: &Record, compound: bool| -> Result<Component> {
        Ok(Component {
            spec: tax_spec(t)?,
            compound,
            withholding: t.get("is_withholding") == Some(&json!(true)),
        })
    };
    check_side(tax)?;
    if text(tax, "kind") != Some("group") {
        return Ok(vec![(tax.clone(), one(tax, false)?)]);
    }
    let mut rows: Vec<Record> = db::find("tax_component").filter("group", id_of(tax)?).limit(50).all()?;
    rows.sort_by_key(|r| r.get("position").and_then(Value::as_i64).unwrap_or(0));
    if rows.is_empty() {
        return Err(Error::msg("the tax group has no taxes"));
    }
    let mut out = Vec::new();
    for row in rows {
        let comp = require("tax", text(&row, "component").unwrap_or_default(), "tax")?;
        if text(&comp, "kind") == Some("group") {
            return Err(Error::msg("a tax group cannot contain another group"));
        }
        check_side(&comp)?;
        let compound = row.get("compound") == Some(&json!(true));
        out.push((comp.clone(), one(&comp, compound)?));
    }
    Ok(out)
}

fn do_compute_line(input: ComputeLine) -> Result<Value> {
    require_reader()?;
    let taxes = match input.tax.filter(|t| !t.is_empty()) {
        Some(id) => {
            let tax = require("tax", &id, "tax")?;
            components_of(&tax, input.side.as_deref())?
        }
        None => Vec::new(),
    };
    let discount = input.discount_pct.unwrap_or_else(|| Decimal::zero(PCT_SCALE));
    let specs: Vec<Component> = taxes.iter().map(|(_, c)| *c).collect();
    let m = compute_line_multi(input.qty, input.unit_price, discount, &specs)?;
    let a = m.line;
    let breakdown: Vec<Value> = taxes
        .iter()
        .zip(&m.amounts)
        .map(|((record, c), amount)| {
            json!({
                "tax": id_of(record).ok(),
                "code": text(record, "code"),
                "account": text(record, "account"),
                "amount": amount,
                "withholding": c.withholding,
            })
        })
        .collect();
    Ok(json!({
        "gross": a.gross,
        "discount": a.discount,
        "net": a.net,
        "tax": a.tax,
        "total": a.total,
        "net_unit": a.net_unit,
        "breakdown": breakdown,
    }))
}

fn do_create_tax_group(input: TaxGroupInput) -> Result<Record> {
    require_admin()?;
    let applies = input.applies_to.as_deref().unwrap_or("both");
    if !APPLIES.contains(&applies) {
        return Err(Error::msg("applies_to is both, sales or purchase"));
    }
    if input.components.is_empty() {
        return Err(Error::msg("a tax group needs at least one tax"));
    }
    let mut components = Vec::new();
    let mut specs = Vec::new();
    for (i, c) in input.components.iter().enumerate() {
        let tax = require("tax", &c.tax, "tax")?;
        if text(&tax, "kind") == Some("group") {
            return Err(Error::msg("a tax group cannot contain another group"));
        }
        let tax_applies = text(&tax, "applies_to").unwrap_or("both");
        if applies != "both" && tax_applies != "both" && tax_applies != applies {
            return Err(Error::msg(format!("the tax `{}` does not apply to {applies}", text(&tax, "code").unwrap_or("?"))));
        }
        let compound = c.compound.unwrap_or(false);
        specs.push(Component {
            spec: tax_spec(&tax)?,
            compound,
            withholding: tax.get("is_withholding") == Some(&json!(true)),
        });
        components.push(json!({ "position": i as i64 + 1, "component": id_of(&tax)?, "compound": compound }));
    }
    // Refuse a combination that cannot be priced (included mixed with added, compound included, ...).
    compute_line_multi(Decimal::whole(1, 0)?, Decimal::whole(100, 0)?, Decimal::zero(PCT_SCALE), &specs)?;
    db::create(
        "tax",
        &json!({
            "code": input.code.trim().to_uppercase(),
            "name": input.name.trim(),
            "kind": "group",
            "rate": Decimal::zero(PCT_SCALE),
            "applies_to": applies,
            "price_includes_tax": false,
            "active": true,
            "components": components,
        }),
    )
    .map_err(|e| e.or("could not create the tax group (the code may be taken)"))
}

/// (code, name, net days, early-payment discount %, discount days)
const SEED_TERMS: &[(&str, &str, i64, i64, i64)] = &[
    ("IMMEDIATE", "Due on receipt", 0, 0, 0),
    ("NET15", "Net 15 days", 15, 0, 0),
    ("NET30", "Net 30 days", 30, 0, 0),
    ("NET60", "Net 60 days", 60, 0, 0),
    ("2-10-NET30", "2% in 10 days, net 30", 30, 2, 10),
];

/// Starter payment terms and one default sell and buy price list, each only when missing.
/// Taxes are left to the organization: a rate is specific to a country.
fn do_seed() -> Result<Value> {
    let mut terms = 0;
    for (code, name, net_days, pct, discount_days) in SEED_TERMS {
        if db::find::<Record>("payment_term").filter("code", *code).first()?.is_some() {
            continue;
        }
        db::create::<Record>(
            "payment_term",
            &json!({
                "code": code,
                "name": name,
                "net_days": net_days,
                "discount_pct": Decimal::whole(*pct, PCT_SCALE)?,
                "discount_days": discount_days,
                "active": true,
            }),
        )?;
        terms += 1;
    }
    let mut lists = 0;
    for (code, name, kind) in [("SELL", "Standard selling prices", "sell"), ("BUY", "Standard buying prices", "buy")] {
        if db::find::<Record>("price_list").filter("kind", kind).first()?.is_some() {
            continue;
        }
        db::create::<Record>(
            "price_list",
            &json!({ "code": code, "name": name, "kind": kind, "is_default": true, "active": true }),
        )?;
        lists += 1;
    }
    Ok(json!({ "payment_terms": terms, "price_lists": lists }))
}

handler! {
    /// Starter payment terms and default price lists; runs on install and upgrade.
    fn seed_pricing(_: Empty) -> Value { do_seed() }

    fn create_tax(input: TaxInput) -> Record { do_create_tax(input) }
    fn set_tax(input: SetTax) -> Record { do_set_tax(input) }
    fn list_taxes(input: ListTaxes) -> Vec<Record> { do_list_taxes(input) }
    fn get_tax(input: Id) -> Record { do_get_tax(input) }
    fn create_payment_term(input: TermInput) -> Record { do_create_payment_term(input) }
    fn due_date_for(input: DueDate) -> Value { do_due_date_for(input) }
    fn create_price_list(input: PriceListInput) -> Record { do_create_price_list(input) }
    fn set_item_price(input: SetItemPrice) -> Record { do_set_item_price(input) }
    fn price_for(input: PriceFor) -> Value { do_price_for(input) }
    fn create_tax_group(input: TaxGroupInput) -> Record { do_create_tax_group(input) }
    fn compute_line(input: ComputeLine) -> Value { do_compute_line(input) }
}
