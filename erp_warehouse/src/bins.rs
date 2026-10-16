//! Bins, putaway rules and the append-only ledger of movements between bins.
//!
//! On hand in a bin is the sum of what came in (`to_bin`) minus what went out (`from_bin`), worked
//! out by the database; nothing is edited in place. The stock ledger and its valuation stay in
//! `erp_inventory`: a bin says where stock sits, not what it is worth.

use aether_sdk::db::Figure;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{dec, id_of, require, require_admin, require_reader, require_user, text, today, Record, QTY_SCALE};

const KINDS: &[&str] = &["storage", "receiving", "shipping", "quarantine"];

#[derive(Deserialize)]
struct CreateBin {
    warehouse: String,
    code: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct SetBin {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize, Default)]
struct WarehouseFilter {
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct CreateRule {
    warehouse: String,
    bin: String,
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    priority: Option<i64>,
}

#[derive(Deserialize)]
struct Suggest {
    warehouse: String,
    item: String,
}

#[derive(Deserialize)]
struct PutAway {
    company: String,
    warehouse: String,
    item: String,
    qty: Decimal,
    #[serde(default)]
    lot: Option<String>,
    /// Default: the bin the putaway rules suggest.
    #[serde(default)]
    bin: Option<String>,
}

#[derive(Deserialize)]
struct Transfer {
    company: String,
    warehouse: String,
    item: String,
    qty: Decimal,
    from_bin: String,
    to_bin: String,
    #[serde(default)]
    lot: Option<String>,
}

#[derive(Deserialize)]
struct StockQuery {
    warehouse: String,
    #[serde(default)]
    item: Option<String>,
}

fn do_create_bin(input: CreateBin) -> Result<Record> {
    require_admin()?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let kind = input.kind.as_deref().unwrap_or("storage");
    if !KINDS.contains(&kind) {
        return Err(Error::msg("kind is storage, receiving, shipping or quarantine"));
    }
    let mut data = json!({
        "warehouse": id_of(&warehouse)?,
        "code": input.code.trim(),
        "kind": kind,
        "active": true,
    });
    if let Some(name) = input.name.filter(|n| !n.trim().is_empty()) {
        data["name"] = json!(name.trim());
    }
    if let Some(parent) = input.parent.filter(|p| !p.is_empty()) {
        let parent = require("wh_bin", &parent, "parent bin")?;
        if text(&parent, "warehouse") != Some(id_of(&warehouse)?) {
            return Err(Error::msg("the parent bin is in another warehouse"));
        }
        data["parent"] = json!(id_of(&parent)?);
    }
    db::create("wh_bin", &data).map_err(|e| e.or("could not create the bin (the code may be taken in this warehouse)"))
}

fn do_set_bin(input: SetBin) -> Result<Record> {
    require_admin()?;
    let _ = require("wh_bin", &input.id, "bin")?;
    let mut changes = json!({});
    if let Some(name) = input.name {
        changes["name"] = json!(name.trim());
    }
    if let Some(kind) = input.kind {
        if !KINDS.contains(&kind.as_str()) {
            return Err(Error::msg("kind is storage, receiving, shipping or quarantine"));
        }
        changes["kind"] = json!(kind);
    }
    if let Some(active) = input.active {
        changes["active"] = json!(active);
    }
    db::update("wh_bin", &input.id, &changes)?.ok_or_else(|| Error::msg("the bin is gone"))
}

fn do_list_bins(input: WarehouseFilter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("wh_bin").limit(1000);
    if let Some(w) = input.warehouse.filter(|w| !w.is_empty()) {
        q = q.filter("warehouse", w.as_str());
    }
    let mut rows = q.all()?;
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}

fn do_create_rule(input: CreateRule) -> Result<Record> {
    require_admin()?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let bin = require("wh_bin", &input.bin, "bin")?;
    if text(&bin, "warehouse") != Some(id_of(&warehouse)?) {
        return Err(Error::msg("the bin is in another warehouse"));
    }
    let item = input.item.filter(|i| !i.is_empty());
    let category = input.category.filter(|c| !c.is_empty());
    if item.is_none() && category.is_none() {
        return Err(Error::msg("name an item or a category"));
    }
    let mut data = json!({
        "warehouse": id_of(&warehouse)?,
        "bin": id_of(&bin)?,
        "priority": input.priority.unwrap_or(10),
        "active": true,
    });
    if let Some(item) = item {
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": item }))?;
        data["item"] = json!(id_of(&item)?);
    }
    if let Some(category) = category {
        data["category"] = json!(category);
    }
    db::create("putaway_rule", &data).map_err(|e| e.or("could not save the rule"))
}

fn do_list_rules(input: WarehouseFilter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("putaway_rule").limit(500);
    if let Some(w) = input.warehouse.filter(|w| !w.is_empty()) {
        q = q.filter("warehouse", w.as_str());
    }
    let mut rows = q.all()?;
    rows.sort_by_key(|r| r.get("priority").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

/// The bin a received item should go to: the lowest-priority-number rule for the item itself,
/// else for its category. Inactive rules and bins are skipped.
fn suggested_bin(warehouse: &str, item: &Record) -> Result<Option<String>> {
    let item_id = id_of(item)?;
    let category = text(item, "category").map(str::to_string);
    let rules: Vec<Record> = db::find("putaway_rule")
        .filter("warehouse", warehouse)
        .filter("active", true)
        .limit(500)
        .all()?;
    let mut best: Option<(u8, i64, String)> = None;
    for rule in &rules {
        let rank = if text(rule, "item") == Some(item_id) {
            0
        } else if text(rule, "item").is_none() && category.is_some() && text(rule, "category") == category.as_deref() {
            1
        } else {
            continue;
        };
        let bin = match text(rule, "bin") {
            Some(b) => b,
            None => continue,
        };
        let active = db::get::<Record>("wh_bin", bin)?.map_or(false, |b| b.get("active") != Some(&json!(false)));
        if !active {
            continue;
        }
        let priority = rule.get("priority").and_then(Value::as_i64).unwrap_or(10);
        if best.as_ref().map_or(true, |(r, p, _)| (rank, priority) < (*r, *p)) {
            best = Some((rank, priority, bin.to_string()));
        }
    }
    Ok(best.map(|(_, _, bin)| bin))
}

fn do_suggest_bin(input: Suggest) -> Result<Value> {
    require_reader()?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    Ok(json!({ "bin": suggested_bin(id_of(&warehouse)?, &item)? }))
}

/// Net quantity per bin of an item in a warehouse: what entered minus what left.
pub(crate) fn bin_balances(warehouse: &str, item: &str) -> Result<Vec<(String, Decimal)>> {
    let mut net: Vec<(String, Decimal)> = Vec::new();
    for (column, sign) in [("to_bin", 1_i64), ("from_bin", -1_i64)] {
        let rows = db::find::<Record>("bin_move")
            .filter("warehouse", warehouse)
            .filter("item", item)
            .aggregate(&[column], &[("qty", Figure::sum("qty"))])?;
        for row in rows {
            let Some(bin) = row.get(column).and_then(Value::as_str) else { continue };
            let qty = dec(&row, "qty")?;
            let signed = if sign > 0 { qty } else { Decimal::zero(QTY_SCALE).checked_sub(qty)? };
            match net.iter_mut().find(|(b, _)| b == bin) {
                Some((_, total)) => *total = total.checked_add(signed)?,
                None => net.push((bin.to_string(), signed)),
            }
        }
    }
    Ok(net)
}

fn lot_filter(q: db::Find<Record>, lot: &Option<String>) -> db::Find<Record> {
    match lot.as_deref().filter(|l| !l.is_empty()) {
        Some(l) => q.filter("lot", l),
        None => q,
    }
}

fn bin_qty_of_lot(bin: &str, item: &str, lot: &Option<String>) -> Result<Decimal> {
    let mut total = Decimal::zero(QTY_SCALE);
    for (column, sign) in [("to_bin", 1_i64), ("from_bin", -1_i64)] {
        let q = db::find::<Record>("bin_move").filter(column, bin).filter("item", item);
        let rows = lot_filter(q, lot).aggregate(&[], &[("qty", Figure::sum("qty"))])?;
        let qty = rows.first().map(|r| dec(r, "qty")).transpose()?.unwrap_or_else(|| Decimal::zero(QTY_SCALE));
        total = if sign > 0 { total.checked_add(qty)? } else { total.checked_sub(qty)? };
    }
    Ok(total)
}

fn positive(qty: Decimal) -> Result<Decimal> {
    let qty = qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    Ok(qty)
}

fn write_move(data: Value) -> Result<Record> {
    db::create("bin_move", &data).map_err(|e| e.or("could not record the movement"))
}

fn do_put_away(input: PutAway) -> Result<Record> {
    require_user()?;
    let qty = positive(input.qty)?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let bin_id = match input.bin.filter(|b| !b.is_empty()) {
        Some(b) => b,
        None => suggested_bin(id_of(&warehouse)?, &item)?
            .ok_or_else(|| Error::msg("no putaway rule matches this item; name a bin"))?,
    };
    let bin = require("wh_bin", &bin_id, "bin")?;
    if text(&bin, "warehouse") != Some(id_of(&warehouse)?) {
        return Err(Error::msg("the bin is in another warehouse"));
    }
    if bin.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the bin is not active"));
    }
    let mut data = json!({
        "company": input.company,
        "warehouse": id_of(&warehouse)?,
        "item": id_of(&item)?,
        "to_bin": id_of(&bin)?,
        "qty": qty,
        "moved_on": today()?,
        "reason": "putaway",
    });
    if let Some(lot) = input.lot.filter(|l| !l.is_empty()) {
        data["lot"] = json!(lot);
    }
    write_move(data)
}

fn do_transfer(input: Transfer) -> Result<Record> {
    require_user()?;
    let qty = positive(input.qty)?;
    if input.from_bin == input.to_bin {
        return Err(Error::msg("the two bins are the same"));
    }
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    for bin_id in [&input.from_bin, &input.to_bin] {
        let bin = require("wh_bin", bin_id, "bin")?;
        if text(&bin, "warehouse") != Some(id_of(&warehouse)?) {
            return Err(Error::msg("a bin is in another warehouse"));
        }
    }
    let have = bin_qty_of_lot(&input.from_bin, id_of(&item)?, &input.lot)?;
    if have < qty {
        return Err(Error::msg(format!("the source bin holds {have}, not {qty}")));
    }
    let mut data = json!({
        "company": input.company,
        "warehouse": id_of(&warehouse)?,
        "item": id_of(&item)?,
        "from_bin": input.from_bin,
        "to_bin": input.to_bin,
        "qty": qty,
        "moved_on": today()?,
        "reason": "transfer",
    });
    if let Some(lot) = input.lot.filter(|l| !l.is_empty()) {
        data["lot"] = json!(lot);
    }
    write_move(data)
}

fn do_bin_stock(input: StockQuery) -> Result<Value> {
    require_reader()?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let mut rows = Vec::new();
    let items: Vec<String> = match input.item.filter(|i| !i.is_empty()) {
        Some(i) => vec![i],
        None => {
            let seen = db::find::<Record>("bin_move")
                .filter("warehouse", id_of(&warehouse)?)
                .aggregate(&["item"], &[("n", Figure::Count)])?;
            seen.iter().filter_map(|r| r.get("item").and_then(Value::as_str).map(str::to_string)).collect()
        }
    };
    for item in items {
        for (bin, qty) in bin_balances(id_of(&warehouse)?, &item)? {
            if !qty.is_zero() {
                rows.push(json!({ "item": item, "bin": bin, "qty": qty }));
            }
        }
    }
    Ok(json!({ "warehouse": id_of(&warehouse)?, "rows": rows }))
}

pub(crate) fn move_out_for_pick(
    company: &str,
    warehouse: &str,
    item: &str,
    bin: &str,
    qty: Decimal,
    lot: Option<&str>,
    source_id: &str,
) -> Result<Record> {
    let mut data = json!({
        "company": company,
        "warehouse": warehouse,
        "item": item,
        "from_bin": bin,
        "qty": qty,
        "moved_on": today()?,
        "reason": "pick",
        "source_kind": "erp_warehouse.pick_list",
        "source_id": source_id,
    });
    if let Some(l) = lot.filter(|l| !l.is_empty()) {
        data["lot"] = json!(l);
    }
    write_move(data)
}

handler! {
    fn create_bin(input: CreateBin) -> Record { do_create_bin(input) }
    fn set_bin(input: SetBin) -> Record { do_set_bin(input) }
    fn list_bins(input: WarehouseFilter) -> Vec<Record> { do_list_bins(input) }
    fn create_putaway_rule(input: CreateRule) -> Record { do_create_rule(input) }
    fn list_putaway_rules(input: WarehouseFilter) -> Vec<Record> { do_list_rules(input) }
    fn suggest_bin(input: Suggest) -> Value { do_suggest_bin(input) }
    fn put_away(input: PutAway) -> Record { do_put_away(input) }
    fn move_between_bins(input: Transfer) -> Record { do_transfer(input) }
    fn bin_stock(input: StockQuery) -> Value { do_bin_stock(input) }
}
