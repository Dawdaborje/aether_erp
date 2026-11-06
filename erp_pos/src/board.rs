//! The point-of-sale overview board: takings, baskets, tills, payment methods, best sellers, and the sessions and
//! cash differences that need a manager. One currency at a time.

use std::collections::{BTreeMap, BTreeSet};

use aether_sdk::dates::{parse_date, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::board_kit::*;
use crate::common::{dec, money_of, require_reader, text, today, Record};

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

struct Data {
    sales: Vec<Record>,
    sessions: Vec<Record>,
    tills: BTreeMap<String, String>,
    items: BTreeMap<String, String>,
    currency: String,
    currency_filter: Value,
}

fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

fn load(filters: Option<&Value>) -> Result<Data> {
    let sales: Vec<Record> = db::find("pos_sale").limit(20_000).all()?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for s in &sales {
        *counts.entry(text(s, "currency").unwrap_or_default().to_string()).or_default() += 1;
    }
    let (currency, currency_filter) = currency_filter(&counts, filters.and_then(|f| f.get("currency")));
    let sales: Vec<Record> = sales.into_iter().filter(|s| text(s, "currency").unwrap_or_default() == currency).collect();
    let profile_ids: BTreeSet<&str> = sales.iter().filter_map(|s| text(s, "profile")).collect();
    let sessions: Vec<Record> = db::find::<Record>("pos_session")
        .limit(10_000)
        .all()?
        .into_iter()
        .filter(|s| text(s, "profile").is_some_and(|p| profile_ids.contains(p)))
        .collect();
    let tills = db::find::<Record>("pos_profile")
        .limit(1000)
        .all()?
        .iter()
        .filter_map(|p| Some((key(text(p, "id")?).to_string(), text(p, "name")?.to_string())))
        .collect();
    let items = db::find::<Record>("erp_item.item")
        .limit(20_000)
        .all()?
        .iter()
        .filter_map(|i| Some((key(text(i, "id")?).to_string(), text(i, "name").or_else(|| text(i, "code"))?.to_string())))
        .collect();
    Ok(Data { sales, sessions, tills, items, currency, currency_filter })
}

fn till_name(d: &Data, id: &str) -> String {
    d.tills.get(key(id)).cloned().unwrap_or_else(|| if id.is_empty() { "Not set".to_string() } else { key(id).to_string() })
}

fn item_name(d: &Data, id: &str) -> String {
    d.items.get(key(id)).cloned().unwrap_or_else(|| key(id).to_string())
}

fn paid_in<'a>(d: &'a Data, from: NaiveDate, to: NaiveDate) -> Vec<&'a Record> {
    d.sales.iter().filter(|s| text(s, "state") == Some("paid") && in_range(text(s, "sale_date"), from, to)).collect()
}

fn takings(sales: &[&Record]) -> Result<f64> {
    let mut total = 0.0;
    for s in sales {
        total += num(money_of(s, "total")?);
    }
    Ok(total)
}

fn average(total: f64, n: usize) -> f64 {
    if n == 0 { 0.0 } else { total / n as f64 }
}

fn sale_row(d: &Data, s: &Record) -> Value {
    json!({ "number": text(s, "number"), "date": text(s, "sale_date"), "till": till_name(d, text(s, "profile").unwrap_or_default()),
        "state": label_of(text(s, "state").unwrap_or_default()), "total": text(s, "total") })
}

/// Lines of the given sales, read in chunks.
fn lines_of(sale_ids: &[&str]) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    for chunk in sale_ids.chunks(200) {
        out.extend(db::find::<Record>("pos_sale_line").matching(Filter::one_of("sale", chunk.iter().copied())).limit(10_000).all()?);
    }
    Ok(out)
}

fn payments_of(sale_ids: &[&str]) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    for chunk in sale_ids.chunks(200) {
        out.extend(db::find::<Record>("pos_payment").matching(Filter::one_of("sale", chunk.iter().copied())).limit(10_000).all()?);
    }
    Ok(out)
}

/// Cash and other takings of open sessions, worked out as the till would at close (a session only stores them then).
fn live_totals(d: &Data, sessions: &[&Record]) -> Result<BTreeMap<String, (f64, f64)>> {
    let open_ids: BTreeSet<&str> = sessions.iter().filter_map(|s| text(s, "id")).collect();
    let paid: Vec<&Record> = d.sales.iter().filter(|s| text(s, "state") == Some("paid") && text(s, "session").is_some_and(|x| open_ids.contains(x))).collect();
    let ids: Vec<&str> = paid.iter().filter_map(|s| text(s, "id")).collect();
    let mut out: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for p in payments_of(&ids)? {
        let e = out.entry(text(&p, "session").unwrap_or_default().to_string()).or_default();
        let amount = num(money_of(&p, "amount")?);
        if text(&p, "method") == Some("cash") { e.0 += amount } else { e.1 += amount }
    }
    for s in &paid {
        out.entry(text(s, "session").unwrap_or_default().to_string()).or_default().0 -= num(money_of(s, "change")?);
    }
    Ok(out)
}

