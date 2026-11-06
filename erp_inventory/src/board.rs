//! The inventory overview board: what stock is worth and where, how old it is, what is about to expire, and
//! how much moved. Stock comes from the open layers (the ledger's own cache), so it always agrees with `on_hand`.

use std::collections::BTreeMap;

use aether_sdk::dates::{Duration, NaiveDate};
use aether_sdk::prelude::*;

use crate::board_kit::*;
use crate::common::{qty_of, require_reader, text, today, value_of, Record};

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
const LAYER_LIMIT: u32 = 20_000;
const AGE_BANDS: &[&str] = &["Up to 30 days", "31 to 90 days", "91 to 180 days", "181 to 365 days", "Over a year"];

fn age_band(days: i64) -> usize {
    match days {
        d if d <= 30 => 0,
        d if d <= 90 => 1,
        d if d <= 180 => 2,
        d if d <= 365 => 3,
        _ => 4,
    }
}

/// An open layer: stock still on hand.
pub(crate) struct Held {
    pub(crate) item: String,
    pub(crate) warehouse: String,
    pub(crate) lot: String,
    pub(crate) qty: f64,
    pub(crate) value: f64,
    pub(crate) opened: Option<NaiveDate>,
}

pub(crate) struct Data {
    pub(crate) held: Vec<Held>,
    pub(crate) truncated: bool,
    pub(crate) company: String,
    pub(crate) currency: String,
    pub(crate) company_filter: Value,
    pub(crate) items: BTreeMap<String, Record>,
    pub(crate) categories: BTreeMap<String, String>,
    pub(crate) warehouses: BTreeMap<String, String>,
}

pub(crate) fn name_in(map: &BTreeMap<String, String>, id: &str) -> String {
    map.get(key(id)).cloned().unwrap_or_else(|| if id.is_empty() { "Not set".to_string() } else { key(id).to_string() })
}

