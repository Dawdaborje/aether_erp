//! Shared helpers.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::dates::{parse_date, Datelike, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_selling.sales_admin";
pub const USER_ROLE: &str = "erp_selling.sales_user";
pub const READER_ROLE: &str = "erp_selling.sales_reader";

pub const QTY_SCALE: u32 = 6;
pub const PRICE_SCALE: u32 = 6;
pub const MONEY_SCALE: u32 = 2;

#[allow(dead_code)]
pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

pub fn require_user() -> Result<()> {
    let context = context::current()?;
    if context.has_role(USER_ROLE) || context.has_role(ADMIN_ROLE) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{USER_ROLE}` to do this")))
    }
}

pub fn require_reader() -> Result<()> {
    let context = context::current()?;
    if [READER_ROLE, USER_ROLE, ADMIN_ROLE].iter().any(|r| context.has_role(r)) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{READER_ROLE}` to do this")))
    }
}

pub fn require_customer(id: &str) -> Result<Record> {
    let party = require("party.party", id, "customer")?;
    // party plugin may use type/role fields; accept any active party as customer in v0.1
    if party.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the customer is not active"));
    }
    Ok(party)
}

#[allow(dead_code)]
pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn dec(record: &Record, field: &str, scale: u32) -> Result<Decimal> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => {
            let parsed: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            parsed.with_scale(scale)
        }
        None => Ok(Decimal::zero(scale)),
    }
}

pub fn qty_of(record: &Record, field: &str) -> Result<Decimal> {
    dec(record, field, QTY_SCALE)
}

pub fn money_of(record: &Record, field: &str) -> Result<Decimal> {
    dec(record, field, MONEY_SCALE)
}

pub fn price_of(record: &Record, field: &str) -> Result<Decimal> {
    dec(record, field, PRICE_SCALE)
}

pub fn company_currency_code(company_id: &str) -> Result<String> {
    let company = require("company.company", company_id, "company")?;
    let currency_id = text(&company, "currency").ok_or_else(|| Error::msg("the company has no currency"))?;
    let currency = require("currency.currency", currency_id, "currency")?;
    text(&currency, "code")
        .map(|c| c.to_string())
        .ok_or_else(|| Error::msg("the currency has no code"))
}

pub fn allocate_number(company_id: &str, prefix: &str, date: &str) -> Result<(String, i64, String)> {
    let year = parse_date(date)?.year();
    let existing: Option<Record> = db::find("sales_counter")
        .filter("company", company_id)
        .filter("prefix", prefix)
        .filter("year", year)
        .first()?;
    let (counter_id, next) = match existing {
        Some(row) => (id_of(&row)?.to_string(), row.get("next").and_then(Value::as_i64).unwrap_or(1)),
        None => {
            let made: Record = db::create(
                "sales_counter",
                &json!({ "company": company_id, "prefix": prefix, "year": year, "next": 1 }),
            )?;
            (id_of(&made)?.to_string(), 1)
        }
    };
    let number = format!("{prefix}-{year}-{next:05}");
    Ok((number, next, counter_id))
}

#[allow(dead_code)]
pub fn line_amount(qty: Decimal, unit_price: Decimal) -> Result<Decimal> {
    qty.with_scale(QTY_SCALE)?.times(unit_price.with_scale(PRICE_SCALE)?, MONEY_SCALE)
}

/// Amounts for one document line from `erp_pricing`.
pub struct PricedLine {
    pub net: Decimal,
    pub tax: Decimal,
}

pub fn price_line(qty: Decimal, unit_price: Decimal, discount_pct: Decimal, tax: Option<&str>) -> Result<PricedLine> {
    let out: Value = plugins::call(
        "erp_pricing",
        "compute_line",
        &json!({
            "qty": qty,
            "unit_price": unit_price,
            "discount_pct": discount_pct,
            "tax": tax,
            "side": "sales",
        }),
    )?;
    Ok(PricedLine { net: dec(&out, "net", MONEY_SCALE)?, tax: dec(&out, "tax", MONEY_SCALE)? })
}

/// Sum `amount` and `tax_amount` over a document's lines: `(subtotal, tax_total)`.
pub fn sum_lines(lines: &[Record]) -> Result<(Decimal, Decimal)> {
    let mut subtotal = Decimal::zero(MONEY_SCALE);
    let mut tax = Decimal::zero(MONEY_SCALE);
    for line in lines {
        subtotal = subtotal.checked_add(money_of(line, "amount")?)?;
        tax = tax.checked_add(money_of(line, "tax_amount")?)?;
    }
    Ok((subtotal, tax))
}

/// Unit price and list discount for a sell line: the typed price if given, else the document's (or the default)
/// sell price list. `doc` carries `customer`, `company`, `currency`, `price_list` and a date under `date_field`.
pub fn resolve_sell_price(doc: &Record, date_field: &str, item_id: &str, qty: Decimal, typed: Option<Decimal>) -> Result<(Decimal, Decimal)> {
    if let Some(p) = typed {
        return Ok((p.with_scale(PRICE_SCALE)?, Decimal::zero(4)));
    }
    let found: Value = plugins::call(
        "erp_pricing",
        "price_for",
        &json!({
            "item": item_id,
            "kind": "sell",
            "qty": qty,
            "price_list": text(doc, "price_list"),
            "party": text(doc, "customer"),
            "date": text(doc, date_field),
            "currency": text(doc, "currency"),
            "company": text(doc, "company"),
        }),
    )?;
    if found.get("found") != Some(&json!(true)) {
        return Err(Error::msg("no sell price found for this item; enter a unit price"));
    }
    Ok((price_of(&found, "unit_price")?, dec(&found, "discount_pct", 4)?))
}
