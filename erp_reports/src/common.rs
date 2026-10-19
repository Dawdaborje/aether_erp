//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::today;

use aether_sdk::prelude::*;

pub const READER_ROLE: &str = "erp_reports.reports_reader";

pub fn require_reader() -> Result<()> {
    context::current()?.require_role(READER_ROLE)
}
