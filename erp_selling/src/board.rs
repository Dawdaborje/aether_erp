//! The sales overview board: what was ordered and invoiced, how quotations turn into cash, and what is stuck.
//! One currency at a time (the most used, or the one picked): amounts in different currencies are never added.

use std::collections::{BTreeMap, BTreeSet};

use aether_sdk::dates::Duration;
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

/// Orders that count as sales: confirmed or beyond, not cancelled.
fn counted(so: &Record) -> bool {
    !matches!(text(so, "state"), Some("draft" | "cancelled") | None)
}

struct Data {
    orders: Vec<Record>,
    quotes: Vec<Record>,
    invoices: Vec<Record>,
    currency: String,
    currency_filter: Value,
}

fn load(filters: Option<&Value>) -> Result<Data> {
    let orders: Vec<Record> = db::find("so").limit(5000).all()?;
    let quotes: Vec<Record> = db::find("quotation").limit(5000).all()?;
    let invoices: Vec<Record> = db::find::<Record>("sales_invoice").filter("state", "approved").limit(5000).all()?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in orders.iter().chain(&invoices) {
        *counts.entry(text(r, "currency").unwrap_or_default().to_string()).or_default() += 1;
    }
    let (currency, currency_filter) = currency_filter(&counts, filters.and_then(|f| f.get("currency")));
    let same = |r: &Record| text(r, "currency").unwrap_or_default() == currency;
    Ok(Data {
        orders: orders.into_iter().filter(same).collect(),
        quotes: quotes.into_iter().filter(same).collect(),
        invoices: invoices.into_iter().filter(same).collect(),
        currency,
        currency_filter,
    })
}

fn ordered(d: &Data, from: aether_sdk::dates::NaiveDate, to: aether_sdk::dates::NaiveDate) -> Vec<&Record> {
    d.orders.iter().filter(|o| counted(o) && in_range(text(o, "order_date"), from, to)).collect()
}

