//! Templates, inspections and readings.
//!
//! What the kernel already does is left to it: the inspection number is a naming series, the
//! parameters and readings are child fields written with their parent, and the `workflow` in
//! `rules/quality_inspection.json` says who may accept or reject. The code here decides a
//! reading's result from its range and the inspection's result from its readings.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, opt_dec, require, require_reader, require_user, text, today, Record, QTY_SCALE};

const KINDS: &[&str] = &["numeric", "pass_fail"];
const STAGES: &[&str] = &["any", "receipt", "delivery", "production"];

#[derive(Deserialize)]
struct ParameterIn {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    min: Option<Decimal>,
    #[serde(default)]
    max: Option<Decimal>,
}

#[derive(Deserialize)]
struct CreateTemplate {
    code: String,
    name: String,
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    applies_to: Option<String>,
    parameters: Vec<ParameterIn>,
}

#[derive(Deserialize)]
struct CreateInspection {
    company: String,
    template: String,
    /// Default: the template's item.
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    source_kind: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    lot: Option<String>,
    #[serde(default)]
    qty: Option<Decimal>,
}

#[derive(Deserialize)]
struct RecordReading {
    reading: String,
    /// A measurement, for a numeric parameter.
    #[serde(default)]
    value: Option<Decimal>,
    /// `pass` or `fail`, for a pass / fail parameter.
    #[serde(default)]
    result: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    #[serde(alias = "id")]
    inspection: String,
}

#[derive(Deserialize)]
struct Status {
    source_kind: String,
    source_id: String,
}

#[derive(Deserialize, Default)]
struct ListFilter {
    #[serde(default)]
    state: Option<String>,
}

fn do_create_template(input: CreateTemplate) -> Result<Record> {
    require_user()?;
    if input.parameters.is_empty() {
        return Err(Error::msg("a template needs at least one parameter"));
    }
    let stage = input.applies_to.as_deref().unwrap_or("any");
    if !STAGES.contains(&stage) {
        return Err(Error::msg("applies_to is any, receipt, delivery or production"));
    }
    let mut parameters = Vec::new();
    for p in &input.parameters {
        let kind = p.kind.as_deref().unwrap_or("numeric");
        if !KINDS.contains(&kind) {
            return Err(Error::msg("a parameter is numeric or pass_fail"));
        }
        if kind == "numeric" && p.min.is_none() && p.max.is_none() {
            return Err(Error::msg("a numeric parameter needs a minimum, a maximum or both"));
        }
        let mut row = json!({ "name": p.name.trim(), "kind": kind });
        if let Some(min) = &p.min {
            row["min"] = json!(min.clone().with_scale(QTY_SCALE)?);
        }
        if let Some(max) = &p.max {
            row["max"] = json!(max.clone().with_scale(QTY_SCALE)?);
        }
        parameters.push(row);
    }
    let mut data = json!({
        "code": input.code.trim(),
        "name": input.name.trim(),
        "applies_to": stage,
        "active": true,
        "parameters": parameters,
    });
    if let Some(item) = input.item.filter(|i| !i.is_empty()) {
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": item }))?;
        data["item"] = json!(id_of(&item)?);
    }
    db::create("quality_template", &data).map_err(|e| e.or("could not create the template (the code may be taken)"))
}

fn do_create_inspection(input: CreateInspection) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let template: Record = db::get_expanded("quality_template", &input.template, &["parameters"])?
        .ok_or_else(|| Error::msg("no such template"))?;
    if template.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the template is not active"));
    }
    let item_id = match input.item.filter(|i| !i.is_empty()).or_else(|| text(&template, "item").map(str::to_string)) {
        Some(i) => i,
        None => return Err(Error::msg("name the item being inspected")),
    };
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": item_id }))?;
    if let Some(template_item) = text(&template, "item") {
        if template_item != id_of(&item)? {
            return Err(Error::msg("the template is for another item"));
        }
    }
    let parameters = template.get("parameters").and_then(Value::as_array).cloned().unwrap_or_default();
    if parameters.is_empty() {
        return Err(Error::msg("the template has no parameters"));
    }
    let readings: Vec<Value> = parameters
        .iter()
        .map(|p| {
            let mut row = json!({
                "name": text(p, "name").unwrap_or_default(),
                "kind": text(p, "kind").unwrap_or("numeric"),
                "result": "pending",
            });
            for key in ["min", "max"] {
                if let Some(v) = p.get(key).filter(|v| !v.is_null()) {
                    row[key] = v.clone();
                }
            }
            row
        })
        .collect();
    let mut data = json!({
        "company": id_of(&company)?,
        "template": id_of(&template)?,
        "item": id_of(&item)?,
        "state": "draft",
        "readings": readings,
    });
    for (key, value) in [("source_kind", &input.source_kind), ("source_id", &input.source_id), ("lot", &input.lot)] {
        if let Some(v) = value.as_ref().filter(|v| !v.is_empty()) {
            data[key] = json!(v);
        }
    }
    if let Some(qty) = input.qty {
        data["qty"] = json!(qty.with_scale(QTY_SCALE)?);
    }
    db::create("quality_inspection", &data).map_err(|e| e.or("could not start the inspection"))
}

fn open_inspection(reading: &Record) -> Result<Record> {
    let inspection = require("quality_inspection", text(reading, "inspection").unwrap_or_default(), "inspection")?;
    if text(&inspection, "state") != Some("draft") {
        return Err(Error::msg("the inspection is already decided; reopen it to change readings"));
    }
    Ok(inspection)
}

