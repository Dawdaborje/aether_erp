//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_warehouse.wh_admin";
pub const USER_ROLE: &str = "erp_warehouse.wh_user";
pub const READER_ROLE: &str = "erp_warehouse.wh_reader";

pub const QTY_SCALE: u32 = 6;

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

/// A decimal in a record or aggregate row; a missing value is zero.
pub fn dec(value: &Value, field: &str) -> Result<Decimal> {
    match value.get(field).filter(|v| !v.is_null()) {
        Some(v) => {
            let parsed: Decimal =
                serde_json::from_value(v.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            parsed.with_scale(QTY_SCALE)
        }
        None => Ok(Decimal::zero(QTY_SCALE)),
    }
}

/// A decimal field of a record (alias of [`dec`], for readability at call sites).
pub fn qty_of_value(value: &Value, field: &str) -> Result<Decimal> {
    dec(value, field)
}
