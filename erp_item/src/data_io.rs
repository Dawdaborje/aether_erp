//! Bulk import and export of the master data, through the kernel's importer and exporter.
//!
//! The file is a CSV or XLSX whose header row is the column list in `SPECS` (`import_template` returns it).
//! The importer runs as the caller, through the model's own checks and record rules; it does not run this
//! plugin's functions, so rules that live only in code are not applied to imported rows.

use aether_sdk::export;
use aether_sdk::import;
use aether_sdk::media;
use aether_sdk::storage;
use aether_sdk::prelude::*;
use serde_json::json;

use crate::common::{require_admin, require_reader};

struct Spec {
    model: &'static str,
    columns: &'static [&'static str],
    key: &'static [&'static str],
    /// Link field -> the field of the target the file names it by.
    link_by: &'static [(&'static str, &'static str)],
}

const SPECS: &[Spec] = &[
    Spec {
        model: "item_category",
        columns: &["code", "name", "parent", "valuation_method", "active"],
        key: &["code"],
        link_by: &[("parent", "code")],
    },
    Spec {
        model: "item",
        columns: &["code", "name", "category", "uom", "tracking", "is_stocked", "active", "expiry_days"],
        key: &["code"],
        link_by: &[("category", "code"), ("uom", "code")],
    },
];

fn spec(model: &str) -> Result<&'static Spec> {
    SPECS
        .iter()
        .find(|s| s.model == model)
        .ok_or_else(|| Error::msg(format!("`{model}` cannot be imported here; use one of: {}", names())))
}

fn names() -> String {
    SPECS.iter().map(|s| s.model).collect::<Vec<_>>().join(", ")
}

#[derive(Deserialize)]
struct Model {
    model: String,
}

#[derive(Deserialize)]
struct Start {
    model: String,
    /// What a file field sends: a media file, a list of them, or its key.
    file: Value,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    force: Option<bool>,
}

#[derive(Deserialize)]
struct Job {
    job: String,
}

#[derive(Deserialize)]
struct ExportInput {
    model: String,
    #[serde(default)]
    format: Option<String>,
}

fn media_key(file: &Value) -> Result<String> {
    let found = match file {
        Value::String(key) => Some(key.as_str()),
        Value::Array(files) => files.first().and_then(|f| f.get("key")).and_then(Value::as_str),
        other => other.get("key").and_then(Value::as_str),
    };
    found
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::msg("attach the CSV or XLSX file first"))
}

fn do_template(input: Model) -> Result<Value> {
    require_admin()?;
    let spec = spec(&input.model)?;
    Ok(json!({ "model": spec.model, "headers": spec.columns, "csv": format!("{}\n", spec.columns.join(",")) }))
}

/// Start an import. The default mode is a dry run: it reports what would happen and keeps nothing.
fn do_import(input: Start) -> Result<Value> {
    require_admin()?;
    let spec = spec(&input.model)?;
    let mode = input.mode.as_deref().filter(|m| !m.is_empty()).unwrap_or("dry_run");
    if !matches!(mode, "dry_run" | "run") {
        return Err(Error::msg("mode is dry_run or run"));
    }
    let columns: serde_json::Map<String, Value> = spec.columns.iter().map(|c| (c.to_string(), json!(c))).collect();
    let link_by: serde_json::Map<String, Value> = spec.link_by.iter().map(|(f, t)| (f.to_string(), json!(t))).collect();
    let started = import::start(&json!({
        "model": spec.model,
        "media_key": media_key(&input.file)?,
        "mode": mode,
        "force": input.force.unwrap_or(false),
        "mapping": { "columns": columns, "key": spec.key, "link_by": link_by },
    }))?;
    Ok(json!({ "job": started["job"], "mode": mode }))
}

fn do_status(input: Job) -> Result<Value> {
    require_admin()?;
    import::status(&input.job)
}

/// The rows that failed, for a list page.
fn do_report(input: Job) -> Result<Vec<Value>> {
    require_admin()?;
    let answer = import::result(&input.job, 0, 200, true)?;
    Ok(answer["rows"].as_array().cloned().unwrap_or_default())
}

fn do_undo(input: Job) -> Result<Value> {
    require_admin()?;
    import::undo(&input.job)
}

/// Write the records the caller may see to a file and hand it back as a media file the page can show and download.
fn do_export(input: ExportInput) -> Result<Value> {
    require_reader()?;
    let spec = spec(&input.model)?;
    let format = input.format.as_deref().filter(|f| !f.is_empty()).unwrap_or("csv");
    let written = export::run(&json!({ "model": spec.model, "fields": spec.columns, "format": format }))?;
    let path = written["storage_key"].as_str().ok_or_else(|| Error::msg("the export has no file"))?;
    let bytes = storage::read_bytes(path)?.ok_or_else(|| Error::msg("the export file is gone"))?;
    let file = media::store(&format!("{}.{format}", spec.model), &bytes);
    storage::delete(path)?;
    let file = file?;
    Ok(json!({ "key": file.key, "name": file.name, "mime": file.mime, "size": file.size, "rows": written["rows"] }))
}

/// The files this plugin stores are exports, readable only by the person who made them.
fn do_can_read(_: Value) -> Result<Value> {
    Ok(json!({ "allow": false }))
}

handler! {
    fn import_template(input: Model) -> Value { do_template(input) }
    fn import_file(input: Start) -> Value { do_import(input) }
    fn import_status(input: Job) -> Value { do_status(input) }
    fn import_report(input: Job) -> Vec<Value> { do_report(input) }
    fn import_undo(input: Job) -> Value { do_undo(input) }
    fn export_data(input: ExportInput) -> Value { do_export(input) }
    fn can_read_media(input: Value) -> Value { do_can_read(input) }
}