fn board(input: BoardInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let (pfrom, pto) = prior(from, to);
    let d = load(input.filters.as_ref())?;
    let cur = Some(d.currency.as_str()).filter(|c| !c.is_empty());

    let now_sales = paid_in(&d, from, to);
    let prev_sales = paid_in(&d, pfrom, pto);
    let voids = |a, b| d.sales.iter().filter(|s| text(s, "state") == Some("void") && in_range(text(s, "sale_date"), a, b)).count() as f64;
    let (t_now, t_prev) = (takings(&now_sales)?, takings(&prev_sales)?);
    let money = number_card(
        "money", "Takings", vec![
            reading("takings", "Takings (paid sales)", t_now, Some(t_prev), Some(true)),
            reading("average", "Average basket", average(t_now, now_sales.len()), Some(average(t_prev, prev_sales.len())), Some(true)),
        ], "currency", cur, 6, "Paid sales dated in the period. Compared with the period of the same length just before.",
    );
    let counts = number_card(
        "counts", "Volume", vec![
            reading("receipts", "Receipts", now_sales.len() as f64, Some(prev_sales.len() as f64), Some(true)),
            reading("voids", "Voided sales", voids(from, to), Some(voids(pfrom, pto)), Some(false)),
        ], "number", None, 6, "Voids going up is usually bad news, so a rise is shown as such.",
    );

    // Today and the tills.
    let today_s = day(today);
    let today_sales: Vec<&Record> = d.sales.iter().filter(|s| text(s, "state") == Some("paid") && text(s, "sale_date").is_some_and(|x| x.starts_with(&today_s))).collect();
    let open_sessions: Vec<&Record> = d.sessions.iter().filter(|s| text(s, "state") == Some("open")).collect();
    let live = live_totals(&d, &open_sessions)?;
    let cash_open: f64 = live.values().map(|v| v.0).sum();
    let now_card = number_card(
        "now", "Today", vec![
            reading("today", "Taken today", takings(&today_sales)?, None, None),
            reading("sessions", "Open sessions", open_sessions.len() as f64, None, None),
            reading("cash", "Cash sales in open sessions", cash_open, None, None),
        ], "currency", cur, 12, "A snapshot of today. The period does not change it.",
    );

    // By month.
    let months = months_in(from, to);
    let mut by_month = vec![0.0; months.len()];
    for s in &now_sales {
        if let Some(i) = date_of(text(s, "sale_date")).and_then(|dt| month_index(&months, dt)) {
            by_month[i] += num(money_of(s, "total")?);
        }
    }
    let trend = series_card(
        "month", "bar", "Takings by month",
        months.iter().map(|(s, _)| month_label(*s)).collect(), months.iter().map(|(s, _)| json!(day(*s))).collect(),
        vec![("Takings", by_month)], "month", "compact", cur, 12,
    );

    // By till.
    let mut by_till: BTreeMap<String, f64> = BTreeMap::new();
    for s in &now_sales {
        *by_till.entry(text(s, "profile").unwrap_or_default().to_string()).or_default() += num(money_of(s, "total")?);
    }
    let mut tills: Vec<(String, f64)> = by_till.into_iter().collect();
    tills.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    tills.truncate(8);
    let till_card = series_card(
        "till", "row", "Takings by till",
        tills.iter().map(|(id, _)| till_name(&d, id)).collect(), tills.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Takings", tills.iter().map(|r| r.1).collect())], "till", "compact", cur, 6,
    );

    // By payment method, and best sellers, from the lines and payments of the period's sales.
    let ids: Vec<&str> = now_sales.iter().filter_map(|s| text(s, "id")).collect();
    let mut by_method: BTreeMap<String, f64> = BTreeMap::new();
    for p in payments_of(&ids)? {
        *by_method.entry(text(&p, "method").unwrap_or("other").to_string()).or_default() += num(money_of(&p, "amount")?);
    }
    let methods = series_card(
        "method", "donut", "Payments by method",
        by_method.keys().map(|m| label_of(m)).collect(), by_method.keys().map(|m| json!(m)).collect(),
        vec![("Paid", by_method.values().copied().collect())], "method", "compact", cur, 6,
    );
    let mut by_item: BTreeMap<String, f64> = BTreeMap::new();
    for l in lines_of(&ids)? {
        *by_item.entry(text(&l, "item").unwrap_or_default().to_string()).or_default() += num(money_of(&l, "amount")?);
    }
    let mut best: Vec<(String, f64)> = by_item.into_iter().collect();
    best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    best.truncate(8);
    let best_card = series_card(
        "item", "row", "Best sellers (net of tax)",
        best.iter().map(|(id, _)| item_name(&d, id)).collect(), best.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Sold", best.iter().map(|r| r.1).collect())], "item", "compact", cur, 12,
    );

    let mut items = vec![money, counts, now_card, trend, till_card, methods, best_card];

    // Sessions to look at: open ones, and closed ones in the period whose cash did not match.
    let open_rows: Vec<Value> = open_sessions
        .iter()
        .take(ROWS)
        .map(|s| json!({ "number": text(s, "number"), "till": till_name(&d, text(s, "profile").unwrap_or_default()), "opened": text(s, "opened_on"),
            "cash": format!("{:.2}", live.get(text(s, "id").unwrap_or_default()).map_or(0.0, |v| v.0)),
            "card": format!("{:.2}", live.get(text(s, "id").unwrap_or_default()).map_or(0.0, |v| v.1)) }))
        .collect();
    if !open_rows.is_empty() {
        items.push(table_card("open_sessions", &format!("Open sessions ({})", open_sessions.len()),
            &[("number", "Session"), ("till", "Till"), ("opened", "Opened"), ("cash", "Cash taken"), ("card", "Other taken")], open_rows, 12));
    }
    let mut off: Vec<&Record> = d.sessions.iter().filter(|s| text(s, "state") == Some("closed") && in_range(text(s, "closed_on"), from, to)
        && dec(s, "difference", 2).map(|x| !x.is_zero()).unwrap_or(false)).collect();
    off.sort_by(|a, b| text(b, "closed_on").cmp(&text(a, "closed_on")));
    let off_rows: Vec<Value> = off
        .iter()
        .take(ROWS)
        .map(|s| json!({ "number": text(s, "number"), "till": till_name(&d, text(s, "profile").unwrap_or_default()), "closed": text(s, "closed_on"),
            "expected": text(s, "expected_cash"), "counted": text(s, "counted_cash"), "difference": text(s, "difference") }))
        .collect();
    if !off_rows.is_empty() {
        items.push(table_card("differences", &format!("Sessions where the cash did not match ({})", off.len()),
            &[("number", "Session"), ("till", "Till"), ("closed", "Closed"), ("expected", "Expected"), ("counted", "Counted"), ("difference", "Difference")], off_rows, 12));
    }
    if d.sales.is_empty() {
        items.push(text_card("empty", "No sales yet", "Counter sales appear here as soon as a cashier takes payment."));
    }
    let _ = Duration::days(0);
    Ok(json!({
        "title": "Point of sale overview", "description": if d.currency.is_empty() { format!("{} to {}", day(from), day(to)) } else { format!("{} to {} · {}", day(from), day(to), d.currency) }, "refresh_secs": 120,
        "filters": [ { "name": "period", "label": "Period", "kind": "period", "value": input.filters.as_ref().and_then(|f| f.get("period")).cloned().unwrap_or(json!({ "from": day(from), "to": day(to) })) }, d.currency_filter ],
        "items": items,
    }))
}

