//! Pick lists: what to take from which bin for a sales order, then confirm what was picked.
//!
//! Selling is an optional dependency: a pick list can also be created by hand (`lines`). The
//! `workflow` in `rules/pick_list.json` decides who may release, confirm or cancel; the number
//! is a kernel series that numbers the list when it is released, per company.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::bins::{bin_balances, ensure_bins_match, move_out_for_pick};
use crate::common::{id_of, qty_of_value, require, require_reader, require_user, text, Record, QTY_SCALE};

#[derive(Deserialize)]
struct ManualLine {
    item: String,
    qty: Decimal,
    #[serde(default)]
    bin: Option<String>,
    #[serde(default)]
    lot: Option<String>,
}

#[derive(Deserialize)]
struct CreatePick {
    company: String,
    warehouse: String,
    /// A sales order to pick for (needs the selling plugin).
    #[serde(default)]
    so: Option<String>,
    #[serde(default)]
    lines: Vec<ManualLine>,
}

#[derive(Deserialize)]
struct Id {
    #[serde(alias = "id")]
    pick: String,
}

#[derive(Deserialize)]
struct PickedLine {
    line: String,
    qty_picked: Decimal,
}

#[derive(Deserialize)]
struct ConfirmPick {
    #[serde(alias = "id")]
    pick: String,
    /// Default: every line picked in full.
    #[serde(default)]
    lines: Vec<PickedLine>,
}

#[derive(Deserialize, Default)]
struct ListFilter {
    #[serde(default)]
    state: Option<String>,
}

/// Lines for the open quantity of a sales order, taken from the bins that hold the item, the
/// fullest bin first; whatever the bins cannot cover is a line without a bin.
fn lines_for_so(warehouse: &str, so_id: &str) -> Result<(Vec<Value>, String)> {
    if !plugins::available("erp_selling")? {
        return Err(Error::msg("the selling plugin is not installed; list the lines by hand"));
    }
    let order: Value = plugins::call("erp_selling", "get_so", &json!({ "id": so_id }))?;
    let lines = order.get("lines").and_then(Value::as_array).cloned().unwrap_or_default();
    let company = order
        .get("so")
        .and_then(|so| text(so, "company"))
        .unwrap_or_default()
        .to_string();
    let mut out = Vec::new();
    for line in lines {
        let item = match text(&line, "item") {
            Some(i) => i.to_string(),
            None => continue,
        };
        let mut open = qty_of_value(&line, "qty")?.checked_sub(qty_of_value(&line, "qty_delivered")?)?;
        if open.is_zero() || open.is_negative() {
            continue;
        }
        let mut bins = bin_balances(warehouse, &item)?;
        bins.retain(|(_, q)| !q.is_zero() && !q.is_negative());
        bins.sort_by(|a, b| b.1.cmp(&a.1));
        for (bin, have) in bins {
            if open.is_zero() {
                break;
            }
            let take = if have < open { have } else { open };
            out.push(json!({ "item": item, "bin": bin, "qty": take }));
            open = open.checked_sub(take)?;
        }
        if !open.is_zero() {
            out.push(json!({ "item": item, "qty": open }));
        }
    }
    Ok((out, company))
}

fn do_create_pick(input: CreatePick) -> Result<Record> {
    require_user()?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let warehouse_id = id_of(&warehouse)?.to_string();
    let mut lines: Vec<Value> = Vec::new();
    let mut data = json!({ "company": input.company, "warehouse": warehouse_id, "state": "draft" });
    if let Some(so) = input.so.filter(|s| !s.is_empty()) {
        let (rows, _) = lines_for_so(&warehouse_id, &so)?;
        lines.extend(rows);
        data["source_kind"] = json!("erp_selling.so");
        data["source_id"] = json!(so);
    }
    for line in &input.lines {
        let qty = line.qty.clone().with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("quantity must be greater than zero"));
        }
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": line.item }))?;
        let mut row = json!({ "item": id_of(&item)?, "qty": qty });
        if let Some(bin) = line.bin.as_ref().filter(|b| !b.is_empty()) {
            row["bin"] = json!(bin);
        }
        if let Some(lot) = line.lot.as_ref().filter(|l| !l.is_empty()) {
            row["lot"] = json!(lot);
        }
        lines.push(row);
    }
    if lines.is_empty() {
        return Err(Error::msg("there is nothing to pick"));
    }
    data["lines"] = json!(lines);
    db::create("pick_list", &data).map_err(|e| e.or("could not create the pick list"))
}

/// A state change is a plain write: the kernel's workflow says whether this caller may make it.
fn move_to(pick: &str, state: &str) -> Result<Record> {
    require_user()?;
    db::update("pick_list", pick, &json!({ "state": state }))?.ok_or_else(|| Error::msg("the pick list is gone"))
}

fn do_confirm(input: ConfirmPick) -> Result<Record> {
    require_user()?;
    let pick: Record = db::get_expanded("pick_list", &input.pick, &["lines"])?
        .ok_or_else(|| Error::msg("no such pick list"))?;
    if text(&pick, "state") != Some("released") {
        return Err(Error::msg("only a released pick list can be confirmed"));
    }
    let lines = pick.get("lines").and_then(Value::as_array).cloned().unwrap_or_default();
    let company = text(&pick, "company").unwrap_or_default().to_string();
    let warehouse = text(&pick, "warehouse").unwrap_or_default().to_string();
    let items: Vec<String> = lines.iter().filter_map(|l| text(l, "item").map(str::to_string)).collect();
    ensure_bins_match(&company, &warehouse, &items)?;
    for line in &lines {
        let line_id = text(line, "id").unwrap_or_default();
        let planned = qty_of_value(line, "qty")?;
        let picked = match input.lines.iter().find(|l| l.line == line_id) {
            Some(l) => l.qty_picked.clone().with_scale(QTY_SCALE)?,
            None if input.lines.is_empty() => planned.clone(),
            None => Decimal::zero(QTY_SCALE),
        };
        if picked.is_negative() || picked > planned {
            return Err(Error::msg("a picked quantity is more than the line asks for"));
        }
        if picked.is_zero() {
            continue;
        }
        let _: Option<Record> = db::update("pick_line", line_id, &json!({ "qty_picked": picked }))?;
        if let Some(bin) = text(line, "bin") {
            move_out_for_pick(
                &company,
                &warehouse,
                text(line, "item").unwrap_or_default(),
                bin,
                picked,
                text(line, "lot"),
                &input.pick,
            )?;
        }
    }
    move_to(&input.pick, "picked")
}

fn do_list(input: ListFilter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("pick_list").limit(500);
    if let Some(s) = input.state.filter(|s| !s.is_empty()) {
        q = q.filter("state", s.as_str());
    }
    let mut rows = q.all()?;
    rows.sort_by(|a, b| text(b, "number").cmp(&text(a, "number")));
    Ok(rows)
}

fn do_list_lines(input: Id) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("pick_line").filter("pick", input.pick.as_str()).limit(500).all()?;
    rows.sort_by_key(|r| r.get("position").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

handler! {
    fn create_pick_list(input: CreatePick) -> Record { do_create_pick(input) }
    fn release_pick_list(input: Id) -> Record { move_to(&input.pick, "released") }
    fn cancel_pick_list(input: Id) -> Record { move_to(&input.pick, "cancelled") }
    fn confirm_pick(input: ConfirmPick) -> Record { do_confirm(input) }
    fn list_pick_lists(input: ListFilter) -> Vec<Record> { do_list(input) }
    fn list_pick_lines(input: Id) -> Vec<Record> { do_list_lines(input) }
}
