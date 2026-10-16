//! Lot master (expiry), serial uniqueness and first-expired-first-out suggestions.

use aether_sdk::dates::{format_date, parse_date, Duration};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_reader, require_user, text, today, Record};
use crate::rules::{fefo_plan, LotStock, QTY_SCALE};

#[derive(Deserialize)]
struct SetLot {
    company: String,
    item: String,
    code: String,
    #[serde(default)]
    expiry_date: Option<String>,
    #[serde(default)]
    mfg_date: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Suggest {
    company: String,
    item: String,
    warehouse: String,
    #[serde(default)]
    qty: Option<Decimal>,
    #[serde(default)]
    as_of: Option<String>,
}

#[derive(Deserialize)]
struct Expiring {
    company: String,
    #[serde(default)]
    warehouse: Option<String>,
    /// Look this many days ahead of `as_of` (default 30).
    #[serde(default)]
    within_days: Option<i64>,
    #[serde(default)]
    as_of: Option<String>,
}

fn lot_row(company: &str, item: &str, code: &str) -> Result<Option<Record>> {
    db::find("stock_lot")
        .filter("company", company)
        .filter("item", item)
        .filter("code", code)
        .first()
}

/// Make sure a received lot has a master row; the expiry comes from the line, else the item's shelf life.
pub(crate) fn ensure_lot(company: &str, item: &Record, code: &str, receipt_date: &str, expiry: Option<&str>) -> Result<()> {
    let item_id = id_of(item)?;
    if lot_row(company, item_id, code)?.is_some() {
        return Ok(());
    }
    let expiry = match expiry.filter(|e| !e.is_empty()) {
        Some(e) => {
            parse_date(e)?;
            Some(e.to_string())
        }
        None => match item.get("expiry_days").and_then(Value::as_i64) {
            Some(days) if days > 0 => Some(format_date(parse_date(receipt_date)? + Duration::days(days))),
            _ => None,
        },
    };
    let mut data = json!({ "company": company, "item": item_id, "code": code });
    if let Some(e) = expiry {
        data["expiry_date"] = json!(e);
    }
    let _: Record = db::create("stock_lot", &data).map_err(|e| e.or("could not register the lot"))?;
    Ok(())
}

/// An expired lot cannot be issued (or consumed) on or after its expiry date.
pub(crate) fn assert_not_expired(company: &str, item: &str, code: &str, on: &str) -> Result<()> {
    if let Some(row) = lot_row(company, item, code)? {
        if let Some(expiry) = text(&row, "expiry_date") {
            if expiry < on {
                return Err(Error::msg(format!("lot {code} expired on {expiry}")));
            }
        }
    }
    Ok(())
}

/// A serial number is in stock at most once: refuse receiving one that still has quantity on hand.
pub(crate) fn assert_serial_new(company: &str, item: &str, serial: &str) -> Result<()> {
    let layers: Vec<Record> = db::find("stock_layer")
        .filter("company", company)
        .filter("item", item)
        .filter("lot", serial)
        .limit(50)
        .all()?;
    for layer in &layers {
        if !qty_of(layer, "remaining_qty")?.is_zero() {
            return Err(Error::msg(format!("serial {serial} is already in stock")));
        }
    }
    Ok(())
}

fn do_set_lot(input: SetLot) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if text(&item, "tracking") == Some("none") {
        return Err(Error::msg("this item does not use lots"));
    }
    let code = input.code.trim();
    if code.is_empty() {
        return Err(Error::msg("the lot needs a code"));
    }
    for d in [&input.expiry_date, &input.mfg_date].into_iter().flatten().filter(|d| !d.is_empty()) {
        parse_date(d)?;
    }
    let mut changes = json!({});
    if let Some(d) = input.expiry_date {
        changes["expiry_date"] = if d.is_empty() { Value::Null } else { json!(d) };
    }
    if let Some(d) = input.mfg_date {
        changes["mfg_date"] = if d.is_empty() { Value::Null } else { json!(d) };
    }
    if let Some(n) = input.note {
        changes["note"] = json!(n.trim());
    }
    match lot_row(id_of(&company)?, id_of(&item)?, code)? {
        Some(row) => db::update("stock_lot", id_of(&row)?, &changes)?.ok_or_else(|| Error::msg("the lot is gone")),
        None => {
            changes["company"] = json!(id_of(&company)?);
            changes["item"] = json!(id_of(&item)?);
            changes["code"] = json!(code);
            db::create("stock_lot", &changes).map_err(|e| e.or("could not create the lot"))
        }
    }
}

