//! Work centers, routing operations, time logs and labour absorption.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{dec, id_of, require, require_user, text, today, Record};
use crate::rules::{operation_cost, planned_minutes};

#[derive(Deserialize)]
struct WorkCenterInput {
    company: String,
    code: String,
    name: String,
    #[serde(default)]
    cost_per_hour: Option<Decimal>,
}

#[derive(Deserialize)]
struct SetWorkCenter {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cost_per_hour: Option<Decimal>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct BomOperationInput {
    #[serde(alias = "id")]
    bom: String,
    name: String,
    work_center: String,
    #[serde(default)]
    setup_minutes: Option<Decimal>,
    #[serde(default)]
    cycle_minutes: Option<Decimal>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct LogTime {
    operation: String,
    minutes: Decimal,
    #[serde(default)]
    logged_on: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

fn non_negative(value: Option<Decimal>, what: &str) -> Result<Decimal> {
    let v = value.unwrap_or_else(|| Decimal::zero(2)).with_scale(2)?;
    if v.is_negative() {
        return Err(Error::msg(format!("{what} must not be negative")));
    }
    Ok(v)
}

fn do_create_work_center(input: WorkCenterInput) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let rate = input.cost_per_hour.unwrap_or_else(|| Decimal::zero(4)).with_scale(4)?;
    if rate.is_negative() {
        return Err(Error::msg("the hourly cost must not be negative"));
    }
    db::create(
        "work_center",
        &json!({
            "company": id_of(&company)?,
            "code": input.code.trim().to_uppercase(),
            "name": input.name.trim(),
            "cost_per_hour": rate,
            "active": true,
        }),
    )
    .map_err(|e| e.or("could not create the work center (the code may be taken)"))
}

fn do_set_work_center(input: SetWorkCenter) -> Result<Record> {
    require_user()?;
    let _ = require("work_center", &input.id, "work center")?;
    let mut changes = json!({});
    if let Some(n) = input.name {
        changes["name"] = json!(n.trim());
    }
    if let Some(r) = input.cost_per_hour {
        let r = r.with_scale(4)?;
        if r.is_negative() {
            return Err(Error::msg("the hourly cost must not be negative"));
        }
        changes["cost_per_hour"] = json!(r);
    }
    if let Some(a) = input.active {
        changes["active"] = json!(a);
    }
    db::update("work_center", &input.id, &changes)?.ok_or_else(|| Error::msg("the work center is gone"))
}

fn do_add_bom_operation(input: BomOperationInput) -> Result<Record> {
    require_user()?;
    let bom = require("bom", &input.bom, "BOM")?;
    let wc = require("work_center", &input.work_center, "work center")?;
    if text(&wc, "company") != text(&bom, "company") {
        return Err(Error::msg("the work center belongs to another company"));
    }
    if wc.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the work center is not active"));
    }
    let existing: Vec<Record> = db::find("bom_operation").filter("bom", input.bom.as_str()).limit(200).all()?;
    let seq = existing.iter().map(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 10;
    db::create(
        "bom_operation",
        &json!({
            "bom": input.bom,
            "seq": seq,
            "name": input.name.trim(),
            "work_center": input.work_center,
            "setup_minutes": non_negative(input.setup_minutes, "setup time")?,
            "cycle_minutes": non_negative(input.cycle_minutes, "cycle time")?,
        }),
    )
    .map_err(|e| e.or("could not add the operation"))
}

pub(crate) fn bom_operations(bom_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("bom_operation").filter("bom", bom_id).limit(200).all()?;
    rows.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

/// Copy the BOM's routing onto the order at confirm, with planned minutes and the work center's rate frozen.
pub(crate) fn snapshot_operations(mo_id: &str, bom_id: &str, mo_qty: Decimal, bom_qty: Decimal) -> Result<()> {
    for op in bom_operations(bom_id)? {
        let wc = require("work_center", text(&op, "work_center").unwrap_or_default(), "work center")?;
        let planned = planned_minutes(dec(&op, "setup_minutes", 2)?, dec(&op, "cycle_minutes", 2)?, mo_qty, bom_qty)?;
        let _: Record = db::create(
            "mo_operation",
            &json!({
                "mo": mo_id,
                "seq": op.get("seq"),
                "name": text(&op, "name"),
                "work_center": text(&op, "work_center"),
                "planned_minutes": planned,
                "actual_minutes": Decimal::zero(2),
                "cost_per_hour": dec(&wc, "cost_per_hour", 4)?,
                "state": "pending",
            }),
        )?;
    }
    Ok(())
}

pub(crate) fn mo_operations(mo_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("mo_operation").filter("mo", mo_id).limit(200).all()?;
    rows.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

/// Labour and overhead absorbed by the order's operations. Every operation must be finished; an operation
/// with no logged time is costed at its planned minutes. An order with no routing absorbs nothing.
pub(crate) fn labour_cost(mo_id: &str) -> Result<Decimal> {
    let ops = mo_operations(mo_id)?;
    let mut total = Decimal::zero(6);
    for op in &ops {
        if text(op, "state") != Some("done") {
            return Err(Error::msg(format!(
                "finish operation `{}` before completing the order",
                text(op, "name").unwrap_or("?")
            )));
        }
        let actual = dec(op, "actual_minutes", 2)?;
        let minutes = if actual.is_zero() { dec(op, "planned_minutes", 2)? } else { actual };
        total = total.checked_add(operation_cost(minutes, dec(op, "cost_per_hour", 4)?, 6)?)?;
    }
    Ok(total)
}

fn operation_of_active_mo(id: &str) -> Result<Record> {
    let op = require("mo_operation", id, "operation")?;
    let mo = require("mo", text(&op, "mo").unwrap_or_default(), "manufacturing order")?;
    if !matches!(text(&mo, "state"), Some("confirmed" | "reserved" | "in_progress")) {
        return Err(Error::msg("the order is not open for production"));
    }
    Ok(op)
}

fn do_start_operation(input: Id) -> Result<Record> {
    require_user()?;
    let op = operation_of_active_mo(&input.id)?;
    if text(&op, "state") != Some("pending") {
        return Err(Error::msg("only a pending operation can be started"));
    }
    // Operations run in sequence: everything before this one must be done.
    let mo_id = text(&op, "mo").unwrap_or_default();
    let seq = op.get("seq").and_then(Value::as_i64).unwrap_or(0);
    for earlier in mo_operations(mo_id)? {
        if earlier.get("seq").and_then(Value::as_i64).unwrap_or(0) < seq && text(&earlier, "state") != Some("done") {
            return Err(Error::msg(format!("finish `{}` first", text(&earlier, "name").unwrap_or("the previous operation"))));
        }
    }
    db::update("mo_operation", &input.id, &json!({ "state": "in_progress", "started_on": today()? }))?
        .ok_or_else(|| Error::msg("the operation is gone"))
}

fn do_log_time(input: LogTime) -> Result<Record> {
    require_user()?;
    let op = operation_of_active_mo(&input.operation)?;
    if text(&op, "state") != Some("in_progress") {
        return Err(Error::msg("start the operation before logging time"));
    }
    let minutes = input.minutes.with_scale(2)?;
    if minutes.is_zero() || minutes.is_negative() {
        return Err(Error::msg("logged minutes must be greater than zero"));
    }
    let on = match input.logged_on.filter(|d| !d.is_empty()) {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let mut data = json!({ "operation": input.operation, "minutes": minutes, "logged_on": on });
    if let Some(n) = input.note.filter(|n| !n.trim().is_empty()) {
        data["note"] = json!(n.trim());
    }
    let log: Record = db::create("mo_time_log", &data)?;
    let total = dec(&op, "actual_minutes", 2)?.checked_add(minutes)?;
    let _: Option<Record> = db::update("mo_operation", &input.operation, &json!({ "actual_minutes": total }))?;
    Ok(log)
}

fn do_finish_operation(input: Id) -> Result<Record> {
    require_user()?;
    let op = operation_of_active_mo(&input.id)?;
    if text(&op, "state") != Some("in_progress") {
        return Err(Error::msg("only a started operation can be finished"));
    }
    db::update("mo_operation", &input.id, &json!({ "state": "done", "finished_on": today()? }))?
        .ok_or_else(|| Error::msg("the operation is gone"))
}


handler! {
    fn create_work_center(input: WorkCenterInput) -> Record { do_create_work_center(input) }
    fn set_work_center(input: SetWorkCenter) -> Record { do_set_work_center(input) }
    fn add_bom_operation(input: BomOperationInput) -> Record { do_add_bom_operation(input) }
    fn start_operation(input: Id) -> Record { do_start_operation(input) }
    fn log_time(input: LogTime) -> Record { do_log_time(input) }
    fn finish_operation(input: Id) -> Record { do_finish_operation(input) }
}