/// The sales behind a month, a till, a payment method or an item.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let today = today_date()?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let d = load(input.filters.as_ref())?;
    let segment = input.segment.unwrap_or(Value::Null);
    let wanted = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);
    let mut sales = paid_in(&d, from, to);
    match input.chart.as_deref() {
        Some("till") => {
            if let Some(t) = wanted("till") {
                sales.retain(|s| text(s, "profile") == Some(t.as_str()));
            }
        }
        Some("month") => {
            if let Some(start) = wanted("month").and_then(|m| date_of(Some(m.as_str()))) {
                let end = aether_sdk::dates::add_months(start, 1) - Duration::days(1);
                sales.retain(|s| in_range(text(s, "sale_date"), start, end));
            }
        }
        Some("method") => {
            if let Some(m) = wanted("method") {
                let ids: Vec<&str> = sales.iter().filter_map(|s| text(s, "id")).collect();
                let hit: BTreeSet<String> = payments_of(&ids)?.iter().filter(|p| text(p, "method") == Some(m.as_str())).filter_map(|p| text(p, "sale").map(str::to_string)).collect();
                sales.retain(|s| text(s, "id").is_some_and(|id| hit.contains(id)));
            }
        }
        Some("item") => {
            if let Some(i) = wanted("item") {
                let ids: Vec<&str> = sales.iter().filter_map(|s| text(s, "id")).collect();
                let hit: BTreeSet<String> = lines_of(&ids)?.iter().filter(|l| text(l, "item") == Some(i.as_str())).filter_map(|l| text(l, "sale").map(str::to_string)).collect();
                sales.retain(|s| text(s, "id").is_some_and(|id| hit.contains(id)));
            }
        }
        _ => {}
    }
    sales.sort_by(|a, b| text(b, "sale_date").cmp(&text(a, "sale_date")));
    let total = sales.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let rows: Vec<Value> = sales.iter().skip(offset).take(limit).map(|s| json!({ "id": text(s, "id"), "cells": sale_row(&d, s) })).collect();
    Ok(json!({
        "columns": [ {"key":"number","label":"Receipt"}, {"key":"date","label":"Date"}, {"key":"till","label":"Till"}, {"key":"state","label":"State"}, {"key":"total","label":"Total"} ],
        "rows": rows, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The point-of-sale overview board for a period and currency.
    fn pos_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The sales behind one bar, slice or month of the point-of-sale overview.
    fn pos_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
