//! Physical stock counts: record what was counted, then post the differences as adjustments.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_user, text, today, valuation_for, value_of, Record};
use crate::moves::{api_add_line, api_draft, api_post, open_layers};
use crate::rules::{QTY_SCALE, VALUE_SCALE};

#[derive(Deserialize)]
struct CreateCount {
    company: String,
    warehouse: String,
    #[serde(default)]
    count_date: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct CountLine {
    #[serde(alias = "id")]
    count: String,
    item: String,
    #[serde(default)]
    lot: Option<String>,
    counted_qty: Decimal,
    #[serde(default)]
    unit_cost: Option<Decimal>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn do_create_count(input: CreateCount) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    if text(&warehouse, "company") != Some(id_of(&company)?) {
        return Err(Error::msg("the warehouse belongs to another company"));
    }
    let date = match input.count_date.filter(|d| !d.is_empty()) {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let mut data = json!({
        "company": id_of(&company)?,
        "warehouse": id_of(&warehouse)?,
        "count_date": date,
        "state": "draft",
    });
    if let Some(n) = input.note.filter(|n| !n.trim().is_empty()) {
        data["note"] = json!(n.trim());
    }
    db::create("stock_count", &data).map_err(|e| e.or("could not start the count"))
}

fn do_set_count_line(input: CountLine) -> Result<Record> {
    require_user()?;
    let count = require("stock_count", &input.count, "count")?;
    if text(&count, "state") != Some("draft") {
        return Err(Error::msg("only a draft count can be changed"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("is_stocked") == Some(&json!(false)) {
        return Err(Error::msg("the item is not stocked"));
    }
    let counted = input.counted_qty.with_scale(QTY_SCALE)?;
    if counted.is_negative() {
        return Err(Error::msg("a counted quantity is not negative"));
    }
    let tracking = text(&item, "tracking").unwrap_or("none");
    let lot = input.lot.as_deref().map(str::trim).filter(|l| !l.is_empty());
    match (tracking, lot) {
        ("none", Some(_)) => return Err(Error::msg("this item does not use lots")),
        ("lot" | "serial", None) => return Err(Error::msg("count this item per lot")),
        ("serial", Some(_)) if counted > Decimal::whole(1, QTY_SCALE)? => {
            return Err(Error::msg("a serial number is counted as 0 or 1"));
        }
        _ => {}
    }
    let mut data = json!({
        "count": input.count,
        "item": id_of(&item)?,
        "counted_qty": counted,
    });
    if let Some(l) = lot {
        data["lot"] = json!(l);
    }
    if let Some(cost) = input.unit_cost {
        let cost = cost.with_scale(VALUE_SCALE)?;
        if cost.is_negative() {
            return Err(Error::msg("unit cost is not negative"));
        }
        data["unit_cost"] = json!(cost);
    }
    let existing: Vec<Record> = db::find("stock_count_line").filter("count", input.count.as_str()).limit(2000).all()?;
    let same = existing
        .iter()
        .find(|r| text(r, "item") == Some(id_of(&item).unwrap_or_default()) && text(r, "lot") == lot);
    match same {
        Some(row) => db::update("stock_count_line", id_of(row)?, &data)?.ok_or_else(|| Error::msg("the line is gone")),
        None => db::create("stock_count_line", &data).map_err(|e| e.or("could not save the count line")),
    }
}

fn on_hand_qty(company: &str, item: &str, warehouse: &str, lot: Option<&str>) -> Result<Decimal> {
    let layers = open_layers(company, item, warehouse, lot)?;
    layers
        .iter()
        .try_fold(Decimal::zero(QTY_SCALE), |acc, (_, l)| acc.checked_add(l.remaining_qty))
}

/// Cost to book a counted gain: the line's own, else the newest layer's, else the standard cost.
fn gain_cost(line: &Record, company: &str, item: &str, warehouse: &str, date: &str) -> Result<Decimal> {
    if let Some(c) = line.get("unit_cost").filter(|v| !v.is_null()) {
        let cost: Decimal = serde_json::from_value(c.clone())?;
        return cost.with_scale(VALUE_SCALE);
    }
    let layers: Vec<Record> = db::find("stock_layer")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .limit(2000)
        .all()?;
    let newest = layers
        .iter()
        .max_by_key(|l| l.get("seq").and_then(Value::as_i64).unwrap_or(0));
    if let Some(layer) = newest {
        let cost = value_of(layer, "unit_cost")?;
        if !cost.is_zero() {
            return Ok(cost);
        }
    }
    let (method, standard) = valuation_for(item, company, date)?;
    if method == "standard" && !standard.is_zero() {
        return Ok(standard);
    }
    Err(Error::msg("a counted gain needs a unit cost: this item has no stock history to take one from"))
}

/// Compare counted with system quantities and post the differences: gains as one adjustment, losses as another.
fn do_post_count(input: Id) -> Result<Value> {
    require_user()?;
    let count = require("stock_count", &input.id, "count")?;
    if text(&count, "state") != Some("draft") {
        return Err(Error::msg("only a draft count can be posted"));
    }
    let company = text(&count, "company").unwrap_or_default().to_string();
    let warehouse = text(&count, "warehouse").unwrap_or_default().to_string();
    let date = text(&count, "count_date").unwrap_or_default().to_string();
    let lines: Vec<Record> = db::find("stock_count_line").filter("count", input.id.as_str()).limit(2000).all()?;
    if lines.is_empty() {
        return Err(Error::msg("count something before posting"));
    }

    struct Diff {
        line_id: String,
        item: String,
        lot: Option<String>,
        system: Decimal,
        diff: Decimal,
        cost: Option<Decimal>,
    }
    let mut diffs = Vec::new();
    for line in &lines {
        let item = text(line, "item").unwrap_or_default().to_string();
        let lot = text(line, "lot").filter(|l| !l.is_empty()).map(str::to_string);
        let system = on_hand_qty(&company, &item, &warehouse, lot.as_deref())?;
        let counted = qty_of(line, "counted_qty")?;
        let diff = counted.checked_sub(system)?;
        let cost = if diff > Decimal::zero(QTY_SCALE) { Some(gain_cost(line, &company, &item, &warehouse, &date)?) } else { None };
        diffs.push(Diff { line_id: id_of(line)?.to_string(), item, lot, system, diff, cost });
    }

    let make_move = |label: &str, rows: Vec<&Diff>, increase: bool| -> Result<Option<String>> {
        if rows.is_empty() {
            return Ok(None);
        }
        let mv = api_draft(
            "adjust",
            json!({
                "company": company,
                "posting_date": date,
                "source_kind": "erp_inventory.count",
                "source_id": input.id,
                "idem_key": format!("count:{}:{label}", input.id),
                "reason": format!("Stock count {date}"),
            }),
        )?;
        let move_id = id_of(&mv)?.to_string();
        for row in rows {
            let qty = if increase { row.diff } else { Decimal::zero(QTY_SCALE).checked_sub(row.diff)? };
            let mut data = json!({
                "move": move_id,
                "item": row.item,
                "qty": qty,
                "warehouse": warehouse,
                "increase": increase,
            });
            if let Some(l) = &row.lot {
                data["lot"] = json!(l);
            }
            if let Some(c) = row.cost {
                data["unit_cost"] = json!(c);
            }
            api_add_line(data)?;
        }
        api_post(&move_id)?;
        Ok(Some(move_id))
    };

    let ups: Vec<&Diff> = diffs.iter().filter(|d| d.diff > Decimal::zero(QTY_SCALE)).collect();
    let downs: Vec<&Diff> = diffs.iter().filter(|d| d.diff.is_negative()).collect();
    let move_up = make_move("up", ups, true)?;
    let move_down = make_move("down", downs, false)?;

    let mut tx = db::transaction();
    for d in &diffs {
        tx = tx.update("stock_count_line", &d.line_id, &json!({ "system_qty": d.system, "difference": d.diff }));
    }
    let mut header = json!({ "state": "posted" });
    if let Some(m) = &move_up {
        header["move_up"] = json!(m);
    }
    if let Some(m) = &move_down {
        header["move_down"] = json!(m);
    }
    tx.update("stock_count", &input.id, &header).run().map_err(|e| e.or("could not close the count"))?;
    Ok(json!({
        "count": require("stock_count", &input.id, "count")?,
        "gains": move_up,
        "losses": move_down,
        "lines": diffs.len(),
        "unchanged": diffs.iter().filter(|d| d.diff.is_zero()).count(),
    }))
}

fn do_cancel_count(input: Id) -> Result<Record> {
    require_user()?;
    let count = require("stock_count", &input.id, "count")?;
    if text(&count, "state") != Some("draft") {
        return Err(Error::msg("only a draft count can be cancelled"));
    }
    db::update("stock_count", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the count is gone"))
}

handler! {
    fn create_count(input: CreateCount) -> Record { do_create_count(input) }
    fn set_count_line(input: CountLine) -> Record { do_set_count_line(input) }
    fn post_count(input: Id) -> Value { do_post_count(input) }
    fn cancel_count(input: Id) -> Record { do_cancel_count(input) }
}
