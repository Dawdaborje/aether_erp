//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_quality.quality_admin";
pub const USER_ROLE: &str = "erp_quality.quality_user";
pub const READER_ROLE: &str = "erp_quality.quality_reader";

pub const QTY_SCALE: u32 = 6;

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

/// The roles of the plugins that ask `inspection_status` before they release goods.
const GATE_ROLES: &[&str] = &[
    "erp_procurement.purchase_user",
    "erp_procurement.purchase_admin",
    "erp_selling.sales_user",
    "erp_selling.sales_admin",
    "erp_inventory.stock_user",
    "erp_inventory.stock_admin",
    "erp_manufacturing.mfg_user",
    "erp_manufacturing.mfg_admin",
];

/// The status question is open to the people who receive, deliver and produce.
pub fn require_gate_reader() -> Result<()> {
    let context = context::current()?;
    if GATE_ROLES.iter().any(|r| context.has_role(r)) {
        return Ok(());
    }
    require_reader()
}

/// A decimal field of a record, if it has a value.
pub fn opt_dec(record: &Record, field: &str) -> Result<Option<Decimal>> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => {
            let parsed: Decimal =
                serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            Ok(Some(parsed.with_scale(QTY_SCALE)?))
        }
        None => Ok(None),
    }
}
