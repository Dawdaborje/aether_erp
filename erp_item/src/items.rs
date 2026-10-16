//! Categories, items and per-company valuation settings.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, pick, require, require_admin, require_reader, text, today, Record, METHODS, TRACKING};

#[derive(Deserialize)]
struct CategoryInput {
    code: String,
    name: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    valuation_method: Option<String>,
}

#[derive(Deserialize)]
struct SetCategory {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    valuation_method: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct ItemInput {
    code: String,
    name: String,
    uom: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tracking: Option<String>,
    #[serde(default)]
    is_stocked: Option<bool>,
    #[serde(default)]
    expiry_days: Option<i64>,
}

#[derive(Deserialize)]
struct SetItem {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    uom: Option<String>,
    #[serde(default)]
    tracking: Option<String>,
    #[serde(default)]
    is_stocked: Option<bool>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    expiry_days: Option<i64>,
}

#[derive(Deserialize)]
struct ItemCompanyInput {
    item: String,
    company: String,
    valuation_method: String,
    #[serde(default)]
    standard_cost: Option<Decimal>,
    /// When setting a standard cost, the date it becomes effective (default: today).
    #[serde(default)]
    effective_date: Option<String>,
}

#[derive(Deserialize)]
struct SetStandardCost {
    item: String,
    company: String,
    rate: Decimal,
    #[serde(default)]
    effective_date: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    active_only: Option<bool>,
}

fn method_or(value: Option<&str>, fallback: &str) -> Result<String> {
    let m = value.unwrap_or(fallback);
    if METHODS.contains(&m) {
        Ok(m.to_string())
    } else {
        Err(Error::msg("valuation_method is fifo, average or standard"))
    }
}

fn do_create_category(input: CategoryInput) -> Result<Record> {
    require_admin()?;
    let method = method_or(input.valuation_method.as_deref(), "fifo")?;
    let mut data = json!({
        "code": input.code.trim(),
        "name": input.name.trim(),
        "valuation_method": method,
        "active": true,
    });
    if let Some(parent_id) = input.parent {
        let parent = require("item_category", &parent_id, "category")?;
        if parent.get("active") == Some(&json!(false)) {
            return Err(Error::msg("the parent category is not active"));
        }
        data["parent"] = json!(id_of(&parent)?);
    }
    db::create("item_category", &data).map_err(|e| e.or("could not create the category (the code may be taken)"))
}

fn do_set_category(input: SetCategory) -> Result<Record> {
    require_admin()?;
    let _ = require("item_category", &input.id, "category")?;
    let mut changes = json!({});
    if let Some(name) = input.name {
        changes["name"] = json!(name.trim());
    }
    if let Some(method) = input.valuation_method {
        changes["valuation_method"] = json!(method_or(Some(&method), "fifo")?);
    }
    if let Some(active) = input.active {
        changes["active"] = json!(active);
    }
    if let Some(parent_id) = input.parent {
        if parent_id.is_empty() {
            changes["parent"] = Value::Null;
        } else {
            if parent_id == input.id {
                return Err(Error::msg("a category cannot be its own parent"));
            }
            let parent = require("item_category", &parent_id, "category")?;
            changes["parent"] = json!(id_of(&parent)?);
        }
    }
    db::update("item_category", &input.id, &changes)?.ok_or_else(|| Error::msg("the category is gone"))
}

fn do_create_item(input: ItemInput) -> Result<Record> {
    require_admin()?;
    let tracking = input.tracking.as_deref().unwrap_or("none");
    if !TRACKING.contains(&tracking) {
        return Err(Error::msg("tracking is none, lot or serial"));
    }
    let uom = require("uom.uom", &input.uom, "unit")?;
    let mut data = json!({
        "code": input.code.trim(),
        "name": input.name.trim(),
        "uom": id_of(&uom)?,
        "tracking": tracking,
        "is_stocked": input.is_stocked.unwrap_or(true),
        "active": true,
    });
    if let Some(days) = input.expiry_days {
        if days <= 0 {
            return Err(Error::msg("shelf life must be a positive number of days"));
        }
        if tracking == "none" {
            return Err(Error::msg("shelf life needs a lot or serial tracked item"));
        }
        data["expiry_days"] = json!(days);
    }
    if let Some(category_id) = input.category {
        let category = require("item_category", &category_id, "category")?;
        data["category"] = json!(id_of(&category)?);
    }
    db::create("item", &data).map_err(|e| e.or("could not create the item (the code may be taken)"))
}

fn do_set_item(input: SetItem) -> Result<Record> {
    require_admin()?;
    let _ = require("item", &input.id, "item")?;
    let mut changes = json!({});
    if let Some(name) = input.name {
        changes["name"] = json!(name.trim());
    }
    if let Some(category_id) = input.category {
        if category_id.is_empty() {
            changes["category"] = Value::Null;
        } else {
            let category = require("item_category", &category_id, "category")?;
            changes["category"] = json!(id_of(&category)?);
        }
    }
    if let Some(uom_id) = input.uom {
        let uom = require("uom.uom", &uom_id, "unit")?;
        changes["uom"] = json!(id_of(&uom)?);
    }
    if let Some(tracking) = input.tracking {
        if !TRACKING.contains(&tracking.as_str()) {
            return Err(Error::msg("tracking is none, lot or serial"));
        }
        changes["tracking"] = json!(tracking);
    }
    if let Some(is_stocked) = input.is_stocked {
        changes["is_stocked"] = json!(is_stocked);
    }
    if let Some(active) = input.active {
        changes["active"] = json!(active);
    }
    if let Some(days) = input.expiry_days {
        if days < 0 {
            return Err(Error::msg("shelf life must not be negative"));
        }
        changes["expiry_days"] = if days == 0 { Value::Null } else { json!(days) };
    }
    db::update("item", &input.id, &changes)?.ok_or_else(|| Error::msg("the item is gone"))
}

fn append_standard_cost(item_id: &str, company_id: &str, rate: Decimal, effective_date: &str) -> Result<Record> {
    parse_date(effective_date)?;
    if rate.is_zero() || rate.is_negative() {
        return Err(Error::msg("standard cost must be greater than zero"));
    }
    let existing: Option<Record> = db::find("item_standard_cost")
        .filter("item", item_id)
        .filter("company", company_id)
        .filter("effective_date", effective_date)
        .first()?;
    if existing.is_some() {
        return Err(Error::msg("a standard cost already exists for this effective date; add a later date"));
    }
    db::create(
        "item_standard_cost",
        &json!({
            "item": item_id,
            "company": company_id,
            "effective_date": effective_date,
            "rate": rate,
        }),
    )
    .map_err(|e| e.or("could not save the standard cost"))
}

fn rate_as_of(item_id: &str, company_id: &str, as_of: &str) -> Result<Option<Decimal>> {
    let as_of_date = parse_date(as_of)?;
    let rows: Vec<Record> = db::find("item_standard_cost")
        .filter("item", item_id)
        .filter("company", company_id)
        .limit(500)
        .all()?;
    let mut best: Option<(aether_sdk::dates::NaiveDate, Decimal)> = None;
    for row in rows {
        let date = parse_date(text(&row, "effective_date").unwrap_or_default())?;
        if date > as_of_date {
            continue;
        }
        let rate = match row.get("rate") {
            Some(v) if !v.is_null() => {
                let parsed: Decimal = serde_json::from_value(v.clone()).map_err(|e| Error::msg(format!("rate: {e}")))?;
                parsed.with_scale(6)?
            }
            _ => continue,
        };
        match &best {
            Some((d, _)) if *d >= date => {}
            _ => best = Some((date, rate)),
        }
    }
    Ok(best.map(|(_, r)| r))
}

fn do_set_item_company(input: ItemCompanyInput) -> Result<Record> {
    require_admin()?;
    let method = method_or(Some(&input.valuation_method), "fifo")?;
    let item = require("item", &input.item, "item")?;
    let company = require("company.company", &input.company, "company")?;
    let item_id = id_of(&item)?.to_string();
    let company_id = id_of(&company)?.to_string();
    let cost = input.standard_cost.unwrap_or_else(|| Decimal::zero(6)).with_scale(6)?;
    if method == "standard" && (cost.is_negative() || cost.is_zero()) {
        return Err(Error::msg("standard cost must be greater than zero"));
    }
    if method == "standard" && !cost.is_zero() {
        let date = match input.effective_date.filter(|d| !d.is_empty()) {
            Some(d) => d,
            None => today()?,
        };
        let _ = append_standard_cost(&item_id, &company_id, cost, &date)?;
    }
    let existing: Option<Record> = db::find("item_company")
        .filter("item", &item_id)
        .filter("company", &company_id)
        .first()?;
    let data = json!({
        "item": item_id,
        "company": company_id,
        "valuation_method": method,
        "standard_cost": cost,
        "active": true,
    });
    if let Some(row) = existing {
        db::update("item_company", id_of(&row)?, &pick(&data, &["valuation_method", "standard_cost", "active"]))?
            .ok_or_else(|| Error::msg("the company settings are gone"))
    } else {
        db::create("item_company", &data).map_err(|e| e.or("could not save company settings"))
    }
}

fn do_set_standard_cost(input: SetStandardCost) -> Result<Record> {
    require_admin()?;
    let item = require("item", &input.item, "item")?;
    let company = require("company.company", &input.company, "company")?;
    let rate = input.rate.with_scale(6)?;
    let date = match input.effective_date.filter(|d| !d.is_empty()) {
        Some(d) => d,
        None => today()?,
    };
    let row = append_standard_cost(id_of(&item)?, id_of(&company)?, rate, &date)?;
    // Keep denormalised current cost on item_company for display.
    if let Some(ic) = db::find::<Record>("item_company")
        .filter("item", id_of(&item)?)
        .filter("company", id_of(&company)?)
        .first()?
    {
        let _: Option<Record> = db::update("item_company", id_of(&ic)?, &json!({ "standard_cost": rate }))?;
    }
    Ok(row)
}

fn do_get_item(input: Id) -> Result<Record> {
    require_reader()?;
    require("item", &input.id, "item")
}

fn do_search_items(input: Search) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("item").limit(200);
    if input.active_only.unwrap_or(true) {
        q = q.filter("active", true);
    }
    let mut rows = q.all()?;
    if let Some(needle) = input.q {
        let needle = needle.to_lowercase();
        rows.retain(|r| {
            text(r, "code").map(|c| c.to_lowercase().contains(&needle)).unwrap_or(false)
                || text(r, "name").map(|n| n.to_lowercase().contains(&needle)).unwrap_or(false)
        });
    }
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}

