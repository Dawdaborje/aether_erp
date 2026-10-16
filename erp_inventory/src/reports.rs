//! Stock balance, ageing, stock card, dashboard and list sources for the screens.

use std::collections::BTreeMap;

use aether_sdk::dates::{parse_date, Duration};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{qty_of, require, require_reader, text, today, value_of, Record};
use crate::rules::{QTY_SCALE, VALUE_SCALE};

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    as_of: Option<String>,
}

fn layers(f: &Filter) -> Result<Vec<Record>> {
    let mut q = db::find::<Record>("stock_layer").limit(5000);
    if let Some(c) = f.company.as_deref().filter(|c| !c.is_empty()) {
        q = q.filter("company", c);
    }
    if let Some(w) = f.warehouse.as_deref().filter(|w| !w.is_empty()) {
        q = q.filter("warehouse", w);
    }
    if let Some(i) = f.item.as_deref().filter(|i| !i.is_empty()) {
        q = q.filter("item", i);
    }
    q.all()
}

/// On-hand quantity and value per item and warehouse, from open layers.
fn do_stock_balance(f: Filter) -> Result<Value> {
    require_reader()?;
    let mut by: BTreeMap<(String, String), (Decimal, Decimal)> = BTreeMap::new();
    for layer in layers(&f)? {
        let qty = qty_of(&layer, "remaining_qty")?;
        if qty.is_zero() {
            continue;
        }
        let key = (
            text(&layer, "item").unwrap_or_default().to_string(),
            text(&layer, "warehouse").unwrap_or_default().to_string(),
        );
        let e = by.entry(key).or_insert((Decimal::zero(QTY_SCALE), Decimal::zero(VALUE_SCALE)));
        e.0 = e.0.checked_add(qty)?;
        e.1 = e.1.checked_add(value_of(&layer, "remaining_value")?)?;
    }
    let mut total_value = Decimal::zero(VALUE_SCALE);
    let mut rows = Vec::new();
    for ((item, warehouse), (qty, value)) in by {
        total_value = total_value.checked_add(value)?;
        let avg = if qty.is_zero() { Decimal::zero(VALUE_SCALE) } else { value.div(qty, VALUE_SCALE)? };
        rows.push(json!({ "item": item, "warehouse": warehouse, "qty": qty, "value": value, "avg_cost": avg }));
    }
    Ok(json!({ "rows": rows, "total_value": total_value }))
}

/// Value on hand by age since the layer opened: 0-30, 31-60, 61-90, over 90 days.
fn do_stock_ageing(f: Filter) -> Result<Value> {
    require_reader()?;
    let as_of = match f.as_of.clone().filter(|d| !d.is_empty()) {
        Some(d) => parse_date(&d)?,
        None => parse_date(&today()?)?,
    };
    let names = ["0-30", "31-60", "61-90", "90+"];
    let mut buckets = vec![(Decimal::zero(QTY_SCALE), Decimal::zero(VALUE_SCALE)); 4];
    let mut per_item: BTreeMap<String, Vec<(Decimal, Decimal)>> = BTreeMap::new();
    for layer in layers(&f)? {
        let qty = qty_of(&layer, "remaining_qty")?;
        if qty.is_zero() {
            continue;
        }
        let opened = match text(&layer, "opened_at") {
            Some(d) => parse_date(d)?,
            None => as_of,
        };
        let age = (as_of - opened).num_days().max(0);
        let idx = match age {
            0..=30 => 0,
            31..=60 => 1,
            61..=90 => 2,
            _ => 3,
        };
        let value = value_of(&layer, "remaining_value")?;
        buckets[idx].0 = buckets[idx].0.checked_add(qty)?;
        buckets[idx].1 = buckets[idx].1.checked_add(value)?;
        let row = per_item
            .entry(text(&layer, "item").unwrap_or_default().to_string())
            .or_insert_with(|| vec![(Decimal::zero(QTY_SCALE), Decimal::zero(VALUE_SCALE)); 4]);
        row[idx].0 = row[idx].0.checked_add(qty)?;
        row[idx].1 = row[idx].1.checked_add(value)?;
    }
    let summary: Vec<Value> = names
        .iter()
        .zip(&buckets)
        .map(|(n, (q, v))| json!({ "bucket": n, "qty": q, "value": v }))
        .collect();
    let items: Vec<Value> = per_item
        .into_iter()
        .map(|(item, b)| {
            let cells: Vec<Value> = names
                .iter()
                .zip(&b)
                .map(|(n, (q, v))| json!({ "bucket": n, "qty": q, "value": v }))
                .collect();
            json!({ "item": item, "buckets": cells })
        })
        .collect();
    Ok(json!({ "as_of": as_of.to_string(), "buckets": summary, "items": items }))
}

