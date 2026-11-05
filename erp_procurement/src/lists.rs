//! List sources and the purchasing dashboard.

use std::collections::BTreeMap;

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{money_of, require, require_reader, text, Record, MONEY_SCALE};

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

fn list(model: &str, f: &Filter, search: &[&str], newest: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>(model).limit(500);
    if let Some(s) = f.state.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("state", s);
    }
    let mut rows = q.all()?;
    if let Some(k) = f.kind.as_deref().filter(|k| !k.is_empty()) {
        // Documents written before `kind` existed are plain bills.
        rows.retain(|r| text(r, "kind").unwrap_or("bill") == k);
    }
    if let Some(needle) = f.q.as_deref().map(str::to_lowercase).filter(|n| !n.is_empty()) {
        rows.retain(|r| search.iter().any(|f| text(r, f).map_or(false, |v| v.to_lowercase().contains(&needle))));
    }
    rows.sort_by(|a, b| text(b, newest).cmp(&text(a, newest)));
    Ok(rows)
}

fn name_of(model: &str, id: &str) -> String {
    require(model, id, "record")
        .ok()
        .and_then(|r| text(&r, "display_name").or_else(|| text(&r, "name")).map(str::to_string))
        .unwrap_or_else(|| id.to_string())
}

fn do_dashboard(_: Filter) -> Result<Value> {
    require_reader()?;
    let pos: Vec<Record> = db::find("po").limit(2000).all()?;
    let mut open = 0u64;
    let mut to_approve = 0u64;
    let mut open_value = Decimal::zero(MONEY_SCALE);
    let mut by_supplier: BTreeMap<String, Decimal> = BTreeMap::new();
    for po in &pos {
        match text(po, "state") {
            Some("confirmed" | "partially_received") => {
                open += 1;
                open_value = open_value.checked_add(money_of(po, "total")?)?;
            }
            Some("to_approve") => to_approve += 1,
            _ => {}
        }
        if !matches!(text(po, "state"), Some("draft" | "cancelled" | "to_approve")) {
            let e = by_supplier
                .entry(text(po, "supplier").unwrap_or_default().to_string())
                .or_insert_with(|| Decimal::zero(MONEY_SCALE));
            *e = e.checked_add(money_of(po, "total")?)?;
        }
    }
    let bills: Vec<Record> = db::find("purchase_bill").limit(2000).all()?;
    let drafts = bills.iter().filter(|b| text(b, "state") == Some("draft") && text(b, "kind") != Some("debit_note")).count();
    let exceptions = bills
        .iter()
        .filter(|b| text(b, "state") == Some("draft") && text(b, "release_to_pay") == Some("exception"))
        .count();
    let mut ranked: Vec<(String, Decimal)> = by_supplier.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    let top: Vec<Value> = ranked
        .into_iter()
        .take(8)
        .map(|(id, v)| json!({ "name": name_of("party.party", &id), "value": v }))
        .collect();
    Ok(json!({
        "open_orders": open,
        "awaiting_approval": to_approve,
        "open_value": open_value,
        "draft_bills": drafts,
        "bills_in_exception": exceptions,
        "spend_by_supplier": top,
    }))
}

#[derive(Deserialize)]
struct ParentId {
    id: String,
}

fn lines(model: &str, parent_field: &str, id: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>(model).filter(parent_field, id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

handler! {
    fn list_po_lines(input: ParentId) -> Vec<Record> { lines("po_line", "po", &input.id) }
    fn list_rfq_lines(input: ParentId) -> Vec<Record> { lines("rfq_line", "rfq", &input.id) }
    fn list_bill_lines(input: ParentId) -> Vec<Record> { lines("purchase_bill_line", "bill", &input.id) }
    fn list_pos(f: Filter) -> Vec<Record> { list("po", &f, &["number", "notes"], "order_date") }
    fn list_rfqs(f: Filter) -> Vec<Record> { list("rfq", &f, &["number", "request_group"], "rfq_date") }
    fn list_bills(f: Filter) -> Vec<Record> { list("purchase_bill", &f, &["number"], "bill_date") }
    fn list_receipts(f: Filter) -> Vec<Record> { list("purchase_receipt", &f, &["number"], "receipt_date") }
    fn list_returns(f: Filter) -> Vec<Record> { list("purchase_return", &f, &["number", "reason"], "return_date") }
    fn purchasing_dashboard(f: Filter) -> Value { do_dashboard(f) }
}