/// Stock per lot in a warehouse (`item` must be lot or serial tracked), with the lot's expiry attached.
fn stock_by_lot(company: &str, item: &str, warehouse: Option<&str>) -> Result<Vec<(String, String, LotStock)>> {
    let mut q = db::find::<Record>("stock_layer").filter("company", company).filter("item", item).limit(2000);
    if let Some(w) = warehouse {
        q = q.filter("warehouse", w);
    }
    let layers = q.all()?;
    let mut by_lot: std::collections::BTreeMap<(String, String), LotStock> = std::collections::BTreeMap::new();
    for layer in &layers {
        let Some(lot) = text(layer, "lot").filter(|l| !l.is_empty()) else { continue };
        let qty = qty_of(layer, "remaining_qty")?;
        if qty.is_zero() {
            continue;
        }
        let wh = text(layer, "warehouse").unwrap_or_default().to_string();
        let seq = layer.get("seq").and_then(Value::as_i64).unwrap_or(0);
        let entry = by_lot.entry((wh, lot.to_string())).or_insert_with(|| LotStock {
            lot: lot.to_string(),
            qty: Decimal::zero(QTY_SCALE),
            expiry: None,
            seq,
        });
        entry.qty = entry.qty.checked_add(qty)?;
        entry.seq = entry.seq.min(seq);
    }
    let mut out = Vec::new();
    for ((wh, lot), mut stock) in by_lot {
        stock.expiry = lot_row(company, item, &lot)?.and_then(|r| text(&r, "expiry_date").map(str::to_string));
        out.push((wh, lot, stock));
    }
    Ok(out)
}

/// Which lots to pick, earliest expiry first. `plan` is present when `qty` is given.
fn do_suggest_lots(input: Suggest) -> Result<Value> {
    require_reader()?;
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    let as_of = match input.as_of.filter(|d| !d.is_empty()) {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let stock: Vec<LotStock> = stock_by_lot(id_of(&company)?, id_of(&item)?, Some(id_of(&warehouse)?))?
        .into_iter()
        .map(|(_, _, s)| s)
        .collect();
    let lots: Vec<Value> = stock
        .iter()
        .map(|s| {
            json!({
                "lot": s.lot,
                "qty": s.qty,
                "expiry_date": s.expiry,
                "expired": s.expiry.as_deref().map_or(false, |e| e < as_of.as_str()),
            })
        })
        .collect();
    let mut out = json!({ "lots": lots, "as_of": as_of });
    if let Some(qty) = input.qty {
        let (plan, covered) = fefo_plan(&stock, qty, &as_of)?;
        out["plan"] = json!(plan.iter().map(|(lot, q)| json!({ "lot": lot, "qty": q })).collect::<Vec<_>>());
        out["covered"] = json!(covered);
    }
    Ok(out)
}

/// Lots still on hand that expire within the window (or already have).
fn do_expiring_lots(input: Expiring) -> Result<Value> {
    require_reader()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let as_of = match input.as_of.filter(|d| !d.is_empty()) {
        Some(d) => d,
        None => today()?,
    };
    let horizon = format_date(parse_date(&as_of)? + Duration::days(input.within_days.unwrap_or(30).max(0)));
    let lots: Vec<Record> = db::find("stock_lot").filter("company", company_id.as_str()).limit(2000).all()?;
    let mut rows = Vec::new();
    for lot in &lots {
        let Some(expiry) = text(lot, "expiry_date") else { continue };
        if expiry > horizon.as_str() {
            continue;
        }
        let item = text(lot, "item").unwrap_or_default();
        let code = text(lot, "code").unwrap_or_default();
        for (wh, c, stock) in stock_by_lot(&company_id, item, input.warehouse.as_deref())? {
            if c == code {
                rows.push(json!({
                    "item": item,
                    "lot": code,
                    "warehouse": wh,
                    "qty": stock.qty,
                    "expiry_date": expiry,
                    "expired": expiry < as_of.as_str(),
                }));
            }
        }
    }
    rows.sort_by(|a, b| a["expiry_date"].as_str().cmp(&b["expiry_date"].as_str()));
    Ok(json!({ "as_of": as_of, "horizon": horizon, "lots": rows }))
}

fn do_get_lot(input: SetLot) -> Result<Record> {
    require_reader()?;
    lot_row(&input.company, &input.item, input.code.trim())?.ok_or_else(|| Error::msg("there is no such lot"))
}

handler! {
    fn set_lot(input: SetLot) -> Record { do_set_lot(input) }
    fn get_lot(input: SetLot) -> Record { do_get_lot(input) }
    fn suggest_lots(input: Suggest) -> Value { do_suggest_lots(input) }
    fn expiring_lots(input: Expiring) -> Value { do_expiring_lots(input) }
}
