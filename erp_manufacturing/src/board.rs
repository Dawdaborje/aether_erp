//! The manufacturing overview board: what is being made, what is late, how the routing is doing, and where the
//! work centres are loaded. Orders are placed in time by their planned date (an order has no other date).

use std::collections::BTreeMap;

use aether_sdk::dates::{parse_date, Duration, NaiveDate};
use aether_sdk::prelude::*;

use crate::board_kit::*;
use crate::common::{company_currency_code, dec, qty_of, require_reader, text, today, Record};

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

const ROWS: usize = 10;
const OPEN: [&str; 3] = ["confirmed", "reserved", "in_progress"];

fn is_open(mo: &Record) -> bool {
    text(mo, "state").is_some_and(|s| OPEN.contains(&s))
}

struct Data {
    orders: Vec<Record>,
    ops: Vec<Record>,
    items: BTreeMap<String, String>,
    centres: BTreeMap<String, String>,
    currency: String,
    company_filter: Value,
}

fn load(filters: Option<&Value>) -> Result<Data> {
    let all: Vec<Record> = db::find("mo").limit(5000).all()?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for o in &all {
        *counts.entry(text(o, "company").unwrap_or_default().to_string()).or_default() += 1;
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
    let orders: Vec<Record> = all.into_iter().filter(|o| text(o, "company") == Some(company.as_str())).collect();
    let ids: std::collections::BTreeSet<&str> = orders.iter().filter_map(|o| text(o, "id")).collect();
    let ops: Vec<Record> = db::find::<Record>("mo_operation").limit(20_000).all()?.into_iter().filter(|o| text(o, "mo").is_some_and(|m| ids.contains(m))).collect();
    let items = db::find::<Record>("erp_item.item")
        .limit(20_000)
        .all()?
        .iter()
        .filter_map(|i| Some((key(text(i, "id")?).to_string(), text(i, "name").or_else(|| text(i, "code"))?.to_string())))
        .collect();
    let centres = db::find::<Record>("work_center")
        .limit(2000)
        .all()?
        .iter()
        .filter_map(|w| Some((key(text(w, "id")?).to_string(), text(w, "name")?.to_string())))
        .collect();
    let currency = if company.is_empty() { String::new() } else { company_currency_code(&company).unwrap_or_default() };
    Ok(Data {
        orders, ops, items, centres, currency,
        company_filter: json!({ "name": "company", "label": "Company", "kind": "select", "value": [company], "options": options }),
    })
}

fn item_name(d: &Data, id: &str) -> String {
    d.items.get(key(id)).cloned().unwrap_or_else(|| key(id).to_string())
}

fn centre_name(d: &Data, id: &str) -> String {
    d.centres.get(key(id)).cloned().unwrap_or_else(|| if id.is_empty() { "Not set".to_string() } else { key(id).to_string() })
}

fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

/// Orders planned inside the period, whatever their state, less the cancelled.
fn planned<'a>(d: &'a Data, from: NaiveDate, to: NaiveDate) -> Vec<&'a Record> {
    d.orders.iter().filter(|o| text(o, "state") != Some("cancelled") && in_range(text(o, "planned_date"), from, to)).collect()
}

fn units(orders: &[&Record]) -> Result<f64> {
    let mut total = 0.0;
    for o in orders {
        total += num(qty_of(o, "qty")?);
    }
    Ok(total)
}

fn labour(orders: &[&Record]) -> Result<f64> {
    let mut total = 0.0;
    for o in orders.iter().filter(|o| text(o, "state") == Some("done")) {
        total += num(dec(o, "labour_cost", 6)?);
    }
    Ok(total)
}

/// Planned against logged minutes of the finished operations of finished orders.
fn efficiency(d: &Data, orders: &[&Record]) -> Result<Option<f64>> {
    let done: std::collections::BTreeSet<&str> = orders.iter().filter(|o| text(o, "state") == Some("done")).filter_map(|o| text(o, "id")).collect();
    let (mut plan, mut actual) = (0.0, 0.0);
    for op in d.ops.iter().filter(|o| text(o, "state") == Some("done") && text(o, "mo").is_some_and(|m| done.contains(m))) {
        plan += num(dec(op, "planned_minutes", 2)?);
        actual += num(dec(op, "actual_minutes", 2)?);
    }
    Ok(if actual <= 0.0 { None } else { Some((plan / actual * 1000.0).round() / 10.0) })
}

fn order_row(d: &Data, o: &Record) -> Value {
    json!({ "number": text(o, "number").unwrap_or("Draft"), "item": item_name(d, text(o, "item").unwrap_or_default()), "qty": text(o, "qty"),
        "state": label_of(text(o, "state").unwrap_or_default()), "planned": text(o, "planned_date") })
}