/// Move lines of an item with their move's date, number, kind and state (newest first).
fn do_stock_ledger(f: Filter) -> Result<Value> {
    require_reader()?;
    let item = f.item.clone().filter(|i| !i.is_empty()).ok_or_else(|| Error::msg("name an item"))?;
    let lines: Vec<Record> = db::find("stock_move_line").filter("item", item.as_str()).limit(1000).all()?;
    let mut moves: BTreeMap<String, Record> = BTreeMap::new();
    let mut rows = Vec::new();
    for line in lines {
        let move_id = text(&line, "move").unwrap_or_default().to_string();
        if !moves.contains_key(&move_id) {
            moves.insert(move_id.clone(), require("stock_move", &move_id, "move")?);
        }
        let mv = &moves[&move_id];
        if let Some(c) = f.company.as_deref().filter(|c| !c.is_empty()) {
            if text(mv, "company") != Some(c) {
                continue;
            }
        }
        if let Some(w) = f.warehouse.as_deref().filter(|w| !w.is_empty()) {
            if text(&line, "warehouse") != Some(w) && text(&line, "warehouse_from") != Some(w) {
                continue;
            }
        }
        rows.push(json!({
            "date": text(mv, "posting_date"),
            "number": text(mv, "number"),
            "kind": text(mv, "kind"),
            "state": text(mv, "state"),
            "warehouse": text(&line, "warehouse"),
            "warehouse_from": text(&line, "warehouse_from"),
            "lot": text(&line, "lot"),
            "qty": line.get("qty"),
            "unit_cost": line.get("unit_cost"),
            "value": line.get("value"),
            "source_kind": text(mv, "source_kind"),
            "source_id": text(mv, "source_id"),
        }));
    }
    rows.sort_by(|a, b| b["date"].as_str().cmp(&a["date"].as_str()).then(b["number"].as_str().cmp(&a["number"].as_str())));
    let balance = do_stock_balance(Filter { item: Some(item), company: f.company, warehouse: f.warehouse, ..Default::default() })?;
    Ok(json!({ "rows": rows, "balance": balance }))
}

fn do_dashboard(f: Filter) -> Result<Value> {
    require_reader()?;
    let balance = do_stock_balance(Filter { company: f.company.clone(), ..Default::default() })?;
    let rows = balance["rows"].as_array().cloned().unwrap_or_default();
    let mut items = std::collections::BTreeSet::new();
    let mut by_wh: BTreeMap<String, Decimal> = BTreeMap::new();
    for r in &rows {
        items.insert(r["item"].as_str().unwrap_or_default().to_string());
        let wh = r["warehouse"].as_str().unwrap_or_default().to_string();
        let v: Decimal = serde_json::from_value(r["value"].clone())?;
        let e = by_wh.entry(wh).or_insert_with(|| Decimal::zero(VALUE_SCALE));
        *e = e.checked_add(v)?;
    }
    let mut by_warehouse = Vec::new();
    for (wh, v) in by_wh {
        let name = require("stock_warehouse", &wh, "warehouse")
            .ok()
            .and_then(|w| text(&w, "name").map(str::to_string))
            .unwrap_or(wh);
        by_warehouse.push(json!({ "name": name, "value": v }));
    }
    let today_s = today()?;
    let horizon = (parse_date(&today_s)? + Duration::days(30)).to_string();
    let lots: Vec<Record> = db::find("stock_lot").limit(2000).all()?;
    let expiring = lots
        .iter()
        .filter(|l| text(l, "expiry_date").map_or(false, |e| e <= horizon.as_str()))
        .count();
    let draft_moves = db::find::<Record>("stock_move").filter("state", "draft").limit(1000).all()?.len();
    Ok(json!({
        "total_value": balance["total_value"],
        "items_in_stock": items.len(),
        "lots_expiring_30d": expiring,
        "draft_moves": draft_moves,
        "by_warehouse": by_warehouse,
    }))
}

