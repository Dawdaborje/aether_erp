//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_replenishment.replenish_admin";
pub const READER_ROLE: &str = "erp_replenishment.replenish_reader";

pub const QTY_SCALE: u32 = 6;

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

pub fn require_reader() -> Result<()> {
    let context = context::current()?;
    if context.has_role(READER_ROLE) || context.has_role(ADMIN_ROLE) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{READER_ROLE}` to do this")))
    }
}

pub fn qty_of(record: &Record, field: &str) -> Result<Decimal> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => {
            let parsed: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            parsed.with_scale(QTY_SCALE)
        }
        None => Ok(Decimal::zero(QTY_SCALE)),
    }
}
