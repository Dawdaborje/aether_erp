//! Scrap: write damaged or spoiled stock off. A scrap is an adjustment that removes stock, with
//! `source_kind` `erp_inventory.scrap` and a required reason, so the books see a loss (not a sale).

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_user, text, Record};
use crate::moves::{api_add_line, api_draft, api_post};

pub const SCRAP_SOURCE: &str = "erp_inventory.scrap";

#[derive(Deserialize)]
struct ScrapInput {
    company: String,
    warehouse: String,
    item: String,
    qty: Decimal,
    reason: String,
    #[serde(default)]
    lot: Option<String>,
    #[serde(default)]
    posting_date: Option<String>,
    /// Retrying with the same key returns the same scrap instead of writing off twice.
    #[serde(default)]
    idem_key: Option<String>,
}

fn do_scrap(input: ScrapInput) -> Result<Record> {
    require_user()?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the stock is scrapped"));
    }
    if input.qty.is_zero() || input.qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    if text(&warehouse, "kind") == Some("scrap") {
        return Err(Error::msg("that warehouse already holds scrap"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let mut draft = json!({
        "company": input.company,
        "source_kind": SCRAP_SOURCE,
        "reason": input.reason.trim(),
    });
    if let Some(date) = input.posting_date.filter(|d| !d.is_empty()) {
        draft["posting_date"] = json!(date);
    }
    if let Some(key) = input.idem_key.filter(|k| !k.is_empty()) {
        draft["idem_key"] = json!(key);
    }
    let move_row = api_draft("adjust", draft)?;
    if text(&move_row, "state") == Some("posted") {
        return Ok(move_row);
    }
    let move_id = id_of(&move_row)?.to_string();
    let mut line = json!({
        "move": move_id,
        "item": id_of(&item)?,
        "qty": input.qty,
        "warehouse": id_of(&warehouse)?,
        "increase": false,
    });
    if let Some(lot) = input.lot.filter(|l| !l.is_empty()) {
        line["lot"] = json!(lot);
    }
    api_add_line(line)?;
    api_post(&move_id)
}

handler! {
    fn scrap_stock(input: ScrapInput) -> Record { do_scrap(input) }
}
