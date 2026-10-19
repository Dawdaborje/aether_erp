//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const MANAGER_ROLE: &str = "erp_pos.pos_manager";
pub const CASHIER_ROLE: &str = "erp_pos.pos_cashier";
pub const READER_ROLE: &str = "erp_pos.pos_reader";

pub const QTY_SCALE: u32 = 6;
pub const PRICE_SCALE: u32 = 6;
pub const MONEY_SCALE: u32 = 2;

pub fn require_manager() -> Result<()> {
    context::current()?.require_role(MANAGER_ROLE)
}

pub fn is_manager() -> bool {
    context::current().map(|c| c.has_role(MANAGER_ROLE)).unwrap_or(false)
}

pub fn require_cashier() -> Result<()> {
    let context = context::current()?;
    if context.has_role(CASHIER_ROLE) || context.has_role(MANAGER_ROLE) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{CASHIER_ROLE}` to do this")))
    }
}

pub fn require_reader() -> Result<()> {
    let context = context::current()?;
    if [READER_ROLE, CASHIER_ROLE, MANAGER_ROLE].iter().any(|r| context.has_role(r)) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{READER_ROLE}` to do this")))
    }
}

/// The account id (`users:…`) of the person at the till.
pub fn caller() -> Result<String> {
    context::current()?.actor.id.ok_or_else(|| Error::msg("sign in to use the till"))
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

pub fn money_of(record: &Record, field: &str) -> Result<Decimal> {
    dec(record, field, MONEY_SCALE)
}

/// A session the caller may act on: open, and theirs unless they manage the till.
pub fn open_session_of(id: &str) -> Result<Record> {
    let session = require("pos_session", id, "session")?;
    if text(&session, "state") != Some("open") {
        return Err(Error::msg("the session is closed"));
    }
    if text(&session, "cashier") != Some(caller()?.as_str()) && !is_manager() {
        return Err(Error::msg("this session belongs to another cashier"));
    }
    Ok(session)
}
