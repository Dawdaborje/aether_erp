//! List sources and the production dashboard.

use std::collections::BTreeMap;

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{dec, qty_of, require_reader, text, Record};

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

fn list(model: &str, f: &Filter, search: &[&str], newest: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>(model).limit(500);
    if let Some(s) = f.state.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("state", s);
    }
    let mut rows = q.all()?;
    if let Some(needle) = f.q.as_deref().map(str::to_lowercase).filter(|n| !n.is_empty()) {
        rows.retain(|r| search.iter().any(|f| text(r, f).map_or(false, |v| v.to_lowercase().contains(&needle))));
    }
    rows.sort_by(|a, b| text(b, newest).cmp(&text(a, newest)));
    Ok(rows)
}

/// Orders by state, units in progress, and planned against logged minutes of finished operations.
fn do_dashboard(_: Filter) -> Result<Value> {
    require_reader()?;
    let orders: Vec<Record> = db::find("mo").limit(2000).all()?;
    let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
    let mut in_progress_qty = Decimal::zero(6);
    for mo in &orders {
        let state = text(mo, "state").unwrap_or("draft").to_string();
        if matches!(state.as_str(), "confirmed" | "reserved" | "in_progress") {
            in_progress_qty = in_progress_qty.checked_add(qty_of(mo, "qty")?)?;
        }
        *by_state.entry(state).or_default() += 1;
    }
    let ops: Vec<Record> = db::find("mo_operation").limit(5000).all()?;
    let mut planned = Decimal::zero(2);
    let mut actual = Decimal::zero(2);
    for op in ops.iter().filter(|o| text(o, "state") == Some("done")) {
        planned = planned.checked_add(dec(op, "planned_minutes", 2)?)?;
        actual = actual.checked_add(dec(op, "actual_minutes", 2)?)?;
    }
    let efficiency = if actual.is_zero() { Value::Null } else { json!(planned.times(Decimal::parse("100")?, 2)?.div(actual, 1)?) };
    Ok(json!({
        "open_orders": by_state.iter().filter(|(k, _)| matches!(k.as_str(), "confirmed" | "reserved" | "in_progress")).map(|(_, v)| v).sum::<u64>(),
        "units_in_progress": in_progress_qty,
        "done_orders": by_state.get("done").copied().unwrap_or(0),
        "planned_minutes": planned,
        "actual_minutes": actual,
        "efficiency_pct": efficiency,
        "orders_by_state": by_state.into_iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct ParentId {
    id: String,
}

fn children(model: &str, parent_field: &str, id: &str, order: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>(model).filter(parent_field, id).limit(500).all()?;
    rows.sort_by_key(|r| r.get(order).and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

handler! {
    fn list_bom_lines(input: ParentId) -> Vec<Record> { children("bom_line", "bom", &input.id, "line_no") }
    fn list_bom_operations(input: ParentId) -> Vec<Record> { children("bom_operation", "bom", &input.id, "seq") }
    fn list_mo_operations(input: ParentId) -> Vec<Record> { children("mo_operation", "mo", &input.id, "seq") }
    fn list_mos(f: Filter) -> Vec<Record> { list("mo", &f, &["number"], "planned_date") }
    fn list_boms(f: Filter) -> Vec<Record> { list("bom", &f, &["code"], "code") }
    fn list_work_centers(f: Filter) -> Vec<Record> { list("work_center", &f, &["code", "name"], "code") }
    fn production_dashboard(f: Filter) -> Value { do_dashboard(f) }
}
