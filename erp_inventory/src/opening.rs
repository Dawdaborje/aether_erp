//! Opening stock from a CSV: one adjust move per 200 rows, posted through the ledger like any other move.
//!
//! Columns: `item` (the item code), `qty`, `unit_cost`, and for tracked items `lot` and, optionally,
//! `expiry_date`. A dry run checks every row and writes nothing; a real run refuses the whole file when
//! one row is wrong. The ledger stays append-only: undoing is a void of the posted moves.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::media;
use aether_sdk::prelude::*;
use aether_sdk::cache;
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::common::{id_of, require, require_user, text, today, Record};
use crate::moves::{opening_draft, opening_line, opening_post};
use crate::rules::{QTY_SCALE, VALUE_SCALE};

const CHUNK: usize = 200;
const COLUMNS: &[&str] = &["item", "qty", "unit_cost", "lot", "expiry_date"];

#[derive(Deserialize)]
struct OpeningInput {
    company: String,
    warehouse: String,
    #[serde(default)]
    posting_date: Option<String>,
    /// What a file field sends: a media file, a list of them, or its key.
    file: Value,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Deserialize)]
struct ReportId {
    report: String,
}

#[derive(Deserialize, Default)]
struct Nothing {}

fn media_key(file: &Value) -> Result<String> {
    let found = match file {
        Value::String(key) => Some(key.as_str()),
        Value::Array(files) => files.first().and_then(|f| f.get("key")).and_then(Value::as_str),
        other => other.get("key").and_then(Value::as_str),
    };
    found
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::msg("attach the CSV file first"))
}

/// RFC 4180: quoted cells, doubled quotes, commas, `\r\n` or `\n`. Blank lines are skipped.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => row.push(std::mem::take(&mut cell)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut cell));
                if row.iter().any(|c| !c.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            (c, _) => cell.push(c),
        }
    }
    row.push(cell);
    if row.iter().any(|c| !c.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

struct Line {
    row: usize,
    item: String,
    qty: Decimal,
    unit_cost: Decimal,
    lot: Option<String>,
    expiry: Option<String>,
}

fn find_item(code: &str, cache: &mut HashMap<String, Option<Record>>) -> Result<Option<Record>> {
    if let Some(known) = cache.get(code) {
        return Ok(known.clone());
    }
    let found = db::find::<Record>("erp_item.item").filter("code", code).first()?;
    cache.insert(code.to_string(), found.clone());
    Ok(found)
}

/// Every row's problem in the file's own words; an empty list means the file can be posted.
fn check(rows: &[Vec<String>]) -> Result<(Vec<Line>, Vec<Value>)> {
    let header: Vec<String> = rows.first().map(|r| r.iter().map(|c| c.trim().to_lowercase()).collect()).unwrap_or_default();
    let missing: Vec<&str> = ["item", "qty", "unit_cost"].into_iter().filter(|c| !header.iter().any(|h| h == c)).collect();
    if !missing.is_empty() {
        return Err(Error::msg(format!("the first row must name the columns {}; missing: {}", COLUMNS.join(", "), missing.join(", "))));
    }
    let at = |name: &str| header.iter().position(|h| h == name);
    let mut items = HashMap::new();
    let mut serials: HashSet<(String, String)> = HashSet::new();
    let (mut lines, mut problems) = (Vec::new(), Vec::new());
    for (index, cells) in rows.iter().enumerate().skip(1) {
        let row = index + 1;
        let cell = |name: &str| at(name).and_then(|i| cells.get(i)).map(|c| c.trim()).unwrap_or("");
        let mut fail = |message: String| problems.push(json!({ "row": row, "item": cell("item"), "message": message }));
        let code = cell("item");
        let Some(item) = find_item(code, &mut items)? else {
            fail(format!("no item has the code `{code}`"));
            continue;
        };
        if item.get("active") == Some(&json!(false)) || item.get("is_stocked") == Some(&json!(false)) {
            fail("the item is inactive or not stocked".into());
            continue;
        }
        let qty = match Decimal::parse(cell("qty")).and_then(|q| q.with_scale(QTY_SCALE)) {
            Ok(q) if !q.is_zero() && !q.is_negative() => q,
            _ => {
                fail("qty must be a number above zero".into());
                continue;
            }
        };
        let unit_cost = match Decimal::parse(cell("unit_cost")).and_then(|c| c.with_scale(VALUE_SCALE)) {
            Ok(c) if !c.is_negative() => c,
            _ => {
                fail("unit_cost must be a number, zero or more".into());
                continue;
            }
        };
        let lot = Some(cell("lot")).filter(|l| !l.is_empty()).map(str::to_string);
        let tracking = text(&item, "tracking").unwrap_or("none");
        match (tracking, lot.is_some()) {
            ("lot" | "serial", false) => {
                fail("this item is tracked: give a lot or serial".into());
                continue;
            }
            ("none", true) => {
                fail("this item does not use lots".into());
                continue;
            }
            _ => {}
        }
        if tracking == "serial" {
            if qty != Decimal::whole(1, QTY_SCALE)? {
                fail("a serial number is one unit: one row per serial".into());
                continue;
            }
            if !serials.insert((code.to_string(), lot.clone().unwrap_or_default())) {
                fail("this serial is in the file twice".into());
                continue;
            }
        }
        let expiry = Some(cell("expiry_date")).filter(|d| !d.is_empty()).map(str::to_string);
        if let Some(date) = &expiry {
            if parse_date(date).is_err() {
                fail("expiry_date is YYYY-MM-DD".into());
                continue;
            }
        }
        lines.push(Line { row, item: id_of(&item)?.to_string(), qty, unit_cost, lot, expiry });
    }
    Ok((lines, problems))
}

fn report_key(id: &str) -> String {
    format!("opening-report:{id}")
}

/// Check the file, and with `mode: "run"` post it as opening stock.
fn do_opening(input: OpeningInput) -> Result<Value> {
    require_user()?;
    let mode = input.mode.as_deref().filter(|m| !m.is_empty()).unwrap_or("dry_run");
    if !matches!(mode, "dry_run" | "run") {
        return Err(Error::msg("mode is dry_run or run"));
    }
    let company = require("company.company", &input.company, "company")?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    let posting_date = match input.posting_date.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => {
            parse_date(d)?;
            d.to_string()
        }
        None => today()?,
    };
    let key = media_key(&input.file)?;
    let bytes = media::read(&key)?;
    let text_of = String::from_utf8(bytes).map_err(|_| Error::msg("the file must be UTF-8 text (save it as CSV UTF-8)"))?;
    let (lines, problems) = check(&parse_csv(&text_of))?;

    let mut moves = Vec::new();
    let mut posted_rows = 0;
    if mode == "run" && problems.is_empty() && !lines.is_empty() {
        let company_id = id_of(&company)?;
        let warehouse_id = id_of(&warehouse)?;
        for (n, chunk) in lines.chunks(CHUNK).enumerate() {
            let draft = opening_draft(company_id, &posting_date, &format!("opening:{key}:{warehouse_id}:{n}"))?;
            let draft_id = id_of(&draft)?.to_string();
            if text(&draft, "state") != Some("posted") {
                for line in chunk {
                    opening_line(&draft_id, &line.item, warehouse_id, line.qty, line.unit_cost, line.lot.as_deref(), line.expiry.as_deref())
                        .map_err(|e| Error::msg(format!("row {}: {e}; the draft move {draft_id} was left unposted", line.row)))?;
                }
                opening_post(&draft_id)?;
            }
            posted_rows += chunk.len();
            moves.push(draft_id);
        }
    }
    let report = key.replace('/', "_");
    let answer = json!({
        "mode": mode,
        "rows": lines.len() + problems.len(),
        "valid": lines.len(),
        "problems": problems,
        "posted": posted_rows,
        "moves": moves,
    });
    cache::set(&report_key(&report), &answer, Some(3600))?;
    Ok(json!({ "report": report, "mode": mode, "valid": lines.len(), "problems": answer["problems"].as_array().map_or(0, Vec::len), "posted": posted_rows }))
}