pub(crate) fn load(filters: Option<&Value>) -> Result<Data> {
    let layers: Vec<Record> = db::find("stock_layer").limit(LAYER_LIMIT).all()?;
    let truncated = layers.len() as u32 >= LAYER_LIMIT;
    // The company: the one picked, else the one with most layers.
    let mut by_company: BTreeMap<String, usize> = BTreeMap::new();
    for l in &layers {
        *by_company.entry(text(l, "company").unwrap_or_default().to_string()).or_default() += 1;
    }
    let chosen = filters
        .and_then(|f| f.get("company"))
        .and_then(|v| match v {
            Value::Array(list) => list.first().and_then(Value::as_str),
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
        .filter(|c| by_company.contains_key(*c))
        .map(str::to_string);
    let company = chosen.or_else(|| by_company.iter().max_by_key(|(_, n)| **n).map(|(c, _)| c.clone())).unwrap_or_default();
    let mut options = Vec::new();
    for id in by_company.keys() {
        let label = db::get::<Record>("company.company", id).ok().flatten().and_then(|c| text(&c, "name").map(str::to_string)).unwrap_or_else(|| key(id).to_string());
        options.push(json!({ "value": id, "label": label }));
    }
    let currency = if company.is_empty() { String::new() } else { crate::common::company_currency_code(&company).unwrap_or_default() };
    let mut held = Vec::new();
    for l in layers.iter().filter(|l| text(l, "company") == Some(company.as_str())) {
        let qty = num(qty_of(l, "remaining_qty")?);
        if qty <= 0.0 {
            continue;
        }
        held.push(Held {
            item: text(l, "item").unwrap_or_default().to_string(),
            warehouse: text(l, "warehouse").unwrap_or_default().to_string(),
            lot: text(l, "lot").unwrap_or_default().to_string(),
            qty,
            value: num(value_of(l, "remaining_value")?),
            opened: date_of(text(l, "opened_at")),
        });
    }
    let items: BTreeMap<String, Record> = db::find::<Record>("erp_item.item")
        .limit(20_000)
        .all()?
        .into_iter()
        .filter_map(|i| Some((key(text(&i, "id")?).to_string(), i)))
        .collect();
    let categories: BTreeMap<String, String> = db::find::<Record>("erp_item.item_category")
        .limit(5000)
        .all()?
        .iter()
        .filter_map(|c| Some((key(text(c, "id")?).to_string(), text(c, "name")?.to_string())))
        .collect();
    let warehouses: BTreeMap<String, String> = db::find::<Record>("stock_warehouse")
        .limit(2000)
        .all()?
        .iter()
        .filter_map(|w| Some((key(text(w, "id")?).to_string(), text(w, "name")?.to_string())))
        .collect();
    Ok(Data {
        held, truncated,
        currency,
        company_filter: json!({ "name": "company", "label": "Company", "kind": "select", "value": [company.clone()], "options": options }),
        company,
        items, categories, warehouses,
    })
}

pub(crate) fn item_name(d: &Data, id: &str) -> String {
    d.items.get(key(id)).and_then(|i| text(i, "name").or_else(|| text(i, "code"))).map(str::to_string).unwrap_or_else(|| key(id).to_string())
}

pub(crate) fn category_of(d: &Data, item: &str) -> String {
    d.items.get(key(item)).and_then(|i| text(i, "category")).unwrap_or_default().to_string()
}

fn board(input: BoardInput) -> Result<Value> {
    require_reader()?;
    let today_d = aether_sdk::dates::parse_date(&today()?)?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today_d);
    let d = load(input.filters.as_ref())?;
    let cur = Some(d.currency.as_str()).filter(|c| !c.is_empty());

    let total_value: f64 = d.held.iter().map(|h| h.value).sum();
    let skus = d.held.iter().map(|h| key(&h.item)).collect::<std::collections::BTreeSet<_>>().len();
    let lots = d.held.iter().filter(|h| !h.lot.is_empty()).map(|h| (key(&h.item), h.lot.as_str())).collect::<std::collections::BTreeSet<_>>().len();
    let places = d.held.iter().map(|h| key(&h.warehouse)).collect::<std::collections::BTreeSet<_>>().len();

    // Lots that expire soon, with what is left of them.
    let soon = today_d + Duration::days(30);
    let master: Vec<Record> = db::find("stock_lot").limit(20_000).all()?;
    let mut expiring: Vec<(NaiveDate, Value)> = Vec::new();
    let mut expired_count = 0usize;
    for lot in master.iter().filter(|l| text(l, "company") == Some(d.company.as_str())) {
        let (Some(expiry), Some(item), Some(code)) = (date_of(text(lot, "expiry_date")), text(lot, "item"), text(lot, "code")) else { continue };
        let qty: f64 = d.held.iter().filter(|h| key(&h.item) == key(item) && h.lot == code).map(|h| h.qty).sum();
        let value: f64 = d.held.iter().filter(|h| key(&h.item) == key(item) && h.lot == code).map(|h| h.value).sum();
        if qty <= 0.0 || expiry > soon {
            continue;
        }
        let left = (expiry - today_d).num_days();
        if left < 0 {
            expired_count += 1;
        }
        let when = if left < 0 { format!("Expired {} days ago", -left) } else if left == 0 { "Expires today".to_string() } else { format!("In {left} days") };
        expiring.push((expiry, json!({ "item": item_name(&d, item), "lot": code, "expiry": text(lot, "expiry_date"), "when": when, "qty": format!("{qty}"), "value": format!("{value:.2}") })));
    }
    expiring.sort_by_key(|e| e.0);

    let value_card = number_card(
        "value", "Stock", vec![reading("value", "Stock value", total_value, None, None)], "currency", cur, 12,
        "What the open layers hold at their cost, in the company's currency. Add or take out a move and this changes with it.",
    );
    let mut count_readings = vec![
        reading("skus", "Items in stock", skus as f64, None, None),
        reading("warehouses", "Warehouses holding stock", places as f64, None, None),
    ];
    if lots > 0 {
        count_readings.push(reading("lots", "Lots and serials held", lots as f64, None, None));
        count_readings.push(reading("expiring", "Lots expiring within 30 days", (expiring.len() - expired_count) as f64, None, None));
        count_readings.push(reading("expired", "Expired, still on hand", expired_count as f64, None, None));
    }
    let counts = number_card("counts", "Today", count_readings, "number", None, 12, "A snapshot of today. The period does not change it.");

    // By warehouse and by category.
    let mut by_warehouse: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_category: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_age = vec![0.0; AGE_BANDS.len()];
    for h in &d.held {
        *by_warehouse.entry(h.warehouse.clone()).or_default() += h.value;
        *by_category.entry(category_of(&d, &h.item)).or_default() += h.value;
        by_age[age_band(h.opened.map_or(0, |o| (today_d - o).num_days()))] += h.value;
    }
    let ranked = |m: BTreeMap<String, f64>, n: usize| {
        let mut v: Vec<(String, f64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(n);
        v
    };
    let wh = ranked(by_warehouse, 10);
    let warehouses = series_card(
        "warehouse", "row", "Value by warehouse",
        wh.iter().map(|(id, _)| name_in(&d.warehouses, id)).collect(), wh.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Value", wh.iter().map(|r| r.1).collect())], "warehouse", "compact", cur, 6,
    );
    let cat = ranked(by_category, 8);
    let categories = series_card(
        "category", "donut", "Value by category",
        cat.iter().map(|(id, _)| name_in(&d.categories, id)).collect(), cat.iter().map(|(id, _)| json!(id)).collect(),
        vec![("Value", cat.iter().map(|r| r.1).collect())], "category", "compact", cur, 6,
    );
    let aging = series_card(
        "age", "bar", "How long stock has been on hand",
        AGE_BANDS.iter().map(|b| b.to_string()).collect(), AGE_BANDS.iter().map(|b| json!(b)).collect(),
        vec![("Value", by_age)], "age", "compact", cur, 12,
    );

    // Movement in the period: posted receipts in against issues out, by month.
    let moves: Vec<Record> = db::find::<Record>("stock_move").filter("state", "posted").limit(20_000).all()?;
    let months = months_in(from, to);
    let (mut receipts, mut issues) = (vec![0.0; months.len()], vec![0.0; months.len()]);
    for m in moves.iter().filter(|m| text(m, "company") == Some(d.company.as_str()) && in_range(text(m, "posting_date"), from, to)) {
        let Some(i) = date_of(text(m, "posting_date")).and_then(|dt| month_index(&months, dt)) else { continue };
        let value = num(value_of(m, "total_value")?).abs();
        match text(m, "kind") {
            Some("receipt" | "manufacture_produce") => receipts[i] += value,
            Some("issue" | "manufacture_consume") => issues[i] += value,
            _ => {}
        }
    }
    let flow = plain_series_card(
        "flow", "bar", "Stock in and out by month",
        months.iter().map(|(s, _)| month_label(*s)).collect(), vec![("In (receipts, production)", receipts), ("Out (issues, consumption)", issues)], "compact", cur, 12,
    );

    let mut items = vec![value_card, counts, warehouses, categories, aging, flow];
    if !expiring.is_empty() {
        items.push(table_card("expiring", &format!("Lots expired or expiring within 30 days ({})", expiring.len()),
            &[("item", "Item"), ("lot", "Lot"), ("expiry", "Expires"), ("when", "When"), ("qty", "On hand"), ("value", "Value")],
            expiring.iter().take(ROWS).map(|e| e.1.clone()).collect(), 12));
    }
    if d.held.is_empty() {
        items.push(text_card("empty", "No stock yet", "Stock appears here once a receipt or an opening balance is posted."));
    }
    if d.truncated {
        items.push(text_card("truncated", "Showing part of the stock", "There are more open layers than this board reads at once; the figures cover the first 20,000."));
    }
    Ok(json!({
        "title": "Inventory overview", "description": format!("{} · {} to {}", d.currency, day(from), day(to)), "refresh_secs": 300,
        "filters": [ { "name": "period", "label": "Period (stock in and out)", "kind": "period", "value": input.filters.as_ref().and_then(|f| f.get("period")).cloned().unwrap_or(json!({ "from": day(from), "to": day(to) })) }, d.company_filter ],
        "items": items,
    }))
}

/// The stock behind a warehouse bar, a category slice or an age band: item by warehouse, with quantity and value.
fn drill(input: DrillInput) -> Result<Value> {
    require_reader()?;
    let today_d = aether_sdk::dates::parse_date(&today()?)?;
    let d = load(input.filters.as_ref())?;
    let segment = input.segment.unwrap_or(Value::Null);
    let wanted = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);
    let mut layers: Vec<&Held> = d.held.iter().collect();
    match input.chart.as_deref() {
        Some("warehouse") => {
            if let Some(w) = wanted("warehouse") {
                layers.retain(|h| h.warehouse == w);
            }
        }
        Some("category") => {
            if let Some(c) = wanted("category") {
                layers.retain(|h| category_of(&d, &h.item) == c);
            }
        }
        Some("age") => {
            if let Some(b) = wanted("age").and_then(|b| AGE_BANDS.iter().position(|x| *x == b)) {
                layers.retain(|h| age_band(h.opened.map_or(0, |o| (today_d - o).num_days())) == b);
            }
        }
        _ => {}
    }
    let mut grouped: BTreeMap<(String, String), (f64, f64)> = BTreeMap::new();
    for h in layers {
        let e = grouped.entry((h.item.clone(), h.warehouse.clone())).or_default();
        e.0 += h.qty;
        e.1 += h.value;
    }
    let mut rows: Vec<((String, String), (f64, f64))> = grouped.into_iter().collect();
    rows.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap_or(std::cmp::Ordering::Equal));
    let total = rows.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let page: Vec<Value> = rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|((item, warehouse), (qty, value))| json!({ "id": item, "cells": {
            "item": item_name(&d, item), "warehouse": name_in(&d.warehouses, warehouse), "qty": format!("{qty}"), "value": format!("{value:.2}"),
        }}))
        .collect();
    Ok(json!({
        "columns": [ {"key":"item","label":"Item"}, {"key":"warehouse","label":"Warehouse"}, {"key":"qty","label":"On hand"}, {"key":"value","label":"Value"} ],
        "rows": page, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The inventory overview board for a company and period.
    fn inventory_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The stock behind one bar, slice or band of the inventory overview.
    fn inventory_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
