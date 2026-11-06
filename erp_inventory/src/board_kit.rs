//! Small helpers for `board` dashboards: the period filter, month buckets, and the cards.
//!
//! The same file sits in each ERP plugin that has a board (plugins cannot share code), so change them together.

#![allow(dead_code)]

use std::collections::BTreeMap;

use aether_sdk::dates::{self, Datelike, Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

/// A decimal as a plain number for a chart. Charts take floats; the books stay exact.
pub fn num(d: Decimal) -> f64 {
    match serde_json::to_value(d) {
        Ok(Value::String(s)) => s.parse().unwrap_or(0.0),
        Ok(v) => v.as_f64().unwrap_or(0.0),
        Err(_) => 0.0,
    }
}

pub fn key(v: &str) -> &str {
    v.rsplit(':').next().unwrap_or(v)
}

pub fn day(date: NaiveDate) -> String {
    dates::format_date(date)
}

pub fn date_of(text: Option<&str>) -> Option<NaiveDate> {
    text.and_then(|d| dates::parse_date(d).ok())
}

pub fn in_range(text: Option<&str>, from: NaiveDate, to: NaiveDate) -> bool {
    date_of(text).is_some_and(|d| d >= from && d <= to)
}

pub fn label_of(raw: &str) -> String {
    let mut chars: Vec<char> = raw.replace('_', " ").chars().collect();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// The period the person picked: a named one, a `{from, to}` range, or the last twelve months.
pub fn resolve_period(value: Option<&Value>, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let default = (today - Duration::days(364), today);
    let Some(value) = value else { return default };
    if let Some(range) = value.as_object() {
        let from = range.get("from").and_then(Value::as_str).and_then(|d| dates::parse_date(d).ok());
        let to = range.get("to").and_then(Value::as_str).and_then(|d| dates::parse_date(d).ok());
        return match (from, to) {
            (Some(from), Some(to)) if from <= to => (from, to),
            _ => default,
        };
    }
    let first_of = |year: i32, month: u32| NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(today);
    let quarter_start = |d: NaiveDate| first_of(d.year(), (d.month() - 1) / 3 * 3 + 1);
    match value.as_str().unwrap_or_default() {
        "today" => (today, today),
        "last_7_days" => (today - Duration::days(6), today),
        "last_30_days" => (today - Duration::days(29), today),
        "last_90_days" => (today - Duration::days(89), today),
        "this_month" => (first_of(today.year(), today.month()), today),
        "last_month" => {
            let end = first_of(today.year(), today.month()) - Duration::days(1);
            (first_of(end.year(), end.month()), end)
        }
        "this_quarter" => (quarter_start(today), today),
        "last_quarter" => {
            let end = quarter_start(today) - Duration::days(1);
            (quarter_start(end), end)
        }
        "this_year" | "year_to_date" => (first_of(today.year(), 1), today),
        "last_year" => (first_of(today.year() - 1, 1), first_of(today.year(), 1) - Duration::days(1)),
        _ => default,
    }
}

/// The period of the same length just before.
pub fn prior(from: NaiveDate, to: NaiveDate) -> (NaiveDate, NaiveDate) {
    let length = (to - from).num_days() + 1;
    (from - Duration::days(length), from - Duration::days(1))
}

fn month_start(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date)
}

pub fn month_label(date: NaiveDate) -> String {
    const NAMES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {}", NAMES[(date.month() - 1) as usize], date.year() % 100)
}

/// The months a period touches, oldest first, at most 24: `(first day, last day inside the period)`.
pub fn months_in(from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut months = Vec::new();
    let mut start = month_start(from);
    while start <= to && months.len() < 24 {
        let next = dates::add_months(start, 1);
        months.push((start.max(from), (next - Duration::days(1)).min(to)));
        start = next;
    }
    months
}

/// Which month bucket a date belongs to, as an index into `months_in`.
pub fn month_index(months: &[(NaiveDate, NaiveDate)], date: NaiveDate) -> Option<usize> {
    months.iter().position(|(s, e)| date >= *s && date <= *e)
}

/// `(now - previous) / previous`, or nothing without a previous figure.
fn change(now: f64, previous: f64) -> Value {
    if previous == 0.0 { Value::Null } else { json!((now - previous) / previous) }
}

/// One reading of a number card, compared with `previous` when given. `more_is_good` says which way is good news.
pub fn reading(name: &str, label: &str, now: f64, previous: Option<f64>, more_is_good: Option<bool>) -> Value {
    let Some(previous) = previous else {
        return json!({ "name": name, "label": label, "value": now, "previous": null, "change": null });
    };
    let tone = match more_is_good {
        _ if now == previous => "flat",
        Some(true) => if now > previous { "good" } else { "bad" },
        Some(false) => if now < previous { "good" } else { "bad" },
        None => "flat",
    };
    json!({ "name": name, "label": label, "value": now, "previous": previous, "change": change(now, previous), "tone": tone })
}

pub fn number_card(id: &str, title: &str, readings: Vec<Value>, format: &str, currency: Option<&str>, width: u32, info: &str) -> Value {
    json!({ "id": id, "width": width, "card": {
        "kind": "number", "title": title, "format": format, "currency": currency, "info": info, "readings": readings, "compared": true,
    }})
}

/// A chart card. `series` is `(name, values)`; `keys` are the raw values a click sends to the drill-down.
#[allow(clippy::too_many_arguments)]
pub fn series_card(
    id: &str, kind: &str, title: &str, categories: Vec<String>, keys: Vec<Value>, series: Vec<(&str, Vec<f64>)>,
    dimension: &str, format: &str, currency: Option<&str>, width: u32,
) -> Value {
    json!({ "id": id, "chart": id, "width": width, "card": {
        "kind": kind, "title": title, "format": format, "currency": currency,
        "categories": categories, "keys": keys, "dimensions": [dimension],
        "series": series.iter().map(|(n, v)| json!({ "name": n, "measure": "sum", "values": v })).collect::<Vec<_>>(),
    }})
}

/// A chart card with no drill-down.
#[allow(clippy::too_many_arguments)]
pub fn plain_series_card(
    id: &str, kind: &str, title: &str, categories: Vec<String>, series: Vec<(&str, Vec<f64>)>, format: &str, currency: Option<&str>, width: u32,
) -> Value {
    let keys = categories.iter().map(|c| json!(c)).collect();
    let mut card = series_card(id, kind, title, categories, keys, series, "stage", format, currency, width);
    card.as_object_mut().map(|o| o.remove("chart"));
    card
}

pub fn table_card(id: &str, title: &str, columns: &[(&str, &str)], rows: Vec<Value>, width: u32) -> Value {
    json!({ "id": id, "width": width, "card": {
        "kind": "table", "title": title,
        "columns": columns.iter().map(|(k, l)| json!({ "key": k, "label": l, "type": "dimension" })).collect::<Vec<_>>(),
        "rows": rows.into_iter().map(|cells| json!({ "cells": cells, "keys": {} })).collect::<Vec<_>>(),
    }})
}

pub fn text_card(id: &str, title: &str, body: &str) -> Value {
    json!({ "id": id, "width": 12, "card": { "kind": "text", "title": title, "text": body } })
}

/// The most-used currency among `counts`, unless the person chose one that is present; with the select filter.
/// Totals from different currencies are never added together.
pub fn currency_filter(counts: &BTreeMap<String, usize>, chosen: Option<&Value>) -> (String, Value) {
    let chosen = chosen
        .and_then(|v| match v {
            Value::Array(list) => list.first().and_then(Value::as_str),
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
        .filter(|c| counts.contains_key(*c));
    let picked = chosen
        .map(str::to_string)
        .or_else(|| counts.iter().max_by_key(|(_, n)| **n).map(|(c, _)| c.clone()))
        .unwrap_or_default();
    let options: Vec<Value> = counts.keys().map(|c| json!({ "value": c, "label": c })).collect();
    (picked.clone(), json!({ "name": "currency", "label": "Currency", "kind": "select", "value": [picked], "options": options }))
}