/// The problems found, for the result page.
fn do_report(input: ReportId) -> Result<Vec<Value>> {
    require_user()?;
    let answer: Option<Value> = cache::get(&report_key(&input.report))?;
    let answer = answer.ok_or_else(|| Error::msg("that report has expired: check the file again"))?;
    let mut rows = answer["problems"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        let summary = if answer["mode"] == "run" {
            format!("{} rows posted in {} move(s)", answer["posted"], answer["moves"].as_array().map_or(0, Vec::len))
        } else {
            format!("{} rows are valid and ready to post", answer["valid"])
        };
        rows.push(json!({ "row": "", "item": "", "message": summary }));
    }
    Ok(rows)
}

fn do_template(_: Nothing) -> Result<Value> {
    require_user()?;
    Ok(json!({ "headers": COLUMNS, "csv": format!("{}\n", COLUMNS.join(",")) }))
}

/// Files uploaded for opening stock are read only by the plugin, for the person who uploaded them.
fn do_can_read(_: Value) -> Result<Value> {
    Ok(json!({ "allow": false }))
}

handler! {
    fn import_opening_stock(input: OpeningInput) -> Value { do_opening(input) }
    fn opening_stock_report(input: ReportId) -> Vec<Value> { do_report(input) }
    fn opening_stock_template(input: Nothing) -> Value { do_template(input) }
    fn can_read_media(input: Value) -> Value { do_can_read(input) }
}

#[cfg(test)]
mod tests {
    use super::parse_csv;

    #[test]
    fn reads_quotes_bom_crlf_and_blank_lines() {
        let rows = parse_csv("\u{feff}item,qty,unit_cost,lot\r\n\"A,1\",\"2\",3.5,\"say \"\"hi\"\"\"\r\n\r\nB,1,0,\r\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], ["A,1", "2", "3.5", "say \"hi\""]);
        assert_eq!(rows[2], ["B", "1", "0", ""]);
    }

    #[test]
    fn keeps_a_last_row_without_a_newline() {
        assert_eq!(parse_csv("a,b\n1,2"), [vec!["a", "b"], vec!["1", "2"]]);
    }
}
