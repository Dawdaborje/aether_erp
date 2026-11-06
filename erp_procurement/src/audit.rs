//! The History tab of a document: who changed the header and its lines, when, and what from and to.
//!
//! Built from the kernel's version store (`history::list` and `history::diff`), so it holds for every write,
//! whichever page or function made it. Plugins cannot share code, so this file is copied into each plugin that
//! has documents; keep the copies identical apart from `LINK_MODELS`.
//!
//! Limit: it lists the lines that exist now. A line that was removed is not listed (its removal shows in the
//! document's own timeline as the total changing).

use std::collections::BTreeMap;

use aether_sdk::history;
use aether_sdk::prelude::*;

use crate::common::{require_reader, text, Record};

/// Field name to the model that names what it points at, for readable values.
const LINK_MODELS: &[(&str, &str)] = &[
    ("item", "erp_item.item"),
    ("supplier", "party.party"),
    ("warehouse", "erp_inventory.stock_warehouse"),
    ("payment_term", "erp_pricing.payment_term"),
    ("price_list", "erp_pricing.price_list"),
    ("tax", "erp_pricing.tax"),
    ("po", "po"),
];

/// Figures worked out from others: listing them would only repeat the quantity, price and total that caused them.
const DERIVED: &[&str] = &["subtotal", "tax_total", "amount", "tax_amount", "net_unit_price"];

const VERSIONS_PER_RECORD: usize = 30;
const LINES: u32 = 100;
const ROWS: usize = 200;

#[derive(Deserialize)]
pub(crate) struct Id {
    pub id: String,
}

struct Names {
    cache: BTreeMap<String, String>,
}

impl Names {
    fn of(&mut self, field: &str, value: &str) -> String {
        if let Some(hit) = self.cache.get(value) {
            return hit.clone();
        }
        let label = LINK_MODELS
            .iter()
            .find(|(f, _)| *f == field)
            .and_then(|(_, model)| db::get::<Record>(model, value).ok().flatten())
            .and_then(|r| {
                ["display_name", "name", "number", "code"].iter().find_map(|k| text(&r, k).map(str::to_string))
            })
            .unwrap_or_else(|| value.rsplit(':').next().unwrap_or(value).chars().take(8).collect());
        self.cache.insert(value.to_string(), label.clone());
        label
    }

    /// Who made a change: the account's name when it can be read, else a short form of its id.
    fn actor(&mut self, id: &str) -> String {
        if let Some(hit) = self.cache.get(id) {
            return hit.clone();
        }
        let label = ["users", "user.user", "base_ui.user"]
            .iter()
            .find_map(|m| db::get::<Record>(m, id).ok().flatten())
            .and_then(|r| ["display_name", "name", "username", "email"].iter().find_map(|k| text(&r, k).map(str::to_string)))
            .unwrap_or_else(|| id.rsplit(':').next().unwrap_or(id).chars().take(8).collect());
        self.cache.insert(id.to_string(), label.clone());
        label
    }

    fn show(&mut self, field: &str, v: &Value) -> String {
        match v {
            Value::Null => "empty".to_string(),
            Value::String(s) if s.contains(':') && !s.contains(' ') && !s.starts_with("20") => self.of(field, s),
            Value::String(s) => s.clone(),
            Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
            other => other.to_string().trim_matches('"').to_string(),
        }
    }
}

fn pretty(field: &str) -> String {
    let mut s = field.replace('_', " ");
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

/// The readable lines for one record: one per version.
fn record_rows(names: &mut Names, model: &str, id: &str, what: &str, out: &mut Vec<Value>) -> Result<()> {
    let versions = history::list(model, id, None)?;
    for v in versions.iter().take(VERSIONS_PER_RECORD) {
        let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let op = v.get("op").and_then(Value::as_str).unwrap_or("");
        let change = match op {
            "create" => "Created".to_string(),
            "delete" => "Deleted".to_string(),
            "restore" => "Restored to an earlier version".to_string(),
            "update" if seq > 1 => {
                let Some(d) = history::diff(model, id, seq - 1, Some(seq))? else { continue };
                let parts: Vec<String> = d
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|cs| {
                        cs.iter()
                            .filter(|c| !c.get("field").and_then(Value::as_str).is_some_and(|f| DERIVED.contains(&f)))
                            .map(|c| {
                                let f = c.get("field").and_then(Value::as_str).unwrap_or("?");
                                format!("{}: {} → {}", pretty(f), names.show(f, &c["before"]), names.show(f, &c["after"]))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if parts.is_empty() {
                    continue;
                }
                parts.join("; ")
            }
            _ => continue,
        };
        let who = v.get("actor").and_then(Value::as_str).unwrap_or("");
        out.push(json!({
            "id": format!("{id}:{seq}"),
            "at": v.get("at"),
            "who": names.actor(who),
            "what": what,
            "change": change,
        }));
    }
    Ok(())
}

/// History of a header and the lines it has now, newest first. `lines` is `(line model, parent field)`.
pub(crate) fn document_history(header_model: &str, header_label: &str, id: &str, lines: Option<(&str, &str)>) -> Result<Vec<Value>> {
    require_reader()?;
    let mut names = Names { cache: BTreeMap::new() };
    let mut out = Vec::new();
    record_rows(&mut names, header_model, id, header_label, &mut out)?;
    if let Some((line_model, parent)) = lines {
        let rows: Vec<Record> = db::find(line_model).filter(parent, id).limit(LINES).all()?;
        for line in &rows {
            let Some(line_id) = text(line, "id") else { continue };
            let item = text(line, "item").map(|i| names.of("item", i)).unwrap_or_default();
            let no = line.get("line_no").and_then(Value::as_i64).unwrap_or(0);
            record_rows(&mut names, line_model, line_id, &format!("Line {no} {item}"), &mut out)?;
        }
    }
    out.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
    out.truncate(ROWS);
    Ok(out)
}

handler! {
    /// Who changed this purchase order and its lines, and what.
    fn po_history(input: Id) -> Vec<Value> { document_history("po", "Order", &input.id, Some(("po_line", "po"))) }

    /// Who changed this bill and its lines, and what.
    fn bill_history(input: Id) -> Vec<Value> { document_history("purchase_bill", "Bill", &input.id, Some(("purchase_bill_line", "bill"))) }
}