fn matches_q(r: &Record, q: &Option<String>, fields: &[&str]) -> bool {
    match q.as_deref().map(str::to_lowercase).filter(|q| !q.is_empty()) {
        None => true,
        Some(q) => fields.iter().any(|f| text(r, f).map_or(false, |v| v.to_lowercase().contains(&q))),
    }
}

fn do_list_moves(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("stock_move").limit(500);
    if let Some(s) = f.state.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("state", s);
    }
    if let Some(k) = f.kind.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("kind", k);
    }
    let mut rows = q.all()?;
    rows.retain(|r| matches_q(r, &f.q, &["number", "source_id"]));
    rows.sort_by(|a, b| text(b, "posting_date").cmp(&text(a, "posting_date")));
    Ok(rows)
}

fn do_list_lots(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find("stock_lot").limit(1000).all()?;
    rows.retain(|r| matches_q(r, &f.q, &["code"]));
    rows.sort_by(|a, b| text(a, "expiry_date").cmp(&text(b, "expiry_date")));
    Ok(rows)
}

fn do_list_counts(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find("stock_count").limit(500).all()?;
    rows.retain(|r| f.state.as_deref().map_or(true, |s| s.is_empty() || text(r, "state") == Some(s)));
    rows.sort_by(|a, b| text(b, "count_date").cmp(&text(a, "count_date")));
    Ok(rows)
}

fn do_list_all_warehouses(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find("stock_warehouse").limit(500).all()?;
    rows.retain(|r| matches_q(r, &f.q, &["code", "name"]));
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}


/// Balance rows only, for a list screen.
fn do_list_stock(f: Filter) -> Result<Vec<Value>> {
    let balance = do_stock_balance(f)?;
    Ok(balance["rows"].as_array().cloned().unwrap_or_default())
}

#[derive(Deserialize)]
struct ParentId {
    id: String,
}

fn do_count_lines(input: ParentId) -> Result<Vec<Record>> {
    require_reader()?;
    db::find::<Record>("stock_count_line").filter("count", input.id.as_str()).limit(2000).all()
}

fn do_landed_charges(input: ParentId) -> Result<Vec<Record>> {
    require_reader()?;
    db::find::<Record>("stock_landed_charge").filter("landed", input.id.as_str()).limit(500).all()
}

fn do_list_landed(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("stock_landed_cost").limit(500).all()?;
    rows.retain(|r| f.state.as_deref().map_or(true, |s| s.is_empty() || text(r, "state") == Some(s)));
    Ok(rows)
}

handler! {
    fn list_stock(f: Filter) -> Vec<Value> { do_list_stock(f) }
    fn list_count_lines(input: ParentId) -> Vec<Record> { do_count_lines(input) }
    fn list_landed_charges(input: ParentId) -> Vec<Record> { do_landed_charges(input) }
    fn list_landed_costs(f: Filter) -> Vec<Record> { do_list_landed(f) }
    fn stock_balance(f: Filter) -> Value { do_stock_balance(f) }
    fn stock_ageing(f: Filter) -> Value { do_stock_ageing(f) }
    fn stock_ledger(f: Filter) -> Value { do_stock_ledger(f) }
    fn inventory_dashboard(f: Filter) -> Value { do_dashboard(f) }
    fn list_moves(f: Filter) -> Vec<Record> { do_list_moves(f) }
    fn list_lots(f: Filter) -> Vec<Record> { do_list_lots(f) }
    fn list_counts(f: Filter) -> Vec<Record> { do_list_counts(f) }
    fn search_warehouses(f: Filter) -> Vec<Record> { do_list_all_warehouses(f) }
}
