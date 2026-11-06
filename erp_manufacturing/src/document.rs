//! The manufacturing order as one document: header edit, materials with stock cover, operations and time.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{require, require_reader, require_user, text, Record, QTY_SCALE};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct UpdateMo {
    id: String,
    #[serde(default)]
    qty: Option<Decimal>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    planned_date: Option<String>,
}

fn do_get_mo_doc(input: Id) -> Result<Record> {
    require_reader()?;
    require("mo", &input.id, "manufacturing order")
}

/// Only a draft order is edited: confirming freezes the materials and operations at the quantity then.
fn do_update_mo(input: UpdateMo) -> Result<Record> {
    require_user()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    if text(&mo, "state") != Some("draft") {
        return Err(Error::msg(format!(
            "an order can only be changed while it is a draft (state: {})",
            text(&mo, "state").unwrap_or("unknown")
        )));
    }
    let mut data = json!({});
    if let Some(q) = input.qty {
        let q = q.with_scale(QTY_SCALE)?;
        if q.is_zero() || q.is_negative() {
            return Err(Error::msg("order quantity must be greater than zero"));
        }
        data["qty"] = json!(q);
    }
    if let Some(wh) = input.warehouse.filter(|w| !w.is_empty()) {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    if let Some(d) = input.planned_date.filter(|d| !d.is_empty()) {
        parse_date(&d)?;
        data["planned_date"] = json!(d);
    }
    db::update("mo", &input.id, &data)?.ok_or_else(|| Error::msg("the order is gone"))
}

fn item_name(item: &str) -> String {
    db::get::<Record>("erp_item.item", item)
        .ok()
        .flatten()
        .and_then(|i| text(&i, "name").map(str::to_string))
        .unwrap_or_else(|| item.to_string())
}

/// What the order needs (or makes as a byproduct) at its quantity, with stock cover while it is still open.
fn do_list_mo_materials(input: Id) -> Result<Vec<Value>> {
    require_reader()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    let state = text(&mo, "state").unwrap_or("draft").to_string();
    let open = matches!(state.as_str(), "draft" | "confirmed" | "reserved");
    let company = text(&mo, "company").unwrap_or_default().to_string();
    let warehouse = text(&mo, "warehouse").unwrap_or_default().to_string();
    let mut out = Vec::new();
    for (i, line) in crate::orders::exploded_for_mo(&mo)?.iter().enumerate() {
        let mut row = json!({
            "id": format!("{}:{}", input.id, i + 1),
            "line_no": i + 1,
            "item": line.item_id,
            "item_name": item_name(&line.item_id),
            "kind": if line.is_byproduct { "byproduct" } else { "material" },
            "qty": line.qty,
        });
        if open && !line.is_byproduct {
            let stock: Value = plugins::call(
                "erp_inventory",
                "on_hand",
                &json!({ "company": company, "item": line.item_id, "warehouse": warehouse }),
            )?;
            let on_hand = crate::common::dec(&stock, "qty", QTY_SCALE)?;
            let available = crate::common::dec(&stock, "available", QTY_SCALE)?;
            // Once the order holds its materials, its own hold is already out of `available`.
            let cover = if state == "reserved" { on_hand } else { available };
            row["on_hand"] = json!(on_hand);
            row["available"] = json!(available);
            row["cover"] = json!(if cover < line.qty { "short" } else { "ok" });
        }
        out.push(row);
    }
    Ok(out)
}

fn do_list_child_mos(input: Id) -> Result<Vec<Record>> {
    require_reader()?;
    db::find("mo").filter("parent_mo", input.id.as_str()).limit(200).all()
}

/// Operations that are running, for the log-time picker.
fn do_list_open_operations(input: Id) -> Result<Vec<Value>> {
    require_reader()?;
    Ok(crate::routing::mo_operations(&input.id)?
        .iter()
        .filter(|o| text(o, "state") == Some("in_progress"))
        .map(|o| json!({ "id": o.get("id"), "display_name": format!("{} {}", o.get("seq").and_then(Value::as_i64).unwrap_or(0), text(o, "name").unwrap_or("")) }))
        .collect())
}

fn do_get_mo_operation(input: Id) -> Result<Value> {
    require_reader()?;
    let op = require("mo_operation", &input.id, "operation")?;
    Ok(json!({ "id": op.get("id"), "operation": op.get("id") }))
}

#[derive(Deserialize)]
struct LogOperationTime {
    id: String,
    minutes: Decimal,
    #[serde(default)]
    logged_on: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

fn do_log_operation_time(input: LogOperationTime) -> Result<Record> {
    require_user()?;
    crate::routing::do_log_time(crate::routing::LogTime {
        operation: input.id,
        minutes: input.minutes,
        logged_on: input.logged_on,
        note: input.note,
    })
}

handler! {
    fn get_mo_doc(input: Id) -> Record { do_get_mo_doc(input) }
    fn update_mo(input: UpdateMo) -> Record { do_update_mo(input) }
    fn list_mo_materials(input: Id) -> Vec<Value> { do_list_mo_materials(input) }
    fn list_child_mos(input: Id) -> Vec<Record> { do_list_child_mos(input) }
    fn list_open_operations(input: Id) -> Vec<Value> { do_list_open_operations(input) }
    fn get_mo_operation(input: Id) -> Value { do_get_mo_operation(input) }
    fn log_operation_time(input: LogOperationTime) -> Record { do_log_operation_time(input) }
}