fn board(input: BoardInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let (pfrom, pto) = prior(from, to);
    let d = load(input.filters.as_ref())?;
    let cur = Some(d.currency.as_str()).filter(|c| !c.is_empty());

    let now_orders = planned(&d, from, to);
    let prev_orders = planned(&d, pfrom, pto);
    let done_now: Vec<&Record> = now_orders.iter().copied().filter(|o| text(o, "state") == Some("done")).collect();
    let done_prev: Vec<&Record> = prev_orders.iter().copied().filter(|o| text(o, "state") == Some("done")).collect();

    let production = number_card(
        "production", "Production planned in the period", vec![
            reading("planned", "Orders planned", now_orders.len() as f64, Some(prev_orders.len() as f64), Some(true)),
            reading("done", "Completed", done_now.len() as f64, Some(done_prev.len() as f64), Some(true)),
            reading("units", "Units completed", units(&done_now)?, Some(units(&done_prev)?), Some(true)),
        ], "number", None, 12,
        "Orders are placed in time by their planned date. Compared with the period of the same length just before.",
    );
    let mut cost_readings = vec![reading("labour", "Labour cost of completed orders", labour(&done_now)?, Some(labour(&done_prev)?), None)];
    // Only when time was logged: no logged time is not 0% efficient.
    if let Some(now) = efficiency(&d, &now_orders)? {
        cost_readings.push(reading("efficiency", "Routing efficiency % (planned / logged minutes)", now, efficiency(&d, &prev_orders)?, Some(true)));
    }
    let cost = number_card(
        "cost", "Cost and efficiency", cost_readings, "number", cur, 12,
        "Efficiency compares planned with logged minutes on finished operations of completed orders; above 100 means faster than planned.",
    );

    // Where things stand today.
    let open: Vec<&Record> = d.orders.iter().filter(|o| is_open(o)).collect();
    let late: Vec<&Record> = open.iter().copied().filter(|o| date_of(text(o, "planned_date")).is_some_and(|p| p < today)).collect();
    let running = d.ops.iter().filter(|o| text(o, "state") == Some("in_progress")).count();
    let now_card = number_card(
        "now", "Open today", vec![
            reading("open", "Open orders", open.len() as f64, None, None),
            reading("units_open", "Units in progress", units(&open)?, None, None),
            reading("late", "Late orders", late.len() as f64, None, None),
            reading("drafts", "Drafts", d.orders.iter().filter(|o| text(o, "state") == Some("draft")).count() as f64, None, None),
            reading("running", "Operations running", running as f64, None, None),
        ], "number", None, 12, "A snapshot of today. The period does not change it.",
    );

    // Units by planned month: finished against still to do.
    let months = months_in(from, to);
    let (mut finished, mut todo) = (vec![0.0; months.len()], vec![0.0; months.len()]);
    for o in &now_orders {
        let Some(i) = date_of(text(o, "planned_date")).and_then(|dt| month_index(&months, dt)) else { continue };
        let q = num(qty_of(o, "qty")?);
        if text(o, "state") == Some("done") { finished[i] += q } else { todo[i] += q }
    }
    let trend = series_card(
        "month", "bar", "Units by planned month",
        months.iter().map(|(s, _)| month_label(*s)).collect(), months.iter().map(|(s, _)| json!(day(*s))).collect(),
        vec![("Completed", finished), ("Still to make", todo)], "month", "number", None, 12,
    );

    let mut by_state: BTreeMap<String, f64> = BTreeMap::new();
    for o in &now_orders {
        *by_state.entry(text(o, "state").unwrap_or_default().to_string()).or_default() += 1.0;
    }
    let states = series_card(
        "state", "donut", "Orders by state",
        by_state.keys().map(|s| label_of(s)).collect(), by_state.keys().map(|s| json!(s)).collect(),
        vec![("Orders", by_state.values().copied().collect())], "state", "number", None, 6,
    );

    // Work still to do, by work centre: minutes of operations not yet finished on open orders.
    let open_ids: std::collections::BTreeSet<&str> = open.iter().filter_map(|o| text(o, "id")).collect();
    let mut load_by: BTreeMap<String, f64> = BTreeMap::new();
    for op in d.ops.iter().filter(|o| text(o, "state") != Some("done") && text(o, "mo").is_some_and(|m| open_ids.contains(m))) {
        *load_by.entry(text(op, "work_center").unwrap_or_default().to_string()).or_default() += num(dec(op, "planned_minutes", 2)?);
    }
    let mut ranked: Vec<(String, f64)> = load_by.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(8);
    let centres = series_card(
        "centre", "row", "Work still to do by work centre (planned minutes)",
        ranked.iter().map(|(id, _)| centre_name(&d, id)).collect(), ranked.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Minutes", ranked.iter().map(|r| r.1).collect())], "centre", "number", None, 6,
    );

    let mut items = vec![production, cost, now_card, trend, states, centres];
    let mut late_sorted = late.clone();
    late_sorted.sort_by_key(|o| text(o, "planned_date").unwrap_or_default().to_string());
    if !late_sorted.is_empty() {
        let rows: Vec<Value> = late_sorted
            .iter()
            .take(ROWS)
            .map(|o| {
                let mut r = order_row(&d, o);
                let days = date_of(text(o, "planned_date")).map_or(0, |p| (today - p).num_days());
                r["late"] = json!(format!("{days} days"));
                r
            })
            .collect();
        items.push(table_card("late", &format!("Late orders ({})", late_sorted.len()),
            &[("number", "Order"), ("item", "Item"), ("qty", "Qty"), ("state", "State"), ("planned", "Planned"), ("late", "Late by")], rows, 12));
    }
    let names: BTreeMap<&str, &str> = d.orders.iter().filter_map(|o| Some((text(o, "id")?, text(o, "number")?))).collect();
    let running_rows: Vec<Value> = d
        .ops
        .iter()
        .filter(|o| text(o, "state") == Some("in_progress"))
        .take(ROWS)
        .map(|op| json!({ "order": names.get(text(op, "mo").unwrap_or_default()).copied().unwrap_or("?"), "name": text(op, "name"),
            "centre": centre_name(&d, text(op, "work_center").unwrap_or_default()), "started": text(op, "started_on"),
            "planned": text(op, "planned_minutes"), "actual": text(op, "actual_minutes") }))
        .collect();
    if !running_rows.is_empty() {
        items.push(table_card("running", "Operations running now",
            &[("order", "Order"), ("name", "Operation"), ("centre", "Work centre"), ("started", "Started"), ("planned", "Planned min"), ("actual", "Logged min")], running_rows, 12));
    }
    if d.orders.is_empty() {
        items.push(text_card("empty", "No manufacturing yet", "Orders appear here as soon as you plan production."));
    }
    let _ = Duration::days(0);
    Ok(json!({
        "title": "Manufacturing overview", "description": format!("{} to {}", day(from), day(to)), "refresh_secs": 300,
        "filters": [ { "name": "period", "label": "Period (planned date)", "kind": "period", "value": input.filters.as_ref().and_then(|f| f.get("period")).cloned().unwrap_or(json!({ "from": day(from), "to": day(to) })) }, d.company_filter ],
        "items": items,
    }))
}

