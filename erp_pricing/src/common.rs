//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_pricing.pricing_admin";
pub const READER_ROLE: &str = "erp_pricing.pricing_reader";

pub const QTY_SCALE: u32 = 6;
pub const PRICE_SCALE: u32 = 6;
pub const MONEY_SCALE: u32 = 2;
pub const PCT_SCALE: u32 = 4;

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

/// Roles that may read taxes and prices (buying and selling call into this plugin).
const READ_ROLES: &[&str] = &[
    READER_ROLE,
    ADMIN_ROLE,
    "erp_procurement.purchase_reader",
    "erp_procurement.purchase_user",
    "erp_procurement.purchase_admin",
    "erp_selling.sales_reader",
    "erp_selling.sales_user",
    "erp_selling.sales_admin",
];

pub fn require_reader() -> Result<()> {
    let context = context::current()?;
    if READ_ROLES.iter().any(|r| context.has_role(r)) {
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
