//! The replenishment board: which reorder rules are below their minimum right now (what "Check reorder rules"
//! would ask for), what is already requested, and how old the open requests are. Nothing is created here.

use std::collections::BTreeMap;

use aether_sdk::dates::parse_date;
use aether_sdk::prelude::*;

use crate::board_kit::*;
use crate::common::{qty_of, require_reader, text, today, Record};
use crate::replenish::{available_at, incoming};
use crate::rules::reorder_qty;

#[derive(Deserialize, Default)]
struct BoardInput {
    #[serde(default)]
    filters: Option<Value>,
}

#[derive(Deserialize, Default)]
struct DrillInput {
    #[serde(default)]
    chart: Option<String>,
    #[serde(default)]
    segment: Option<Value>,
    #[serde(default)]
    filters: Option<Value>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

const ROWS: usize = 15;
/// Rules checked at once; each needs two stock lookups.
const RULE_LIMIT: usize = 200;

struct Data {
    company: String,
    company_filter: Value,
    rules: Vec<Record>,
    requests: Vec<Record>,
    items: BTreeMap<String, String>,
    warehouses: BTreeMap<String, String>,
}

fn load(filters: Option<&Value>) -> Result<Data> {
    let rules: Vec<Record> = db::find::<Record>("reorder_rule").filter("active", true).limit(2000).all()?;
    let requests: Vec<Record> = db::find::<Record>("replenish_request").limit(5000).all()?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in rules.iter().chain(&requests) {
        *counts.entry(text(r, "company").unwrap_or_default().to_string()).or_default() += 1;
    }
    let chosen = filters
        .and_then(|f| f.get("company"))
        .and_then(|v| match v {
            Value::Array(list) => list.first().and_then(Value::as_str),
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
        .filter(|c| counts.contains_key(*c))
        .map(str::to_string);
    let company = chosen.or_else(|| counts.iter().max_by_key(|(_, n)| **n).map(|(c, _)| c.clone())).unwrap_or_default();
    let options: Vec<Value> = counts
        .keys()
        .map(|id| {
            let label = db::get::<Record>("company.company", id).ok().flatten().and_then(|c| text(&c, "name").map(str::to_string)).unwrap_or_else(|| key(id).to_string());
            json!({ "value": id, "label": label })
        })
        .collect();
    let items = db::find::<Record>("erp_item.item")
        .limit(20_000)
        .all()?
        .iter()
        .filter_map(|i| Some((key(text(i, "id")?).to_string(), text(i, "name").or_else(|| text(i, "code"))?.to_string())))
        .collect();
    let warehouses = db::find::<Record>("erp_inventory.stock_warehouse")
        .limit(2000)
        .all()?
        .iter()
        .filter_map(|w| Some((key(text(w, "id")?).to_string(), text(w, "name")?.to_string())))
        .collect();
    let mine = |r: &Record| text(r, "company") == Some(company.as_str());
    Ok(Data {
        rules: rules.iter().filter(|r| mine(r)).cloned().collect(),
        requests: requests.iter().filter(|r| mine(r)).cloned().collect(),
        company_filter: json!({ "name": "company", "label": "Company", "kind": "select", "value": [company.clone()], "options": options }),
        company, items, warehouses,
    })
}

fn item_name(d: &Data, id: &str) -> String {
    d.items.get(key(id)).cloned().unwrap_or_else(|| key(id).to_string())
}

fn warehouse_name(d: &Data, id: &str) -> String {
    d.warehouses.get(key(id)).cloned().unwrap_or_else(|| key(id).to_string())
}

fn request_row(d: &Data, r: &Record) -> Value {
    json!({ "item": item_name(d, text(r, "item").unwrap_or_default()), "warehouse": warehouse_name(d, text(r, "warehouse").unwrap_or_default()),
        "qty": text(r, "qty"), "route": label_of(text(r, "route").unwrap_or_default()), "reason": label_of(text(r, "reason").unwrap_or_default()),
        "state": label_of(text(r, "state").unwrap_or_default()), "needed": text(r, "needed_by") })
}

fn board(input: BoardInput) -> Result<Value> {
    require_reader()?;
    let today = parse_date(&today()?)?;
    let d = load(input.filters.as_ref())?;
    let open: Vec<&Record> = d.requests.iter().filter(|r| text(r, "state") == Some("open")).collect();
    let has_open: std::collections::BTreeSet<&str> = open.iter().filter_map(|r| text(r, "rule")).collect();

    // Dry run of the reorder check: the rules that would raise a request if it ran now.
    let mut below: Vec<Value> = Vec::new();
    let mut below_count = 0usize;
    let mut covered = 0usize;
    let mut notes: Vec<String> = Vec::new();
    for rule in d.rules.iter().take(RULE_LIMIT) {
        let (item, wh) = (text(rule, "item").unwrap_or_default(), text(rule, "warehouse").unwrap_or_default());
        let looked = (|| -> Result<Option<(String, String, String, String)>> {
            let available = available_at(&d.company, item, wh)?;
            let coming = incoming(&d.company, item, wh)?;
            let qty = reorder_qty(available, coming, qty_of(rule, "min_qty")?, qty_of(rule, "max_qty")?, qty_of(rule, "multiple")?)?;
            Ok(qty.map(|q| (format!("{available}"), format!("{coming}"), format!("{}", qty_of(rule, "min_qty").map(|m| format!("{m}")).unwrap_or_default()), format!("{q}"))))
        })();
        match looked {
            Ok(Some((available, coming, min, qty))) => {
                below_count += 1;
                let requested = text(rule, "id").is_some_and(|id| has_open.contains(id));
                if requested {
                    covered += 1;
                }
                below.push(json!({ "item": item_name(&d, item), "warehouse": warehouse_name(&d, wh), "available": available, "incoming": coming,
                    "min": min, "suggest": qty, "route": label_of(text(rule, "route").unwrap_or("buy")), "requested": if requested { "Already requested" } else { "Not yet" } }));
            }
            Ok(None) => {}
            Err(e) => {
                if notes.len() < 3 {
                    notes.push(format!("{}: {}", item_name(&d, item), e));
                }
            }
        }
    }

    let oldest_days = |r: &Record| text(r, "needed_by").and_then(|n| date_of(Some(n))).map_or(0, |n| (today - n).num_days());
    let overdue = open.iter().filter(|r| oldest_days(r) > 0).count();
    let numbers = number_card(
        "now", "Today", vec![
            reading("rules", "Active reorder rules", d.rules.len() as f64, None, None),
            reading("below", "Rules below their minimum", below_count as f64, None, None),
            reading("not_requested", "Below minimum, nothing requested yet", (below_count - covered) as f64, None, None),
            reading("open", "Open requests", open.len() as f64, None, None),
            reading("overdue", "Open requests past their needed-by date", overdue as f64, None, None),
        ], "number", None, 12,
        "Below minimum counts stock available now plus what is already on order or being made, the same as the reorder check. Nothing is created here.",
    );

    let count_by = |field: &str| {
        let mut m: BTreeMap<String, f64> = BTreeMap::new();
        for r in &open {
            *m.entry(text(r, field).unwrap_or_default().to_string()).or_default() += 1.0;
        }
        m
    };
    let (by_route, by_reason) = (count_by("route"), count_by("reason"));
    let route_card = series_card(
        "route", "donut", "Open requests by route",
        by_route.keys().map(|s| label_of(s)).collect(), by_route.keys().map(|s| json!(s)).collect(),
        vec![("Requests", by_route.values().copied().collect())], "route", "number", None, 6,
    );
    let reason_card = series_card(
        "reason", "donut", "Open requests by reason",
        by_reason.keys().map(|s| label_of(s)).collect(), by_reason.keys().map(|s| json!(s)).collect(),
        vec![("Requests", by_reason.values().copied().collect())], "reason", "number", None, 6,
    );

    let mut items = vec![numbers];
    if !open.is_empty() {
        items.push(route_card);
        items.push(reason_card);
    }
    if !below.is_empty() {
        let shown: Vec<Value> = below.iter().take(ROWS).cloned().collect();
        items.push(table_card("below", &format!("Below the reorder point ({})", below_count),
            &[("item", "Item"), ("warehouse", "Warehouse"), ("available", "Available"), ("incoming", "On the way"), ("min", "Minimum"), ("suggest", "Would order"), ("route", "Route"), ("requested", "Request")], shown, 12));
    }
    let mut oldest: Vec<&Record> = open.clone();
    oldest.sort_by_key(|r| text(r, "needed_by").unwrap_or("9999-12-31").to_string());
    if !oldest.is_empty() {
        items.push(table_card("oldest", "Open requests, most urgent first",
            &[("item", "Item"), ("warehouse", "Warehouse"), ("qty", "Qty"), ("route", "Route"), ("reason", "Reason"), ("needed", "Needed by")],
            oldest.iter().take(ROWS).map(|r| request_row(&d, r)).collect(), 12));
    }
    if !notes.is_empty() {
        items.push(text_card("notes", "Some rules could not be checked", &notes.join("; ")));
    }
    if d.rules.len() > RULE_LIMIT {
        items.push(text_card("limit", "Showing part of the rules", &format!("Only the first {RULE_LIMIT} of {} rules are checked here.", d.rules.len())));
    }
    if d.rules.is_empty() && d.requests.is_empty() {
        items.push(text_card("empty", "No reorder rules yet", "Add a reorder rule to an item and warehouse to see shortfalls here."));
    }
    Ok(json!({
        "title": "Replenishment overview", "description": "Stock against reorder rules, today", "refresh_secs": 300,
        "filters": [ d.company_filter ],
        "items": items,
    }))
}

/// The open requests behind a route or reason slice.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let d = load(input.filters.as_ref())?;
    let segment = input.segment.unwrap_or(Value::Null);
    let wanted = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);
    let mut rows: Vec<&Record> = d.requests.iter().filter(|r| text(r, "state") == Some("open")).collect();
    match input.chart.as_deref() {
        Some("route") => {
            if let Some(v) = wanted("route") {
                rows.retain(|r| text(r, "route") == Some(v.as_str()));
            }
        }
        Some("reason") => {
            if let Some(v) = wanted("reason") {
                rows.retain(|r| text(r, "reason") == Some(v.as_str()));
            }
        }
        _ => {}
    }
    rows.sort_by_key(|r| text(r, "needed_by").unwrap_or("9999-12-31").to_string());
    let total = rows.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let page: Vec<Value> = rows.iter().skip(offset).take(limit).map(|r| json!({ "id": text(r, "id"), "cells": request_row(&d, r) })).collect();
    Ok(json!({
        "columns": [ {"key":"item","label":"Item"}, {"key":"warehouse","label":"Warehouse"}, {"key":"qty","label":"Qty"}, {"key":"route","label":"Route"}, {"key":"reason","label":"Reason"}, {"key":"needed","label":"Needed by"} ],
        "rows": page, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The replenishment board for a company.
    fn replenishment_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The open requests behind one slice of the replenishment board.
    fn replenishment_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