/// The orders behind a month or a state, or the open operations behind a work centre.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let d = load(input.filters.as_ref())?;
    let segment = input.segment.unwrap_or(Value::Null);
    let wanted = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));

    if input.chart.as_deref() == Some("centre") {
        let open_ids: std::collections::BTreeSet<&str> = d.orders.iter().filter(|o| is_open(o)).filter_map(|o| text(o, "id")).collect();
        let names: BTreeMap<&str, &str> = d.orders.iter().filter_map(|o| Some((text(o, "id")?, text(o, "number")?))).collect();
        let mut ops: Vec<&Record> = d.ops.iter().filter(|o| text(o, "state") != Some("done") && text(o, "mo").is_some_and(|m| open_ids.contains(m))).collect();
        if let Some(c) = wanted("centre") {
            ops.retain(|o| text(o, "work_center") == Some(c.as_str()));
        }
        let total = ops.len();
        let rows: Vec<Value> = ops
            .iter()
            .skip(offset)
            .take(limit)
            .map(|o| json!({ "id": text(o, "mo"), "cells": { "order": names.get(text(o, "mo").unwrap_or_default()).copied().unwrap_or("?"), "name": text(o, "name"),
                "state": label_of(text(o, "state").unwrap_or_default()), "planned": text(o, "planned_minutes"), "actual": text(o, "actual_minutes") }}))
            .collect();
        return Ok(json!({
            "columns": [ {"key":"order","label":"Order"}, {"key":"name","label":"Operation"}, {"key":"state","label":"State"}, {"key":"planned","label":"Planned min"}, {"key":"actual","label":"Logged min"} ],
            "rows": rows, "total": total, "limit": limit, "offset": offset,
        }));
    }

    let mut orders = planned(&d, from, to);
    match input.chart.as_deref() {
        Some("state") => {
            if let Some(s) = wanted("state") {
                orders.retain(|o| text(o, "state") == Some(s.as_str()));
            }
        }
        Some("month") => {
            if let Some(start) = wanted("month").and_then(|m| date_of(Some(m.as_str()))) {
                let end = aether_sdk::dates::add_months(start, 1) - Duration::days(1);
                orders.retain(|o| in_range(text(o, "planned_date"), start, end));
            }
        }
        _ => {}
    }
    orders.sort_by(|a, b| text(b, "planned_date").cmp(&text(a, "planned_date")));
    let total = orders.len();
    let rows: Vec<Value> = orders
        .iter()
        .skip(offset)
        .take(limit)
        .map(|o| json!({ "id": text(o, "id"), "cells": order_row(&d, o) }))
        .collect();
    Ok(json!({
        "columns": [ {"key":"number","label":"Order"}, {"key":"item","label":"Item"}, {"key":"qty","label":"Qty"}, {"key":"state","label":"State"}, {"key":"planned","label":"Planned"} ],
        "rows": rows, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The manufacturing overview board for a company and period.
    fn manufacturing_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The orders or operations behind one bar, slice or month of the manufacturing overview.
    fn manufacturing_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