/// Net invoiced: invoices less credit notes dated inside the period.
fn invoiced(d: &Data, from: aether_sdk::dates::NaiveDate, to: aether_sdk::dates::NaiveDate) -> Result<f64> {
    let mut total = 0.0;
    for i in d.invoices.iter().filter(|i| in_range(text(i, "invoice_date"), from, to)) {
        let amount = num(money_of(i, "total")?);
        total += if text(i, "kind") == Some("credit_note") { -amount } else { amount };
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

/// Value of what has been sold but not yet invoiced, from the order lines.
fn uninvoiced_value(d: &Data) -> Result<f64> {
    let open: Vec<&str> = d
        .orders
        .iter()
        .filter(|o| matches!(text(o, "state"), Some("confirmed" | "partially_delivered" | "delivered")))
        .filter_map(|o| text(o, "id"))
        .collect();
    let mut total = 0.0;
    for chunk in open.chunks(200) {
        let lines: Vec<Record> = db::find("so_line").matching(Filter::one_of("so", chunk.iter().copied())).limit(5000).all()?;
        for line in lines {
            let qty = num(qty_of(&line, "qty")?);
            let left = qty - num(qty_of(&line, "qty_invoiced")?);
            if qty <= 0.0 || left <= 0.0 {
                continue;
            }
            total += (num(money_of(&line, "amount")?) + num(money_of(&line, "tax_amount")?)) * left / qty;
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
    let (invoiced_now, invoiced_prev) = (invoiced(&d, from, to)?, invoiced(&d, pfrom, pto)?);
    let average = |total: f64, n: usize| if n == 0 { 0.0 } else { total / n as f64 };

    let money = number_card(
        "money", "Sales", vec![
            reading("ordered", "Ordered", ordered_now, Some(ordered_prev), Some(true)),
            reading("invoiced", "Invoiced (net of credit notes)", invoiced_now, Some(invoiced_prev), Some(true)),
            reading("average", "Average order", average(ordered_now, now_orders.len()), Some(average(ordered_prev, prev_orders.len())), Some(true)),
        ], "currency", cur, 12,
        "Ordered adds confirmed orders dated in the period. Invoiced adds approved invoices and takes off credit notes. Compared with the period of the same length just before.",
    );

    // Quotations dated in the period, and how many of the decided ones became orders.
    let quotes_in = |a, b| d.quotes.iter().filter(|q| in_range(text(q, "quote_date"), a, b)).collect::<Vec<_>>();
    let win = |qs: &[&Record]| {
        let won = qs.iter().filter(|q| text(q, "state") == Some("converted")).count();
        let decided = qs.iter().filter(|q| matches!(text(q, "state"), Some("converted" | "rejected" | "expired"))).count();
        if decided == 0 { 0.0 } else { (won as f64 / decided as f64 * 1000.0).round() / 10.0 }
    };
    let (q_now, q_prev) = (quotes_in(from, to), quotes_in(pfrom, pto));
    let counts = number_card(
        "counts", "Volume", vec![
            reading("orders", "Orders", now_orders.len() as f64, Some(prev_orders.len() as f64), Some(true)),
            reading("quotes", "Quotations", q_now.len() as f64, Some(q_prev.len() as f64), Some(true)),
            reading("win", "Quotation win %", win(&q_now), Some(win(&q_prev)), Some(true)),
        ], "number", None, 12, "Win % is quotations that became orders out of those decided (converted, rejected or expired).",
    );

    // Where things stand today, whatever the period.
    let open_orders: Vec<&Record> = d.orders.iter().filter(|o| matches!(text(o, "state"), Some("confirmed" | "partially_delivered"))).collect();
    let open_quotes: Vec<&Record> = d.quotes.iter().filter(|q| matches!(text(q, "state"), Some("sent" | "accepted"))).collect();
    let now_card = number_card(
        "now", "Open today", vec![
            reading("open_orders", "Open orders", sum_total(&open_orders)?, None, None),
            reading("uninvoiced", "Not yet invoiced", uninvoiced_value(&d)?, None, None),
            reading("open_quotes", "Open quotations", sum_total(&open_quotes)?, None, None),
        ], "currency", cur, 12, "A snapshot of today. The period does not change it.",
    );

    // By month.
    let months = months_in(from, to);
    let mut by_month_ordered = vec![0.0; months.len()];
    let mut by_month_invoiced = vec![0.0; months.len()];
    for o in &now_orders {
        if let Some(i) = date_of(text(o, "order_date")).and_then(|dt| month_index(&months, dt)) {
            by_month_ordered[i] += num(money_of(o, "total")?);
        }
    }
    for inv in d.invoices.iter().filter(|i| in_range(text(i, "invoice_date"), from, to)) {
        if let Some(i) = date_of(text(inv, "invoice_date")).and_then(|dt| month_index(&months, dt)) {
            let amount = num(money_of(inv, "total")?);
            by_month_invoiced[i] += if text(inv, "kind") == Some("credit_note") { -amount } else { amount };
        }
    }
    let trend = series_card(
        "month", "bar", "Ordered and invoiced by month",
        months.iter().map(|(s, _)| month_label(*s)).collect(), months.iter().map(|(s, _)| json!(day(*s))).collect(),
        vec![("Ordered", by_month_ordered), ("Invoiced", by_month_invoiced)], "month", "compact", cur, 12,
    );

    // Top customers.
    let mut by_customer: BTreeMap<String, f64> = BTreeMap::new();
    for o in &now_orders {
        *by_customer.entry(text(o, "customer").unwrap_or_default().to_string()).or_default() += num(money_of(o, "total")?);
    }
    let mut ranked: Vec<(String, f64)> = by_customer.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(8);
    let customers = series_card(
        "customers", "row", "Top customers",
        ranked.iter().map(|(id, _)| name_of(id)).collect(), ranked.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Ordered", ranked.iter().map(|r| r.1).collect())], "customer", "compact", cur, 6,
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

    // Quotation to cash, for orders and quotations dated in the period.
    let invoiced_orders: BTreeSet<&str> = d.invoices.iter().filter(|i| text(i, "kind") != Some("credit_note")).filter_map(|i| text(i, "so")).collect();
    let delivered = now_orders.iter().filter(|o| matches!(text(o, "state"), Some("delivered" | "closed"))).count();
    let billed = now_orders.iter().filter(|o| text(o, "id").is_some_and(|id| invoiced_orders.contains(id))).count();
    let funnel = plain_series_card(
        "funnel", "funnel", "Quotation to cash",
        vec!["Quotations".into(), "Orders".into(), "Delivered".into(), "Invoiced".into()],
        vec![("Documents", vec![q_now.len() as f64, now_orders.len() as f64, delivered as f64, billed as f64])], "number", None, 12,
    );

    // Orders waiting longest, and quotations about to lapse.
    let mut waiting: Vec<&Record> = open_orders.clone();
    waiting.sort_by_key(|o| text(o, "order_date").unwrap_or_default().to_string());
    let waiting_rows: Vec<Value> = waiting
        .iter()
        .take(ROWS)
        .map(|o| {
            let age = date_of(text(o, "order_date")).map_or(0, |dt| (today - dt).num_days());
            json!({ "number": text(o, "number"), "customer": name_of(text(o, "customer").unwrap_or_default()), "state": label_of(text(o, "state").unwrap_or_default()),
                "date": text(o, "order_date"), "age": format!("{age} days"), "total": text(o, "total") })
        })
        .collect();
    let soon = today + Duration::days(14);
    let mut lapsing: Vec<&Record> = open_quotes.iter().copied().filter(|q| in_range(text(q, "valid_until"), today, soon)).collect();
    lapsing.sort_by_key(|q| text(q, "valid_until").unwrap_or_default().to_string());
    let lapsing_rows: Vec<Value> = lapsing
        .iter()
        .take(ROWS)
        .map(|q| json!({ "number": text(q, "number"), "customer": name_of(text(q, "customer").unwrap_or_default()), "valid_until": text(q, "valid_until"), "total": text(q, "total") }))
        .collect();

    let mut items = vec![money, counts, now_card, trend, customers, states, funnel];
    if !waiting_rows.is_empty() {
        items.push(table_card("waiting", &format!("Open orders waiting longest ({})", open_orders.len()),
            &[("number", "Order"), ("customer", "Customer"), ("state", "State"), ("date", "Ordered"), ("age", "Waiting"), ("total", "Total")], waiting_rows, 12));
    }
    if !lapsing_rows.is_empty() {
        items.push(table_card("lapsing", "Quotations lapsing within 14 days",
            &[("number", "Quotation"), ("customer", "Customer"), ("valid_until", "Valid until"), ("total", "Total")], lapsing_rows, 12));
    }
    if d.orders.is_empty() && d.quotes.is_empty() {
        items.push(text_card("empty", "No sales yet", "Orders and quotations appear here as soon as you create them."));
    }
    Ok(json!({
        "title": "Sales overview", "description": format!("{} to {} · {}", day(from), day(to), d.currency), "refresh_secs": 300,
        "filters": [ { "name": "period", "label": "Period", "kind": "period", "value": input.filters.as_ref().and_then(|f| f.get("period")).cloned().unwrap_or(json!({ "from": day(from), "to": day(to) })) }, d.currency_filter ],
        "items": items,
    }))
}

/// The orders behind a customer bar, a state slice or a month.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let d = load(input.filters.as_ref())?;
    let segment = input.segment.unwrap_or(Value::Null);
    let wanted = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);
    let mut rows: Vec<&Record> = ordered(&d, from, to);
    match input.chart.as_deref() {
        Some("customers") => {
            if let Some(c) = wanted("customer") {
                rows.retain(|o| text(o, "customer") == Some(c.as_str()));
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
            "number": text(o, "number"), "customer": name_of(text(o, "customer").unwrap_or_default()),
            "state": label_of(text(o, "state").unwrap_or_default()), "date": text(o, "order_date"), "total": text(o, "total"),
        }}))
        .collect();
    Ok(json!({
        "columns": [ {"key":"number","label":"Order"}, {"key":"customer","label":"Customer"}, {"key":"state","label":"State"}, {"key":"date","label":"Date"}, {"key":"total","label":"Total"} ],
        "rows": page, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The sales overview board for a period and currency.
    fn sales_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The orders behind one bar, slice or month of the sales overview.
    fn sales_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
