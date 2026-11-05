//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, pick, require, text, today, Record};

use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "erp_item.item_admin";
pub const READER_ROLE: &str = "erp_item.item_reader";

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

/// Roles that may read items / valuation (stock, buying, and manufacturing call into this plugin).
const READ_ROLES: &[&str] = &[
    READER_ROLE,
    ADMIN_ROLE,
    "erp_inventory.stock_reader",
    "erp_inventory.stock_user",
    "erp_inventory.stock_admin",
    "erp_procurement.purchase_reader",
    "erp_procurement.purchase_user",
    "erp_procurement.purchase_admin",
    "erp_manufacturing.mfg_reader",
    "erp_manufacturing.mfg_user",
    "erp_manufacturing.mfg_admin",
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

pub const METHODS: &[&str] = &["fifo", "average", "standard"];
pub const TRACKING: &[&str] = &["none", "lot", "serial"];