#[derive(Deserialize)]
struct ValuationQuery {
    item: String,
    company: String,
    /// Posting date for standard cost lookup (default: today).
    #[serde(default)]
    as_of: Option<String>,
}

/// Valuation method and standard cost for an item in a company (category default if no company row).
fn do_valuation_for(input: ValuationQuery) -> Result<Value> {
    require_reader()?;
    let item = require("item", &input.item, "item")?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let company = require("company.company", &input.company, "company")?;
    let as_of = match input.as_of.filter(|d| !d.is_empty()) {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let item_id = id_of(&item)?.to_string();
    let company_id = id_of(&company)?.to_string();
    let row: Option<Record> = db::find("item_company")
        .filter("item", &item_id)
        .filter("company", &company_id)
        .filter("active", true)
        .first()?;
    let (method, source_base) = if let Some(row) = &row {
        (text(row, "valuation_method").unwrap_or("fifo").to_string(), "item_company")
    } else {
        let mut method = "fifo".to_string();
        if let Some(category_id) = text(&item, "category") {
            if let Ok(category) = require("item_category", category_id, "category") {
                if category.get("active") != Some(&json!(false)) {
                    if let Some(m) = text(&category, "valuation_method") {
                        method = m.to_string();
                    }
                }
            }
        }
        (method, "category_or_default")
    };
    let (standard_cost, source) = if method == "standard" {
        match rate_as_of(&item_id, &company_id, &as_of)? {
            Some(rate) => (rate, "item_standard_cost"),
            None => {
                // Fallback to denormalised scalar for rows created before dated rates.
                let fallback = row
                    .as_ref()
                    .and_then(|r| r.get("standard_cost"))
                    .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                    .unwrap_or_else(|| Decimal::zero(6));
                if fallback.is_zero() {
                    return Err(Error::msg(format!(
                        "no standard cost effective on {as_of} for this item and company"
                    )));
                }
                (fallback.with_scale(6)?, "item_company")
            }
        }
    } else {
        (Decimal::zero(6), source_base)
    };
    Ok(json!({
        "valuation_method": method,
        "standard_cost": standard_cost,
        "as_of": as_of,
        "source": source,
    }))
}

fn do_list_categories(input: Search) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("item_category").limit(500).all()?;
    if let Some(needle) = input.q.map(|q| q.to_lowercase()).filter(|q| !q.is_empty()) {
        rows.retain(|r| {
            text(r, "code").map_or(false, |c| c.to_lowercase().contains(&needle))
                || text(r, "name").map_or(false, |n| n.to_lowercase().contains(&needle))
        });
    }
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}

