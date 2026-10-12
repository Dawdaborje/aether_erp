//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::rules::{QTY_SCALE, VALUE_SCALE};

pub const ADMIN_ROLE: &str = "erp_inventory.stock_admin";
pub const USER_ROLE: &str = "erp_inventory.stock_user";
pub const READER_ROLE: &str = "erp_inventory.stock_reader";

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

pub const WAREHOUSE_KINDS: &[&str] = &["stock", "view", "transit", "scrap"];
pub const HOLDING_KINDS: &[&str] = &["stock", "transit"];

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

pub fn value_of(record: &Record, field: &str) -> Result<Decimal> {
    dec(record, field, VALUE_SCALE)
}

pub fn company_currency_code(company_id: &str) -> Result<String> {
    let company = require("company.company", company_id, "company")?;
    let currency_id = text(&company, "currency").ok_or_else(|| Error::msg("the company has no currency"))?;
    let currency = require("currency.currency", currency_id, "currency")?;
    text(&currency, "code")
        .map(|c| c.to_string())
        .ok_or_else(|| Error::msg("the currency has no code"))
}

pub fn valuation_for(item: &str, company: &str, as_of: &str) -> Result<(String, Decimal)> {
    let reply: Value = plugins::call(
        "erp_item",
        "valuation_for",
        &json!({ "item": item, "company": company, "as_of": as_of }),
    )?;
    let method = reply
        .get("valuation_method")
        .and_then(Value::as_str)
        .unwrap_or("fifo")
        .to_string();
    let cost = match reply.get("standard_cost") {
        Some(v) if !v.is_null() => serde_json::from_value::<Decimal>(v.clone())?.with_scale(VALUE_SCALE)?,
        _ => Decimal::zero(VALUE_SCALE),
    };
    Ok((method, cost))
}
