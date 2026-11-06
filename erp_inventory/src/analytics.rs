//! Inventory analytics: ABC classes, turnover and days of cover, slow-moving stock, and which items are due for a
//! cycle count. Everything is read from the ledger and the open layers; nothing is stored or changed here.
//!
//! * **Usage** is the value of stock that left in the last 365 days (issues and production consumption, at cost).
//! * **ABC** ranks items by usage: A is the top 80% of it, B the next 15%, C the rest. Items that are held but
//!   had no usage in the year are shown apart as *No movement*.
//! * **Turnover** is usage over the stock value held today, so it is an approximation (the platform keeps no
//!   daily stock history to average over).
//! * **Cycle counts**: A items every 90 days, B every 180, the rest every 365, measured from the last posted count.

use std::collections::BTreeMap;

use aether_sdk::dates::{Duration, NaiveDate};
use aether_sdk::prelude::*;

use crate::board::{category_of, item_name, load, name_in, Data};
use crate::board_kit::*;
use crate::common::{require_reader, text, today, value_of, Record};

#[derive(Deserialize, Default)]
struct BoardInput {
    #[serde(default)]
    filters: Option<Value>,
}

#[derive(Deserialize, Default)]
struct DrillInput {
    #[serde(default)]
    #[allow(dead_code)]
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

const WINDOW_DAYS: i64 = 365;
const SLOW_DAYS: i64 = 90;
const ROWS: usize = 10;
const CLASSES: &[(&str, &str)] = &[("A", "A: top 80% of usage"), ("B", "B: next 15%"), ("C", "C: the rest"), ("N", "No movement in a year")];

#[derive(Default, Clone)]
struct ItemFacts {
    qty: f64,
    value: f64,
    usage: f64,
    last_issue: Option<NaiveDate>,
    last_count: Option<NaiveDate>,
    class: &'static str,
}

struct Analysis {
    d: Data,
    today: NaiveDate,
    facts: BTreeMap<String, ItemFacts>,
}

fn class_days(class: &str) -> i64 {
    match class {
        "A" => 90,
        "B" => 180,
        _ => 365,
    }
}

fn analyse(filters: Option<&Value>) -> Result<Analysis> {
    let today_d = aether_sdk::dates::parse_date(&today()?)?;
    let d = load(filters)?;
    let mut facts: BTreeMap<String, ItemFacts> = BTreeMap::new();
    for h in &d.held {
        let f = facts.entry(key(&h.item).to_string()).or_default();
        f.qty += h.qty;
        f.value += h.value;
    }

    // Issues: posted moves of this company, then their lines.
    let moves: Vec<Record> = db::find::<Record>("stock_move").filter("state", "posted").limit(50_000).all()?;
    let mut issue_moves: BTreeMap<String, Option<NaiveDate>> = BTreeMap::new();
    for m in moves.iter().filter(|m| text(m, "company") == Some(d.company.as_str())) {
        if matches!(text(m, "kind"), Some("issue" | "manufacture_consume")) {
            if let Some(id) = text(m, "id") {
                issue_moves.insert(key(id).to_string(), date_of(text(m, "posting_date")));
            }
        }
    }
    let window_start = today_d - Duration::days(WINDOW_DAYS);
    let lines: Vec<Record> = db::find("stock_move_line").limit(100_000).all()?;
    for l in &lines {
        let Some(date) = text(l, "move").and_then(|m| issue_moves.get(key(m))) else { continue };
        let Some(item) = text(l, "item") else { continue };
        let f = facts.entry(key(item).to_string()).or_default();
        if let Some(date) = date {
            if f.last_issue.map_or(true, |x| *date > x) {
                f.last_issue = Some(*date);
            }
            if *date >= window_start {
                f.usage += num(value_of(l, "value")?).abs();
            }
        }
    }

    // Last posted count per item.
    let counts: Vec<Record> = db::find::<Record>("stock_count").filter("state", "posted").limit(5000).all()?;
    let count_dates: BTreeMap<String, Option<NaiveDate>> = counts
        .iter()
        .filter(|c| text(c, "company") == Some(d.company.as_str()))
        .filter_map(|c| Some((key(text(c, "id")?).to_string(), date_of(text(c, "count_date")))))
        .collect();
    if !count_dates.is_empty() {
        let count_lines: Vec<Record> = db::find("stock_count_line").limit(100_000).all()?;
        for l in &count_lines {
            let (Some(count), Some(item)) = (text(l, "count"), text(l, "item")) else { continue };
            let Some(Some(date)) = count_dates.get(key(count)) else { continue };
            let f = facts.entry(key(item).to_string()).or_default();
            if f.last_count.map_or(true, |x| *date > x) {
                f.last_count = Some(*date);
            }
        }
    }

    // ABC over the items that had usage.
    let mut ranked: Vec<(String, f64)> = facts.iter().filter(|(_, f)| f.usage > 0.0).map(|(k, f)| (k.clone(), f.usage)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = ranked.iter().map(|r| r.1).sum();
    let mut before = 0.0;
    for (k, usage) in &ranked {
        let share = if total > 0.0 { before / total } else { 1.0 };
        if let Some(f) = facts.get_mut(k) {
            f.class = if share < 0.80 { "A" } else if share < 0.95 { "B" } else { "C" };
        }
        before += usage;
    }
    for f in facts.values_mut() {
        if f.class.is_empty() {
            f.class = "N";
        }
    }
    Ok(Analysis { d, today: today_d, facts })
}

impl Analysis {
    fn held(&self) -> impl Iterator<Item = (&String, &ItemFacts)> {
        self.facts.iter().filter(|(_, f)| f.value > 0.0 || f.qty > 0.0)
    }
}

fn since(date: Option<NaiveDate>, today: NaiveDate) -> String {
    date.map_or("Never".to_string(), |d| format!("{} ({} days ago)", day(d), (today - d).num_days()))
}

fn board(input: BoardInput) -> Result<Value> {
    require_reader()?;
    let a = analyse(input.filters.as_ref())?;
    let d = &a.d;
    let cur = Some(d.currency.as_str()).filter(|c| !c.is_empty());

    let stock_value: f64 = a.held().map(|(_, f)| f.value).sum();
    let usage: f64 = a.facts.values().map(|f| f.usage).sum();
    let turnover = if stock_value > 0.0 { usage / stock_value } else { 0.0 };
    let cover = if usage > 0.0 { stock_value / usage * 365.0 } else { 0.0 };

    let is_slow = |f: &ItemFacts| f.value > 0.0 && f.last_issue.map_or(true, |l| (a.today - l).num_days() > SLOW_DAYS);
    let slow_value: f64 = a.held().filter(|(_, f)| is_slow(f)).map(|(_, f)| f.value).sum();
    let dead_value: f64 = a.held().filter(|(_, f)| f.class == "N").map(|(_, f)| f.value).sum();

    let mut efficiency = vec![reading("turnover", "Turns per year", turnover, None, None)];
    if cover > 0.0 {
        efficiency.push(reading("cover", "Days of stock on hand", cover, None, None));
    }
    let efficiency_card = number_card(
        "efficiency", "How fast stock moves", efficiency, "number", None, 6,
        "Usage in the last 365 days at cost, divided by the stock value held today. Days of stock is how long today's stock would last at that pace.",
    );
    let risk_card = number_card(
        "risk", "Stock that is not moving",
        vec![
            reading("slow", &format!("Not issued for {SLOW_DAYS} days"), slow_value, None, None),
            reading("dead", "No issue in a year", dead_value, None, None),
        ],
        "currency", cur, 6, "Value at cost of stock that is held but has not left. Slow includes everything in the second figure.",
    );

    // ABC: stock value per class, with how many items are in it.
    let mut by_class: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    for (_, f) in a.held() {
        let e = by_class.entry(f.class).or_default();
        e.0 += f.value;
        e.1 += 1;
    }
    let abc = series_card(
        "abc", "bar", "Stock value by ABC class",
        CLASSES.iter().map(|(k, label)| format!("{label} ({} items)", by_class.get(k).map_or(0, |e| e.1))).collect(),
        CLASSES.iter().map(|(k, _)| json!(k)).collect(),
        vec![("Stock value", CLASSES.iter().map(|(k, _)| by_class.get(k).map_or(0.0, |e| e.0)).collect())],
        "abc", "compact", cur, 12,
    );

    // Turnover by category.
    let mut cat: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (k, f) in a.held() {
        let e = cat.entry(category_of(d, k)).or_default();
        e.0 += f.value;
        e.1 += f.usage;
    }
    let mut cats: Vec<(String, f64)> = cat.iter().filter(|(_, e)| e.0 > 0.0).map(|(c, e)| (name_in(&d.categories, c), e.1 / e.0)).collect();
    cats.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
    cats.truncate(10);
    let turnover_by_category = plain_series_card(
        "category_turns", "row", "Turns per year by category",
        cats.iter().map(|c| c.0.clone()).collect(), vec![("Turns", cats.iter().map(|c| c.1).collect())], "number", None, 6,
    );

    // Slow movers, biggest first.
    let mut slow: Vec<(&String, &ItemFacts)> = a.held().filter(|(_, f)| is_slow(f)).collect();
    slow.sort_by(|x, y| y.1.value.partial_cmp(&x.1.value).unwrap_or(std::cmp::Ordering::Equal));
    let slow_rows: Vec<Value> = slow
        .iter()
        .take(ROWS)
        .map(|(k, f)| json!({ "item": item_name(d, k), "class": f.class, "qty": format!("{}", f.qty), "value": format!("{:.2}", f.value), "last": since(f.last_issue, a.today) }))
        .collect();

    // Cycle counts due.
    let mut due: Vec<(&String, &ItemFacts, i64)> = a
        .held()
        .filter(|(_, f)| f.value > 0.0)
        .filter_map(|(k, f)| {
            let age = f.last_count.map_or(i64::MAX, |c| (a.today - c).num_days());
            (age > class_days(f.class)).then_some((k, f, age))
        })
        .collect();
    due.sort_by(|x, y| y.2.cmp(&x.2).then(y.1.value.partial_cmp(&x.1.value).unwrap_or(std::cmp::Ordering::Equal)));
    let due_rows: Vec<Value> = due
        .iter()
        .take(ROWS)
        .map(|(k, f, _)| json!({ "item": item_name(d, k), "class": f.class, "value": format!("{:.2}", f.value), "counted": since(f.last_count, a.today), "every": format!("{} days", class_days(f.class)) }))
        .collect();

    let mut items = vec![efficiency_card, risk_card, abc, turnover_by_category];
    if !slow_rows.is_empty() {
        items.push(table_card(
            "slow", &format!("Slow-moving stock ({} items)", slow.len()),
            &[("item", "Item"), ("class", "Class"), ("qty", "On hand"), ("value", "Value"), ("last", "Last issued")], slow_rows, 12,
        ));
    }
    if !due_rows.is_empty() {
        items.push(table_card(
            "due", &format!("Due for a cycle count ({} items)", due.len()),
            &[("item", "Item"), ("class", "Class"), ("value", "Value"), ("counted", "Last counted"), ("every", "Count every")], due_rows, 12,
        ));
    } else if stock_value > 0.0 {
        items.push(text_card("due_ok", "Cycle counts", "Every item with stock has been counted within its schedule (A every 90 days, B every 180, others every 365)."));
    }
    if d.held.is_empty() {
        items.push(text_card("empty", "No stock yet", "Analytics appear once stock has been received and issued."));
    }
    Ok(json!({
        "title": "Inventory analytics",
        "description": format!("{} · usage over the last {WINDOW_DAYS} days", d.currency),
        "refresh_secs": 600,
        "filters": [ d.company_filter.clone() ],
        "items": items,
    }))
}

/// The items in one ABC class: stock held, usage in the year and turns.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let a = analyse(input.filters.as_ref())?;
    let wanted = input.segment.as_ref().and_then(|s| s.get("abc")).and_then(Value::as_str).map(String::from);
    let mut rows: Vec<(&String, &ItemFacts)> = a.held().filter(|(_, f)| wanted.as_deref().map_or(true, |w| f.class == w)).collect();
    rows.sort_by(|x, y| y.1.usage.partial_cmp(&x.1.usage).unwrap_or(std::cmp::Ordering::Equal).then(y.1.value.partial_cmp(&x.1.value).unwrap_or(std::cmp::Ordering::Equal)));
    let total = rows.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let page: Vec<Value> = rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(k, f)| {
            let turns = if f.value > 0.0 { f.usage / f.value } else { 0.0 };
            json!({ "id": k, "cells": {
                "item": item_name(&a.d, k), "class": f.class, "qty": format!("{}", f.qty), "value": format!("{:.2}", f.value),
                "usage": format!("{:.2}", f.usage), "turns": format!("{turns:.1}"), "last": since(f.last_issue, a.today),
            }})
        })
        .collect();
    Ok(json!({
        "columns": [
            {"key":"item","label":"Item"}, {"key":"class","label":"Class"}, {"key":"qty","label":"On hand"}, {"key":"value","label":"Value"},
            {"key":"usage","label":"Used in a year"}, {"key":"turns","label":"Turns"}, {"key":"last","label":"Last issued"},
        ],
        "rows": page, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// ABC classes, turnover, slow-moving stock and cycle counts due.
    fn analytics_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The items behind one ABC class.
    fn analytics_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