/// Whether a measured value is inside the reading's range.
fn within(value: &Decimal, min: Option<&Decimal>, max: Option<&Decimal>) -> bool {
    min.map_or(true, |m| value >= m) && max.map_or(true, |m| value <= m)
}

fn do_record_reading(input: RecordReading) -> Result<Record> {
    require_user()?;
    let reading = require("quality_reading", &input.reading, "reading")?;
    let _ = open_inspection(&reading)?;
    let changes = match text(&reading, "kind") {
        Some("pass_fail") => match input.result.as_deref() {
            Some(r @ ("pass" | "fail")) => json!({ "result": r }),
            _ => return Err(Error::msg("record pass or fail for this parameter")),
        },
        _ => {
            let value = input.value.ok_or_else(|| Error::msg("record the measured value"))?;
            let value = value.with_scale(QTY_SCALE)?;
            let (min, max) = (opt_dec(&reading, "min")?, opt_dec(&reading, "max")?);
            let ok = within(&value, min.as_ref(), max.as_ref());
            json!({ "value": value, "result": if ok { "pass" } else { "fail" } })
        }
    };
    db::update("quality_reading", &input.reading, &changes)?.ok_or_else(|| Error::msg("the reading is gone"))
}

fn do_complete(input: Id) -> Result<Record> {
    require_user()?;
    let inspection: Record = db::get_expanded("quality_inspection", &input.inspection, &["readings"])?
        .ok_or_else(|| Error::msg("no such inspection"))?;
    if text(&inspection, "state") != Some("draft") {
        return Err(Error::msg("the inspection is already decided"));
    }
    let readings = inspection.get("readings").and_then(Value::as_array).cloned().unwrap_or_default();
    if readings.is_empty() {
        return Err(Error::msg("the inspection has no readings"));
    }
    if readings.iter().any(|r| text(r, "result") == Some("pending")) {
        return Err(Error::msg("record every reading before deciding"));
    }
    let failed = readings.iter().any(|r| text(r, "result") == Some("fail"));
    // The workflow checks that this caller may make the move.
    db::update(
        "quality_inspection",
        &input.inspection,
        &json!({ "state": if failed { "rejected" } else { "accepted" }, "inspected_on": today()? }),
    )?
    .ok_or_else(|| Error::msg("the inspection is gone"))
}

fn do_cancel(input: Id) -> Result<Record> {
    require_user()?;
    db::update("quality_inspection", &input.inspection, &json!({ "state": "cancelled" }))?
        .ok_or_else(|| Error::msg("the inspection is gone"))
}

fn do_reopen(input: Id) -> Result<Record> {
    require_user()?;
    db::update("quality_inspection", &input.inspection, &json!({ "state": "draft" }))?
        .ok_or_else(|| Error::msg("the inspection is gone"))
}

/// What other plugins ask before releasing stock: `none` (never inspected), `pending`,
/// `accepted` or `rejected`. A rejected inspection wins over an accepted one; a cancelled one
/// does not count.
fn do_status(input: Status) -> Result<Value> {
    crate::common::require_gate_reader()?;
    let rows: Vec<Record> = db::find("quality_inspection")
        .filter("source_kind", input.source_kind.as_str())
        .filter("source_id", input.source_id.as_str())
        .limit(200)
        .all()?;
    let live: Vec<&Record> = rows.iter().filter(|r| text(r, "state") != Some("cancelled")).collect();
    let state = if live.is_empty() {
        "none"
    } else if live.iter().any(|r| text(r, "state") == Some("rejected")) {
        "rejected"
    } else if live.iter().any(|r| text(r, "state") == Some("draft")) {
        "pending"
    } else {
        "accepted"
    };
    Ok(json!({ "state": state, "inspections": live.len() }))
}

fn do_list_inspections(input: ListFilter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("quality_inspection").limit(500);
    if let Some(state) = input.state.filter(|s| !s.is_empty()) {
        q = q.filter("state", state.as_str());
    }
    let mut rows = q.all()?;
    rows.sort_by(|a, b| text(b, "number").cmp(&text(a, "number")));
    Ok(rows)
}

fn do_list_templates(_input: ListFilter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("quality_template").limit(500).all()?;
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}

#[derive(Deserialize)]
struct ReadingsOf {
    #[serde(alias = "id")]
    inspection: String,
}

fn do_list_readings(input: ReadingsOf) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("quality_reading")
        .filter("inspection", input.inspection.as_str())
        .limit(500)
        .all()?;
    rows.sort_by_key(|r| r.get("position").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

handler! {
    fn create_template(input: CreateTemplate) -> Record { do_create_template(input) }
    fn create_inspection(input: CreateInspection) -> Record { do_create_inspection(input) }
    fn record_reading(input: RecordReading) -> Record { do_record_reading(input) }
    fn complete_inspection(input: Id) -> Record { do_complete(input) }
    fn cancel_inspection(input: Id) -> Record { do_cancel(input) }
    fn reopen_inspection(input: Id) -> Record { do_reopen(input) }
    fn inspection_status(input: Status) -> Value { do_status(input) }
    fn list_inspections(input: ListFilter) -> Vec<Record> { do_list_inspections(input) }
    fn list_templates(input: ListFilter) -> Vec<Record> { do_list_templates(input) }
    fn list_readings(input: ReadingsOf) -> Vec<Record> { do_list_readings(input) }
}
