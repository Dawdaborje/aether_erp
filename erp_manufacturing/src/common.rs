//! Shared helpers.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::dates::{parse_date, Datelike};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_manufacturing.mfg_admin";
pub const USER_ROLE: &str = "erp_manufacturing.mfg_user";
pub const READER_ROLE: &str = "erp_manufacturing.mfg_reader";

pub const QTY_SCALE: u32 = 6;
pub const PCT_SCALE: u32 = 2;
pub const VALUE_SCALE: u32 = 6;

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

pub fn allocate_number(company_id: &str, prefix: &str, date: &str) -> Result<(String, i64, String)> {
    let year = parse_date(date)?.year();
    let existing: Option<Record> = db::find("mfg_counter")
        .filter("company", company_id)
        .filter("prefix", prefix)
        .filter("year", year)
        .first()?;
    let (counter_id, next) = match existing {
        Some(row) => (id_of(&row)?.to_string(), row.get("next").and_then(Value::as_i64).unwrap_or(1)),
        None => {
            let made: Record = db::create(
                "mfg_counter",
                &json!({ "company": company_id, "prefix": prefix, "year": year, "next": 1 }),
            )?;
            (id_of(&made)?.to_string(), 1)
        }
    };
    Ok((format!("{prefix}-{year}-{next:05}"), next, counter_id))
}
