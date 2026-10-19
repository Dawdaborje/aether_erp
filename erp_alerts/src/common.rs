//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, text, today, Record};

use aether_sdk::prelude::*;

pub const USER_ROLE: &str = "erp_alerts.alerts_user";
pub const ADMIN_ROLE: &str = "erp_alerts.alerts_admin";

pub fn require_user() -> Result<()> {
    let context = context::current()?;
    if context.has_role(USER_ROLE) || context.has_role(ADMIN_ROLE) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{USER_ROLE}` to do this")))
    }
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

/// The account id (`users:…`) of the person making the call.
pub fn caller() -> Result<String> {
    context::current()?
        .actor
        .id
        .ok_or_else(|| Error::msg("sign in to manage alerts"))
}