const SEED_CATEGORIES: &[(&str, &str)] = &[
    ("RAW", "Raw materials"),
    ("FINISHED", "Finished goods"),
    ("TRADING", "Trading goods"),
    ("CONSUMABLE", "Consumables"),
    ("SERVICE", "Services"),
];

/// Starter item categories, each only when its code is free.
fn do_seed() -> Result<Value> {
    let mut added = 0;
    for (code, name) in SEED_CATEGORIES {
        if db::find::<Record>("item_category").filter("code", *code).first()?.is_some() {
            continue;
        }
        db::create::<Record>(
            "item_category",
            &json!({ "code": code, "name": name, "valuation_method": "fifo", "active": true }),
        )?;
        added += 1;
    }
    Ok(json!({ "categories": added }))
}

handler! {
    /// Starter item categories; runs on install and upgrade.
    fn seed_items(_: Empty) -> Value { do_seed() }

    fn list_categories(input: Search) -> Vec<Record> { do_list_categories(input) }
    fn create_category(input: CategoryInput) -> Record { do_create_category(input) }
    fn set_category(input: SetCategory) -> Record { do_set_category(input) }
    fn create_item(input: ItemInput) -> Record { do_create_item(input) }
    fn set_item(input: SetItem) -> Record { do_set_item(input) }
    fn set_item_company(input: ItemCompanyInput) -> Record { do_set_item_company(input) }
    fn set_standard_cost(input: SetStandardCost) -> Record { do_set_standard_cost(input) }
    fn get_item(input: Id) -> Record { do_get_item(input) }
    fn search_items(input: Search) -> Vec<Record> { do_search_items(input) }
    fn valuation_for(input: ValuationQuery) -> Value { do_valuation_for(input) }
}
