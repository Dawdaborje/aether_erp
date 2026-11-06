//! The purchasing overview board: what was ordered and billed, what needs a decision, and what is waiting.
//! One currency at a time (the most used, or the one picked): amounts in different currencies are never added.

use std::collections::BTreeMap;

use aether_sdk::dates::{Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::board_kit::*;
use crate::common::{money_of, qty_of, require, require_reader, text, today_date, Record};

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

fn name_of(id: &str) -> String {
    require("party.party", id, "party")
        .ok()
        .and_then(|r| text(&r, "display_name").or_else(|| text(&r, "name")).map(str::to_string))
        .unwrap_or_else(|| key(id).to_string())
}

/// Orders that count as spend: past approval, not cancelled.
fn counted(po: &Record) -> bool {
    !matches!(text(po, "state"), Some("draft" | "cancelled" | "to_approve") | None)
}

struct Data {
    orders: Vec<Record>,
    bills: Vec<Record>,
    currency: String,
    currency_filter: Value,
}

fn load(filters: Option<&Value>) -> Result<Data> {
    let orders: Vec<Record> = db::find("po").limit(5000).all()?;
    let bills: Vec<Record> = db::find("purchase_bill").limit(5000).all()?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in orders.iter().chain(&bills) {
        *counts.entry(text(r, "currency").unwrap_or_default().to_string()).or_default() += 1;
    }
    let (currency, currency_filter) = currency_filter(&counts, filters.and_then(|f| f.get("currency")));
    let same = |r: &Record| text(r, "currency").unwrap_or_default() == currency;
    Ok(Data { orders: orders.into_iter().filter(same).collect(), bills: bills.into_iter().filter(same).collect(), currency, currency_filter })
}

fn ordered(d: &Data, from: NaiveDate, to: NaiveDate) -> Vec<&Record> {
    d.orders.iter().filter(|o| counted(o) && in_range(text(o, "order_date"), from, to)).collect()
}

/// Net billed: approved bills less debit notes dated inside the period.
fn billed(d: &Data, from: NaiveDate, to: NaiveDate) -> Result<f64> {
    let mut total = 0.0;
    for b in d.bills.iter().filter(|b| text(b, "state") == Some("approved") && in_range(text(b, "bill_date"), from, to)) {
        let amount = num(money_of(b, "total")?);
        total += if text(b, "kind") == Some("debit_note") { -amount } else { amount };
    }
    Ok(total)
}

fn sum_total(orders: &[&Record]) -> Result<f64> {
    let mut total = 0.0;
    for o in orders {
        total += num(money_of(o, "total")?);
    }
    Ok(total)
}

/// Goods received but not yet billed, at the order's net price (before tax).
fn received_not_billed(d: &Data) -> Result<f64> {
    let open: Vec<&str> = d
        .orders
        .iter()
        .filter(|o| matches!(text(o, "state"), Some("partially_received" | "received" | "confirmed")))
        .filter_map(|o| text(o, "id"))
        .collect();
    let mut total = 0.0;
    for chunk in open.chunks(200) {
        let lines: Vec<Record> = db::find("po_line").matching(Filter::one_of("po", chunk.iter().copied())).limit(5000).all()?;
        for line in lines {
            let left = num(qty_of(&line, "qty_received")?) - num(qty_of(&line, "qty_billed")?);
            if left > 0.0 {
                total += left * num(qty_of(&line, "net_unit_price")?);
            }
        }
    }
    Ok(total)
}

fn board(input: BoardInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let (pfrom, pto) = prior(from, to);
    let d = load(input.filters.as_ref())?;
    let cur = Some(d.currency.as_str());

    let now_orders = ordered(&d, from, to);
    let prev_orders = ordered(&d, pfrom, pto);
    let (ordered_now, ordered_prev) = (sum_total(&now_orders)?, sum_total(&prev_orders)?);
    let (billed_now, billed_prev) = (billed(&d, from, to)?, billed(&d, pfrom, pto)?);
    let average = |total: f64, n: usize| if n == 0 { 0.0 } else { total / n as f64 };

    // Spend going up is neither good nor bad news by itself.
    let money = number_card(
        "money", "Spend", vec![
            reading("ordered", "Ordered", ordered_now, Some(ordered_prev), None),
            reading("billed", "Billed (net of debit notes)", billed_now, Some(billed_prev), None),
            reading("average", "Average order", average(ordered_now, now_orders.len()), Some(average(ordered_prev, prev_orders.len())), None),
        ], "currency", cur, 12,
        "Ordered adds confirmed purchase orders dated in the period. Billed adds approved bills and takes off debit notes. Compared with the period of the same length just before.",
    );

    let open_orders: Vec<&Record> = d.orders.iter().filter(|o| matches!(text(o, "state"), Some("confirmed" | "partially_received"))).collect();
    let open_card = number_card(
        "open", "Open today", vec![
            reading("open_orders", "Open orders", sum_total(&open_orders)?, None, None),
            reading("grni", "Received, not yet billed", received_not_billed(&d)?, None, None),
        ], "currency", cur, 12, "A snapshot of today. The period does not change it. Received, not yet billed is at the order's price before tax.",
    );

    // Things somebody has to decide or fix.
    let awaiting: Vec<&Record> = d.orders.iter().filter(|o| text(o, "state") == Some("to_approve")).collect();
    let exceptions: Vec<&Record> = d.bills.iter().filter(|b| text(b, "state") == Some("draft") && text(b, "release_to_pay") == Some("exception")).collect();
    let draft_bills = d.bills.iter().filter(|b| text(b, "state") == Some("draft") && text(b, "kind") != Some("debit_note")).count();
    let requests: Vec<Record> = db::find::<Record>("material_request").filter("state", "submitted").limit(2000).all()?;
    let attention = number_card(
        "attention", "Needs attention", vec![
            reading("approve", "Orders awaiting approval", awaiting.len() as f64, None, None),
            reading("requests", "Requests awaiting approval", requests.len() as f64, None, None),
            reading("exceptions", "Bills in exception", exceptions.len() as f64, None, None),
            reading("drafts", "Draft bills", draft_bills as f64, None, None),
        ], "number", None, 12, "Orders over the approval limit, material requests submitted, and bills that failed the three-way match.",
    );

    // By month.
    let months = months_in(from, to);
    let mut by_month_ordered = vec![0.0; months.len()];
    let mut by_month_billed = vec![0.0; months.len()];
    for o in &now_orders {
        if let Some(i) = date_of(text(o, "order_date")).and_then(|dt| month_index(&months, dt)) {
            by_month_ordered[i] += num(money_of(o, "total")?);
        }
    }
    for b in d.bills.iter().filter(|b| text(b, "state") == Some("approved") && in_range(text(b, "bill_date"), from, to)) {
        if let Some(i) = date_of(text(b, "bill_date")).and_then(|dt| month_index(&months, dt)) {
            let amount = num(money_of(b, "total")?);
            by_month_billed[i] += if text(b, "kind") == Some("debit_note") { -amount } else { amount };
        }
    }
    let trend = series_card(
        "month", "bar", "Ordered and billed by month",
        months.iter().map(|(s, _)| month_label(*s)).collect(), months.iter().map(|(s, _)| json!(day(*s))).collect(),
        vec![("Ordered", by_month_ordered), ("Billed", by_month_billed)], "month", "compact", cur, 12,
    );

    // Top suppliers.
    let mut by_supplier: BTreeMap<String, f64> = BTreeMap::new();
    for o in &now_orders {
        *by_supplier.entry(text(o, "supplier").unwrap_or_default().to_string()).or_default() += num(money_of(o, "total")?);
    }
    let mut ranked: Vec<(String, f64)> = by_supplier.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(8);
    let suppliers = series_card(
        "suppliers", "row", "Top suppliers",
        ranked.iter().map(|(id, _)| name_of(id)).collect(), ranked.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Ordered", ranked.iter().map(|r| r.1).collect())], "supplier", "compact", cur, 6,
    );

    // Orders by state.
    let mut by_state: BTreeMap<String, f64> = BTreeMap::new();
    for o in &now_orders {
        *by_state.entry(text(o, "state").unwrap_or_default().to_string()).or_default() += 1.0;
    }
    let states = series_card(
        "state", "donut", "Orders by state",
        by_state.keys().map(|s| label_of(s)).collect(), by_state.keys().map(|s| json!(s)).collect(),
        vec![("Orders", by_state.values().copied().collect())], "state", "number", None, 6,
    );

    // Tables: what to decide, and what is waiting longest.
    let order_row = |o: &Record| {
        let age = date_of(text(o, "order_date")).map_or(0, |dt| (today - dt).num_days());
        json!({ "number": text(o, "number"), "supplier": name_of(text(o, "supplier").unwrap_or_default()), "state": label_of(text(o, "state").unwrap_or_default()),
            "date": text(o, "order_date"), "age": format!("{age} days"), "total": text(o, "total") })
    };
    let order_cols: &[(&str, &str)] = &[("number", "Order"), ("supplier", "Supplier"), ("state", "State"), ("date", "Ordered"), ("age", "Waiting"), ("total", "Total")];
    let mut sorted_awaiting = awaiting.clone();
    sorted_awaiting.sort_by_key(|o| text(o, "order_date").unwrap_or_default().to_string());
    let mut sorted_open = open_orders.clone();
    sorted_open.sort_by_key(|o| text(o, "order_date").unwrap_or_default().to_string());
    let mut items = vec![money, open_card, attention, trend, suppliers, states];
    if !awaiting.is_empty() {
        items.push(table_card("awaiting", &format!("Orders awaiting approval ({})", awaiting.len()), order_cols, sorted_awaiting.iter().take(ROWS).map(|o| order_row(o)).collect(), 12));
    }
    if !exceptions.is_empty() {
        let rows: Vec<Value> = exceptions
            .iter()
            .take(ROWS)
            .map(|b| json!({ "number": text(b, "number"), "supplier": name_of(text(b, "supplier").unwrap_or_default()), "date": text(b, "bill_date"), "total": text(b, "total") }))
            .collect();
        items.push(table_card("exceptions", &format!("Bills that failed the match ({})", exceptions.len()),
            &[("number", "Bill"), ("supplier", "Supplier"), ("date", "Date"), ("total", "Total")], rows, 12));
    }
    if !open_orders.is_empty() {
        items.push(table_card("waiting", &format!("Open orders waiting longest ({})", open_orders.len()), order_cols, sorted_open.iter().take(ROWS).map(|o| order_row(o)).collect(), 12));
    }
    if d.orders.is_empty() {
        items.push(text_card("empty", "No purchasing yet", "Orders and bills appear here as soon as you create them."));
    }
    Ok(json!({
        "title": "Purchasing overview", "description": format!("{} to {} · {}", day(from), day(to), d.currency), "refresh_secs": 300,
        "filters": [ { "name": "period", "label": "Period", "kind": "period", "value": input.filters.as_ref().and_then(|f| f.get("period")).cloned().unwrap_or(json!({ "from": day(from), "to": day(to) })) }, d.currency_filter ],
        "items": items,
    }))
}

/// The orders behind a supplier bar, a state slice or a month.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let d = load(input.filters.as_ref())?;
    let segment = input.segment.unwrap_or(Value::Null);
    let wanted = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);
    let mut rows: Vec<&Record> = ordered(&d, from, to);
    match input.chart.as_deref() {
        Some("suppliers") => {
            if let Some(s) = wanted("supplier") {
                rows.retain(|o| text(o, "supplier") == Some(s.as_str()));
            }
        }
        Some("state") => {
            if let Some(s) = wanted("state") {
                rows.retain(|o| text(o, "state") == Some(s.as_str()));
            }
        }
        Some("month") => {
            if let Some(start) = wanted("month").and_then(|m| date_of(Some(m.as_str()))) {
                let end = aether_sdk::dates::add_months(start, 1) - Duration::days(1);
                rows.retain(|o| in_range(text(o, "order_date"), start, end));
            }
        }
        _ => {}
    }
    rows.sort_by(|a, b| text(b, "order_date").cmp(&text(a, "order_date")));
    let total = rows.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let page: Vec<Value> = rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|o| json!({ "id": text(o, "id"), "cells": {
            "number": text(o, "number"), "supplier": name_of(text(o, "supplier").unwrap_or_default()),
            "state": label_of(text(o, "state").unwrap_or_default()), "date": text(o, "order_date"), "total": text(o, "total"),
        }}))
        .collect();
    Ok(json!({
        "columns": [ {"key":"number","label":"Order"}, {"key":"supplier","label":"Supplier"}, {"key":"state","label":"State"}, {"key":"date","label":"Date"}, {"key":"total","label":"Total"} ],
        "rows": page, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The purchasing overview board for a period and currency.
    fn purchasing_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The orders behind one bar, slice or month of the purchasing overview.
    fn purchasing_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
