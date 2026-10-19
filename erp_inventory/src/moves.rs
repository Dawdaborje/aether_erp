//! Draft and post stock moves; void; on-hand from layers.
//!
//! Layer model (v0.1): allocations are append-only; `remaining_*` on `stock_layer` is updated in
//! place (open-layer cache). Optimistic concurrency re-reads those remainders before commit.
//! See `erp_inventory/docs/design.md` (Layers).

use std::collections::HashMap;

use aether_sdk::dates::{parse_date, Datelike};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    company_currency_code, id_of, qty_of, require, require_admin, require_reader, require_user, text, today, valuation_for,
    value_of, Record, HOLDING_KINDS,
};
use crate::rules::{issue, on_hand as on_hand_of, receive, receive_average, standard_variance, Layer, Method, QTY_SCALE, VALUE_SCALE};

#[derive(Deserialize)]
struct DraftInput {
    company: String,
    #[serde(default)]
    posting_date: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    source_kind: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    source_version: Option<i64>,
    #[serde(default)]
    idem_key: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct AddLine {
    #[serde(rename = "move")]
    move_id: String,
    item: String,
    qty: Decimal,
    warehouse: String,
    #[serde(default)]
    warehouse_from: Option<String>,
    #[serde(default)]
    lot: Option<String>,
    #[serde(default)]
    unit_cost: Option<Decimal>,
    /// For adjust: true adds stock (needs unit_cost), false removes. Default: unit_cost.is_some().
    #[serde(default)]
    increase: Option<bool>,
    /// Inbound lines of a tracked item: the lot's expiry (default: the item's shelf life from the move date).
    #[serde(default)]
    expiry_date: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct VoidInput {
    id: String,
    reason: String,
}

#[derive(Deserialize)]
struct OnHandQuery {
    company: String,
    item: String,
    warehouse: String,
    #[serde(default)]
    lot: Option<String>,
}

struct PostCtx {
    company_id: String,
    posting_date: String,
    total_value: Decimal,
    variances: Vec<Value>,
    line_updates: Vec<(String, Value)>,
    creates: Vec<(String, Value)>,
    layer_updates: Vec<(String, Value)>,
    /// Snapshot of layer remaining qty/value at first touch — used for optimistic concurrency.
    layer_expectations: Vec<(String, Decimal, Decimal)>,
    layer_seq: HashMap<(String, String), i64>,
    /// When set, outbound may consume this demand's reservations.
    demand_kind: Option<String>,
    demand_id: Option<String>,
}

impl PostCtx {
    fn new(company_id: String, posting_date: String) -> Self {
        Self {
            company_id,
            posting_date,
            total_value: Decimal::zero(VALUE_SCALE),
            variances: Vec::new(),
            line_updates: Vec::new(),
            creates: Vec::new(),
            layer_updates: Vec::new(),
            layer_expectations: Vec::new(),
            layer_seq: HashMap::new(),
            demand_kind: None,
            demand_id: None,
        }
    }

    fn next_seq(&mut self, item: &str, warehouse: &str) -> Result<i64> {
        let key = (item.to_string(), warehouse.to_string());
        let seq_n = match self.layer_seq.get(&key).copied() {
            Some(n) => n,
            None => next_layer_seq(&self.company_id, item, warehouse)?,
        };
        self.layer_seq.insert(key, seq_n + 1);
        Ok(seq_n)
    }

    fn note_layer_expectation(&mut self, id: &str, qty: Decimal, value: Decimal) {
        if self.layer_expectations.iter().any(|(eid, _, _)| eid == id) {
            return;
        }
        self.layer_expectations.push((id.to_string(), qty, value));
    }

    fn pending_layer(&self, id: &str) -> Option<&Value> {
        self.layer_updates.iter().rev().find(|(lid, _)| lid == id).map(|(_, v)| v)
    }
}

fn layers_unchanged(ctx: &PostCtx) -> bool {
    for (id, exp_qty, exp_value) in &ctx.layer_expectations {
        let Ok(layer) = require("stock_layer", id, "layer") else {
            return false;
        };
        let Ok(qty) = qty_of(&layer, "remaining_qty") else {
            return false;
        };
        let Ok(value) = value_of(&layer, "remaining_value") else {
            return false;
        };
        if qty != *exp_qty || value != *exp_value {
            return false;
        }
    }
    true
}

fn draft(kind: &str, input: DraftInput) -> Result<Record> {
    require_user()?;
    if let Some(key) = input.idem_key.as_deref().filter(|k| !k.is_empty()) {
        if let Some(existing) = db::find::<Record>("stock_move").filter("idem_key", key).first()? {
            return Ok(existing);
        }
    }
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let currency = match input.currency.filter(|c| !c.is_empty()) {
        Some(code) => code.to_uppercase(),
        None => company_currency_code(&company_id)?,
    };
    let posting_date = match input.posting_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let mut data = json!({
        "company": company_id,
        "kind": kind,
        "state": "draft",
        "posting_date": posting_date,
        "currency": currency,
        "total_value": Decimal::zero(VALUE_SCALE),
        "source_version": input.source_version.unwrap_or(1),
    });
    if let Some(v) = input.source_kind.filter(|s| !s.is_empty()) {
        data["source_kind"] = json!(v);
    }
    if let Some(v) = input.source_id.filter(|s| !s.is_empty()) {
        data["source_id"] = json!(v);
    }
    if let Some(v) = input.idem_key.filter(|s| !s.is_empty()) {
        data["idem_key"] = json!(v);
    }
    if let Some(v) = input.reason.filter(|s| !s.is_empty()) {
        data["reason"] = json!(v);
    }
    db::create("stock_move", &data).map_err(|e| e.or("could not create the draft move"))
}

fn assert_draft(move_row: &Record) -> Result<()> {
    if text(move_row, "state") != Some("draft") {
        return Err(Error::msg("only a draft move can be changed"));
    }
    Ok(())
}

fn holding_warehouse(id: &str, company_id: &str) -> Result<Record> {
    let wh = require("stock_warehouse", id, "warehouse")?;
    if text(&wh, "company") != Some(company_id) {
        return Err(Error::msg("the warehouse belongs to another company"));
    }
    if wh.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the warehouse is not active"));
    }
    let kind = text(&wh, "kind").unwrap_or("stock");
    if !HOLDING_KINDS.contains(&kind) {
        return Err(Error::msg("stock is only held in stock or transit warehouses"));
    }
    Ok(wh)
}

fn do_add_line(input: AddLine) -> Result<Record> {
    require_user()?;
    let move_row = require("stock_move", &input.move_id, "move")?;
    assert_draft(&move_row)?;
    let kind = text(&move_row, "kind").unwrap_or_default();
    let company_id = text(&move_row, "company").ok_or_else(|| Error::msg("move has no company"))?;

    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("line quantity must be greater than zero"));
    }

    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    if item.get("is_stocked") == Some(&json!(false)) {
        return Err(Error::msg("the item is not stocked"));
    }
    let item_id = id_of(&item)?.to_string();
    let uom_id = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?;
    let tracking = text(&item, "tracking").unwrap_or("none");
    let lot = input.lot.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match tracking {
        "lot" if lot.is_none() => return Err(Error::msg("this item requires a lot")),
        "serial" if lot.is_none() => return Err(Error::msg("this item requires a serial number")),
        "none" if lot.is_some() => return Err(Error::msg("this item does not use lots")),
        _ => {}
    }
    if tracking == "serial" && qty != Decimal::whole(1, QTY_SCALE)? {
        return Err(Error::msg("a serial number is one unit: add one line per serial"));
    }

    let warehouse = holding_warehouse(&input.warehouse, company_id)?;
    if kind == "transfer" {
        let from_id = input.warehouse_from.as_deref().ok_or_else(|| Error::msg("a transfer needs warehouse_from"))?;
        let _from = holding_warehouse(from_id, company_id)?;
        if from_id == id_of(&warehouse)? {
            return Err(Error::msg("transfer warehouses must differ"));
        }
    } else if input.warehouse_from.is_some() {
        return Err(Error::msg("warehouse_from is only for transfers"));
    }

    let increase = input.increase.unwrap_or_else(|| input.unit_cost.is_some());
    if kind == "receipt" || kind == "manufacture_produce" || (kind == "adjust" && increase) {
        let cost = input.unit_cost.ok_or_else(|| Error::msg("this line needs unit_cost"))?.with_scale(VALUE_SCALE)?;
        if cost.is_negative() {
            return Err(Error::msg("unit cost is not negative"));
        }
    }

    let existing: Vec<Record> = db::find("stock_move_line").filter("move", id_of(&move_row)?).limit(200).all()?;
    if existing.len() >= 200 {
        return Err(Error::msg("a move cannot have more than 200 lines"));
    }
    if let Some(code) = lot {
        let posting_date = text(&move_row, "posting_date").unwrap_or_default();
        let inbound = kind == "receipt" || kind == "manufacture_produce" || (kind == "adjust" && increase);
        if inbound {
            if tracking == "serial" {
                crate::lots::assert_serial_new(company_id, &item_id, code)?;
                let twice = existing.iter().any(|r| text(r, "item") == Some(item_id.as_str()) && text(r, "lot") == Some(code));
                if twice {
                    return Err(Error::msg(format!("serial {code} is already on this move")));
                }
            }
            crate::lots::ensure_lot(company_id, &item, code, posting_date, input.expiry_date.as_deref())?;
        } else if matches!(kind, "issue" | "manufacture_consume") {
            crate::lots::assert_not_expired(company_id, &item_id, code, posting_date)?;
        }
    }
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;

    let mut data = json!({
        "move": id_of(&move_row)?,
        "line_no": line_no,
        "item": item_id,
        "qty": qty,
        "uom": uom_id,
        "warehouse": id_of(&warehouse)?,
        "unit_cost": input.unit_cost.unwrap_or_else(|| Decimal::zero(VALUE_SCALE)).with_scale(VALUE_SCALE)?,
        "value": Decimal::zero(VALUE_SCALE),
    });
    if let Some(from) = &input.warehouse_from {
        data["warehouse_from"] = json!(from);
    }
    if let Some(lot) = lot {
        data["lot"] = json!(lot);
    }
    // Stash adjust direction in valuation_method draft slot? Use unit_cost==0 and a convention:
    // for adjust decrease, unit_cost stays 0; increase has cost. Already handled at post via unit_cost.
    if kind == "adjust" && !increase {
        data["unit_cost"] = json!(Decimal::zero(VALUE_SCALE));
    }
    db::create("stock_move_line", &data).map_err(|e| e.or("could not add the line"))
}

fn lines_of(move_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("stock_move_line").filter("move", move_id).limit(200).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

pub(crate) fn open_layers(company: &str, item: &str, warehouse: &str, lot: Option<&str>) -> Result<Vec<(Record, Layer)>> {
    open_layers_with(company, item, warehouse, lot, None)
}

fn open_layers_with(
    company: &str,
    item: &str,
    warehouse: &str,
    lot: Option<&str>,
    pending: Option<&PostCtx>,
) -> Result<Vec<(Record, Layer)>> {
    let mut rows: Vec<Record> = db::find("stock_layer")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .limit(2000)
        .all()?;
    if rows.len() >= 2000 {
        return Err(Error::msg("too many stock layers for this item and warehouse; contact support"));
    }
    if let Some(lot) = lot {
        rows.retain(|r| text(r, "lot") == Some(lot));
    } else {
        rows.retain(|r| text(r, "lot").unwrap_or("").is_empty());
    }
    rows.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    let mut out = Vec::new();
    for row in rows {
        let id = id_of(&row)?.to_string();
        let method = Method::parse(text(&row, "valuation_method").unwrap_or("fifo")).unwrap_or(Method::Fifo);
        let (remaining_qty, remaining_value, unit_cost) = if let Some(ctx) = pending {
            if let Some(p) = ctx.pending_layer(&id) {
                (
                    serde_json::from_value::<Decimal>(p.get("remaining_qty").cloned().unwrap_or(Value::Null))?
                        .with_scale(QTY_SCALE)?,
                    serde_json::from_value::<Decimal>(p.get("remaining_value").cloned().unwrap_or(Value::Null))?
                        .with_scale(VALUE_SCALE)?,
                    serde_json::from_value::<Decimal>(p.get("unit_cost").cloned().unwrap_or(Value::Null))?
                        .with_scale(VALUE_SCALE)?,
                )
            } else {
                (qty_of(&row, "remaining_qty")?, value_of(&row, "remaining_value")?, value_of(&row, "unit_cost")?)
            }
        } else {
            (qty_of(&row, "remaining_qty")?, value_of(&row, "remaining_value")?, value_of(&row, "unit_cost")?)
        };
        if remaining_qty.is_zero() {
            continue;
        }
        out.push((
            row.clone(),
            Layer {
                id,
                seq: row.get("seq").and_then(Value::as_i64).unwrap_or(0),
                remaining_qty,
                remaining_value,
                unit_cost,
                method,
            },
        ));
    }
    Ok(out)
}

fn next_layer_seq(company: &str, item: &str, warehouse: &str) -> Result<i64> {
    let rows: Vec<Record> = db::find("stock_layer")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .limit(2000)
        .all()?;
    Ok(rows.iter().map(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1)
}

fn allocate_number(company_id: &str, posting_date: &str) -> Result<(String, i64, Option<String>)> {
    let year = parse_date(posting_date)?.year();
    let prefix = "STK";
    let existing: Option<Record> = db::find("stock_counter")
        .filter("company", company_id)
        .filter("prefix", prefix)
        .filter("year", year)
        .first()?;
    let (counter_id, next) = match existing {
        Some(row) => (id_of(&row)?.to_string(), row.get("next").and_then(Value::as_i64).unwrap_or(1)),
        None => {
            let made: Record = db::create(
                "stock_counter",
                &json!({ "company": company_id, "prefix": prefix, "year": year, "next": 1 }),
            )?;
            (id_of(&made)?.to_string(), 1)
        }
    };
    let number = format!("{prefix}-{year}-{next:05}");
    Ok((number, next, Some(counter_id)))
}

fn post_inbound_line(ctx: &mut PostCtx, line: &Record, warehouse_id: &str, qty: Decimal, actual_cost: Decimal) -> Result<()> {
    let line_id = id_of(line)?.to_string();
    let item_id = text(line, "item").unwrap_or_default().to_string();
    let lot = text(line, "lot").filter(|s| !s.is_empty()).map(str::to_string);
    let (method_s, standard) = valuation_for(&item_id, &ctx.company_id, &ctx.posting_date)?;
    let method = Method::parse(&method_s).ok_or_else(|| Error::msg("unknown valuation method"))?;

    let (layer_cost, line_value, variance) = match method {
        Method::Standard => {
            if standard.is_zero() {
                return Err(Error::msg("standard cost must be set for this item and company"));
            }
            let value = qty.times(standard, VALUE_SCALE)?;
            let var = standard_variance(qty, actual_cost, standard)?;
            (standard, value, Some(var))
        }
        Method::Fifo | Method::Average => {
            let value = qty.times(actual_cost, VALUE_SCALE)?;
            (actual_cost, value, None)
        }
    };
    ctx.total_value = ctx.total_value.checked_add(line_value)?;

    match method {
        Method::Average => {
            let open = open_layers_with(&ctx.company_id, &item_id, warehouse_id, lot.as_deref(), Some(ctx))?;
            let pool = open.into_iter().find(|(_, l)| l.method == Method::Average);
            match pool {
                Some((row, layer)) => {
                    let layer_id = id_of(&row)?.to_string();
                    // Expectation is the DB snapshot, not pending (first touch only).
                    let db_qty = qty_of(&row, "remaining_qty")?;
                    let db_value = value_of(&row, "remaining_value")?;
                    ctx.note_layer_expectation(&layer_id, db_qty, db_value);
                    let updated = receive_average(Some(layer), qty, layer_cost, &layer_id)?;
                    ctx.layer_updates.push((
                        layer_id,
                        json!({
                            "remaining_qty": updated.remaining_qty,
                            "remaining_value": updated.remaining_value,
                            "unit_cost": updated.unit_cost,
                        }),
                    ));
                }
                None => {
                    let seq_n = ctx.next_seq(&item_id, warehouse_id)?;
                    let layer = receive(qty, layer_cost, Method::Average, "new", seq_n)?;
                    let mut data = json!({
                        "company": ctx.company_id,
                        "item": item_id,
                        "warehouse": warehouse_id,
                        "receipt_line": line_id,
                        "seq": seq_n,
                        "qty_in": layer.remaining_qty,
                        "value_in": layer.remaining_value,
                        "remaining_qty": layer.remaining_qty,
                        "remaining_value": layer.remaining_value,
                        "unit_cost": layer.unit_cost,
                        "valuation_method": "average",
                        "opened_at": ctx.posting_date,
                    });
                    if let Some(lot) = &lot {
                        data["lot"] = json!(lot);
                    }
                    ctx.creates.push(("stock_layer".into(), data));
                }
            }
        }
        Method::Fifo | Method::Standard => {
            let seq_n = ctx.next_seq(&item_id, warehouse_id)?;
            let layer = receive(qty, layer_cost, method, "new", seq_n)?;
            let mut data = json!({
                "company": ctx.company_id,
                "item": item_id,
                "warehouse": warehouse_id,
                "receipt_line": line_id,
                "seq": seq_n,
                "qty_in": layer.remaining_qty,
                "value_in": layer.remaining_value,
                "remaining_qty": layer.remaining_qty,
                "remaining_value": layer.remaining_value,
                "unit_cost": layer.unit_cost,
                "valuation_method": method.as_str(),
                "opened_at": ctx.posting_date,
            });
            if let Some(lot) = &lot {
                data["lot"] = json!(lot);
            }
            ctx.creates.push(("stock_layer".into(), data));
            if let Some(var) = variance {
                ctx.variances.push(json!({ "line": line_id, "variance": var }));
            }
        }
    }
    ctx.line_updates.push((
        line_id,
        json!({
            "unit_cost": layer_cost,
            "value": line_value,
            "valuation_method": method.as_str(),
        }),
    ));
    Ok(())
}

/// Inbound that carries exact value from a transfer allocation (no re-valuation).
fn post_inbound_at_cost(ctx: &mut PostCtx, line: &Record, warehouse_id: &str, qty: Decimal, unit_cost: Decimal, method: Method) -> Result<()> {
    let line_id = id_of(line)?.to_string();
    let item_id = text(line, "item").unwrap_or_default().to_string();
    let lot = text(line, "lot").filter(|s| !s.is_empty()).map(str::to_string);
    let line_value = qty.times(unit_cost, VALUE_SCALE)?;
    ctx.total_value = ctx.total_value.checked_add(line_value)?;

    match method {
        Method::Average => {
            let open = open_layers_with(&ctx.company_id, &item_id, warehouse_id, lot.as_deref(), Some(ctx))?;
            let pool = open.into_iter().find(|(_, l)| l.method == Method::Average);
            match pool {
                Some((row, layer)) => {
                    let layer_id = id_of(&row)?.to_string();
                    ctx.note_layer_expectation(&layer_id, qty_of(&row, "remaining_qty")?, value_of(&row, "remaining_value")?);
                    let updated = receive_average(Some(layer), qty, unit_cost, &layer_id)?;
                    ctx.layer_updates.push((
                        layer_id,
                        json!({
                            "remaining_qty": updated.remaining_qty,
                            "remaining_value": updated.remaining_value,
                            "unit_cost": updated.unit_cost,
                        }),
                    ));
                }
                None => {
                    let seq_n = ctx.next_seq(&item_id, warehouse_id)?;
                    let layer = receive(qty, unit_cost, Method::Average, "new", seq_n)?;
                    let mut data = json!({
                        "company": ctx.company_id, "item": item_id, "warehouse": warehouse_id,
                        "receipt_line": line_id, "seq": seq_n,
                        "qty_in": layer.remaining_qty, "value_in": layer.remaining_value,
                        "remaining_qty": layer.remaining_qty, "remaining_value": layer.remaining_value,
                        "unit_cost": layer.unit_cost, "valuation_method": "average", "opened_at": ctx.posting_date,
                    });
                    if let Some(lot) = &lot {
                        data["lot"] = json!(lot);
                    }
                    ctx.creates.push(("stock_layer".into(), data));
                }
            }
        }
        Method::Fifo | Method::Standard => {
            let seq_n = ctx.next_seq(&item_id, warehouse_id)?;
            let layer = receive(qty, unit_cost, method, "new", seq_n)?;
            let mut data = json!({
                "company": ctx.company_id, "item": item_id, "warehouse": warehouse_id,
                "receipt_line": line_id, "seq": seq_n,
                "qty_in": layer.remaining_qty, "value_in": layer.remaining_value,
                "remaining_qty": layer.remaining_qty, "remaining_value": layer.remaining_value,
                "unit_cost": layer.unit_cost, "valuation_method": method.as_str(), "opened_at": ctx.posting_date,
            });
            if let Some(lot) = &lot {
                data["lot"] = json!(lot);
            }
            ctx.creates.push(("stock_layer".into(), data));
        }
    }
    ctx.line_updates.push((
        line_id,
        json!({ "unit_cost": unit_cost, "value": line_value, "valuation_method": method.as_str() }),
    ));
    Ok(())
}

fn post_outbound_line(ctx: &mut PostCtx, line: &Record, warehouse_id: &str, qty: Decimal) -> Result<crate::rules::IssueResult> {
    let line_id = id_of(line)?.to_string();
    let item_id = text(line, "item").unwrap_or_default().to_string();
    let lot = text(line, "lot").filter(|s| !s.is_empty()).map(str::to_string);
    let (method_s, _) = valuation_for(&item_id, &ctx.company_id, &ctx.posting_date)?;
    let method = Method::parse(&method_s).ok_or_else(|| Error::msg("unknown valuation method"))?;

    let open = open_layers_with(&ctx.company_id, &item_id, warehouse_id, lot.as_deref(), Some(ctx))?;
    for (row, layer) in &open {
        ctx.note_layer_expectation(&layer.id, qty_of(row, "remaining_qty")?, value_of(row, "remaining_value")?);
    }
    let layers: Vec<Layer> = open.iter().map(|(_, l)| l.clone()).collect();
    let (on_hand, _) = on_hand_of(&layers)?;
    let reserved = crate::reserve::reserved_qty(&ctx.company_id, &item_id, warehouse_id, lot.as_deref())?;
    let demand_reserved = match (&ctx.demand_kind, &ctx.demand_id) {
        (Some(kind), Some(id)) => {
            crate::reserve::reserved_for_demand(&ctx.company_id, &item_id, warehouse_id, lot.as_deref(), kind, id)?
        }
        _ => Decimal::zero(QTY_SCALE),
    };
    // Free stock + this demand's reservation may be issued; other reservations are protected.
    let available = on_hand.checked_sub(reserved)?.checked_add(demand_reserved)?;
    if qty > available {
        return Err(Error::msg(format!(
            "not enough unreserved stock: need {qty}, available {available} (on hand {on_hand}, reserved {reserved})"
        )));
    }
    let result = issue(qty, method, layers)?;
    ctx.total_value = ctx.total_value.checked_add(result.total_value)?;

    for updated in &result.layers {
        ctx.layer_updates.push((
            updated.id.clone(),
            json!({
                "remaining_qty": updated.remaining_qty,
                "remaining_value": updated.remaining_value,
                "unit_cost": updated.unit_cost,
            }),
        ));
    }
    for (i, alloc) in result.allocations.iter().enumerate() {
        ctx.creates.push((
            "stock_layer_alloc".into(),
            json!({
                "layer": alloc.layer_id,
                "move_line": line_id,
                "qty": alloc.qty,
                "value": alloc.value,
                "seq": i as i64 + 1,
            }),
        ));
    }
    ctx.line_updates.push((
        line_id,
        json!({
            "unit_cost": result.unit_cost,
            "value": result.total_value,
            "valuation_method": method.as_str(),
        }),
    ));
    Ok(result)
}

/// Money fields for `gl` (scale 2). Zero amounts are dropped by posting rules.
fn gl_money(value: Decimal) -> Result<Decimal> {
    value.round_to(2).with_scale(2)
}

fn variance_total(variances: &[Value]) -> Result<Decimal> {
    let mut total = Decimal::zero(VALUE_SCALE);
    for row in variances {
        if let Some(v) = row.get("variance") {
            let d: Decimal = serde_json::from_value(v.clone())?;
            total = total.checked_add(d.with_scale(VALUE_SCALE)?)?;
        }
    }
    Ok(total)
}

/// Ledger-facing amounts for a stock move. All money at scale 2; unused sides are zero.
fn stock_event_amounts(kind: &str, total_value: Decimal, variances: &[Value], adjust_increase: Option<bool>) -> Result<Value> {
    let total = gl_money(total_value)?;
    let var = gl_money(variance_total(variances)?)?;
    let zero = Decimal::zero(2);
    let mut inventory_in = zero;
    let mut inventory_out = zero;
    let mut clearing = zero;
    let clearing_debit = zero;
    let mut cogs = zero;
    let cogs_credit = zero;
    let mut adjust_in = zero;
    let mut adjust_out = zero;
    let mut variance_debit = zero;
    let mut variance_credit = zero;
    let mut wip_debit = zero;
    let mut wip_credit = zero;

    match kind {
        "receipt" => {
            inventory_in = total;
            clearing = total.checked_add(var)?;
            if var > zero {
                variance_debit = var;
            } else if var < zero {
                variance_credit = zero.checked_sub(var)?;
            }
        }
        "manufacture_produce" => {
            inventory_in = total;
            wip_credit = total;
        }
        "issue" => {
            cogs = total;
            inventory_out = total;
        }
        "manufacture_consume" => {
            inventory_out = total;
            wip_debit = total;
        }
        "adjust" => {
            if adjust_increase.unwrap_or(true) {
                inventory_in = total;
                adjust_in = total;
            } else {
                inventory_out = total;
                adjust_out = total;
            }
        }
        _ => {}
    }

    Ok(json!({
        "total_value": total,
        "inventory_in": inventory_in,
        "inventory_out": inventory_out,
        "clearing": clearing,
        "clearing_debit": clearing_debit,
        "cogs": cogs,
        "cogs_credit": cogs_credit,
        "adjust_in": adjust_in,
        "adjust_out": adjust_out,
        "variance_debit": variance_debit,
        "variance_credit": variance_credit,
        "wip_debit": wip_debit,
        "wip_credit": wip_credit,
    }))
}

/// Source kinds of moves that reverse a commercial document: the stock effect is the same as an issue or
/// receipt, but the ledger side differs (a supplier return clears GRNI, a customer return credits COGS).
pub const PURCHASE_RETURN_SOURCE: &str = "erp_procurement.return";
pub const SALES_RETURN_SOURCE: &str = "erp_selling.return";

/// Move the cost/clearing side of `amounts` for return moves; any other move is unchanged.
fn reclass_for_source(mut amounts: Value, kind: &str, source_kind: Option<&str>) -> Value {
    let obj = match amounts.as_object_mut() {
        Some(o) => o,
        None => return amounts,
    };
    match (kind, source_kind) {
        ("issue", Some(PURCHASE_RETURN_SOURCE)) => {
            let cogs = obj.get("cogs").cloned().unwrap_or(json!("0.00"));
            obj.insert("clearing_debit".into(), cogs);
            obj.insert("cogs".into(), json!(Decimal::zero(2)));
        }
        ("receipt", Some(SALES_RETURN_SOURCE)) => {
            let clearing = obj.get("clearing").cloned().unwrap_or(json!("0.00"));
            obj.insert("cogs_credit".into(), clearing);
            obj.insert("clearing".into(), json!(Decimal::zero(2)));
        }
        _ => {}
    }
    amounts
}

/// Flip posted amounts for a void so the same account map posts the reverse entry.
fn flip_stock_amounts(amounts: &Value) -> Result<Value> {
    let g = |name: &str| -> Result<Decimal> {
        match amounts.get(name) {
            Some(v) if !v.is_null() => Ok(serde_json::from_value::<Decimal>(v.clone())?.with_scale(2)?),
            _ => Ok(Decimal::zero(2)),
        }
    };
    Ok(json!({
        "total_value": g("total_value")?,
        "inventory_in": g("inventory_out")?,
        "inventory_out": g("inventory_in")?,
        "clearing": Decimal::zero(2),
        "clearing_debit": g("clearing")?,
        "cogs": Decimal::zero(2),
        "cogs_credit": g("cogs")?,
        "adjust_in": g("adjust_out")?,
        "adjust_out": g("adjust_in")?,
        "variance_debit": g("variance_credit")?,
        "variance_credit": g("variance_debit")?,
        "wip_debit": g("wip_credit")?,
        "wip_credit": g("wip_debit")?,
    }))
}

fn run_post(ctx: PostCtx, move_id: &str, number: String, seq: i64, counter_id: Option<String>, kind: &str) -> Result<Record> {
    let total_value = ctx.total_value;
    let variances = ctx.variances.clone();
    let company_id = ctx.company_id.clone();
    apply_ctx_to_tx(db::transaction(), ctx, move_id, number, seq, counter_id)
        .run()
        .map_err(|e| e.or("could not post the move"))?;
    let posted = require("stock_move", move_id, "move")?;
    let stub = PostCtx {
        company_id,
        posting_date: String::new(),
        total_value,
        variances,
        line_updates: Vec::new(),
        creates: Vec::new(),
        layer_updates: Vec::new(),
        layer_expectations: Vec::new(),
        layer_seq: HashMap::new(),
        demand_kind: None,
        demand_id: None,
    };
    emit_posted(&stub, move_id, kind, &posted)?;
    Ok(posted)
}

fn build_post_ctx(kind: &str, company_id: &str, posting_date: &str, lines: &[Record], move_row: &Record) -> Result<PostCtx> {
    let mut ctx = PostCtx::new(company_id.to_string(), posting_date.to_string());
    ctx.demand_kind = text(move_row, "source_kind").filter(|s| !s.is_empty()).map(str::to_string);
    ctx.demand_id = text(move_row, "source_id").filter(|s| !s.is_empty()).map(str::to_string);
    match kind {
        "receipt" | "manufacture_produce" => {
            for line in lines {
                let warehouse_id = text(line, "warehouse").unwrap_or_default().to_string();
                post_inbound_line(&mut ctx, line, &warehouse_id, qty_of(line, "qty")?, value_of(line, "unit_cost")?)?;
            }
        }
        "issue" | "manufacture_consume" => {
            for line in lines {
                let warehouse_id = text(line, "warehouse").unwrap_or_default().to_string();
                post_outbound_line(&mut ctx, line, &warehouse_id, qty_of(line, "qty")?)?;
            }
        }
        "transfer" => {
            for line in lines {
                let to_id = text(line, "warehouse").unwrap_or_default().to_string();
                let from_id = text(line, "warehouse_from").ok_or_else(|| Error::msg("transfer line needs warehouse_from"))?.to_string();
                let qty = qty_of(line, "qty")?;
                let result = post_outbound_line(&mut ctx, line, &from_id, qty)?;
                ctx.line_updates.pop(); // outbound wrote a line update; inbound replaces it
                ctx.total_value = ctx.total_value.checked_sub(result.total_value)?; // undo outbound total; inbound re-adds once
                let (method_s, _) = valuation_for(text(line, "item").unwrap_or_default(), &ctx.company_id, &ctx.posting_date)?;
                let method = Method::parse(&method_s).unwrap_or(Method::Fifo);
                post_inbound_at_cost(&mut ctx, line, &to_id, qty, result.unit_cost, method)?;
            }
        }
        "adjust" => {
            for line in lines {
                let warehouse_id = text(line, "warehouse").unwrap_or_default().to_string();
                let qty = qty_of(line, "qty")?;
                let cost = value_of(line, "unit_cost")?;
                if cost.is_zero() {
                    post_outbound_line(&mut ctx, line, &warehouse_id, qty)?;
                } else {
                    post_inbound_line(&mut ctx, line, &warehouse_id, qty, cost)?;
                }
            }
        }
        _ => return Err(Error::msg("unsupported move kind")),
    }
    Ok(ctx)
}

fn do_post_move(input: Id) -> Result<Record> {
    require_user()?;
    let move_row = require("stock_move", &input.id, "move")?;
    if text(&move_row, "state") == Some("posted") {
        return Ok(move_row);
    }
    assert_draft(&move_row)?;
    let kind = text(&move_row, "kind").unwrap_or_default().to_string();
    if !matches!(
        kind.as_str(),
        "receipt" | "issue" | "transfer" | "adjust" | "manufacture_consume" | "manufacture_produce"
    ) {
        return Err(Error::msg("post supports receipt, issue, transfer, adjust or manufacture"));
    }
    let company_id = text(&move_row, "company").ok_or_else(|| Error::msg("move has no company"))?.to_string();
    let posting_date = text(&move_row, "posting_date").unwrap_or_default().to_string();
    let lines = lines_of(&input.id)?;
    if lines.is_empty() {
        return Err(Error::msg("add at least one line before posting"));
    }

    // Optimistic concurrency: rebuild and re-check layers if another post raced us.
    const MAX_ATTEMPTS: usize = 5;
    let mut last_err = Error::msg("could not post the move");
    for _ in 0..MAX_ATTEMPTS {
        let move_row = require("stock_move", &input.id, "move")?;
        if text(&move_row, "state") == Some("posted") {
            return Ok(move_row);
        }
        assert_stock_date_open(&posting_date)?;
        let ctx = match build_post_ctx(&kind, &company_id, &posting_date, &lines, &move_row) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        if !layers_unchanged(&ctx) {
            last_err = Error::msg("stock changed while posting; try again");
            continue;
        }
        let (number, seq, counter_id) = allocate_number(&company_id, &posting_date)?;
        // Re-check after allocating the number (another writer may have landed).
        if !layers_unchanged(&ctx) {
            last_err = Error::msg("stock changed while posting; try again");
            continue;
        }
        return run_post(ctx, &input.id, number, seq, counter_id, &kind);
    }
    Err(last_err)
}

/// Soft period gate: if `gl` is installed and the date falls in a closed period, refuse.
fn assert_stock_date_open(posting_date: &str) -> Result<()> {
    let date = parse_date(posting_date)?;
    let periods: Vec<Record> = match plugins::call("gl", "list_periods", &json!({})) {
        Ok(rows) => rows,
        Err(_) => return Ok(()),
    };
    for period in periods {
        let start = match text(&period, "start_date").and_then(|s| parse_date(s).ok()) {
            Some(d) => d,
            None => continue,
        };
        let end = match text(&period, "end_date").and_then(|s| parse_date(s).ok()) {
            Some(d) => d,
            None => continue,
        };
        if start <= date && date <= end {
            if text(&period, "status") == Some("closed") {
                return Err(Error::msg(format!(
                    "the period {} is closed; post with an open-period date",
                    text(&period, "name").unwrap_or("?")
                )));
            }
            return Ok(());
        }
    }
    Ok(())
}

fn apply_ctx_to_tx(mut tx: aether_sdk::db::Transaction, ctx: PostCtx, move_id: &str, number: String, seq: i64, counter_id: Option<String>) -> aether_sdk::db::Transaction {
    for (model, data) in ctx.creates {
        tx = tx.create(&model, &data);
    }
    for (id, changes) in ctx.layer_updates {
        tx = tx.update("stock_layer", &id, &changes);
    }
    for (id, changes) in ctx.line_updates {
        tx = tx.update("stock_move_line", &id, &changes);
    }
    if let Some(cid) = counter_id {
        tx = tx.update("stock_counter", &cid, &json!({ "next": seq + 1 }));
    }
    tx.update(
        "stock_move",
        move_id,
        &json!({
            "state": "posted",
            "number": number,
            "seq": seq,
            "total_value": ctx.total_value,
        }),
    )
}

fn emit_posted(ctx: &PostCtx, move_id: &str, kind: &str, posted: &Record) -> Result<()> {
    let adjust_increase = if kind == "adjust" {
        let lines = lines_of(move_id)?;
        Some(lines.iter().any(|l| value_of(l, "unit_cost").map(|c| !c.is_zero()).unwrap_or(false)))
    } else {
        None
    };
    let amounts = reclass_for_source(
        stock_event_amounts(kind, ctx.total_value, &ctx.variances, adjust_increase)?,
        kind,
        text(posted, "source_kind"),
    );
    let mut payload = amounts;
    let obj = payload.as_object_mut().unwrap();
    obj.insert("move".into(), json!(move_id));
    obj.insert("number".into(), json!(text(posted, "number")));
    obj.insert("kind".into(), json!(kind));
    obj.insert("company".into(), json!(ctx.company_id));
    obj.insert("currency".into(), json!(text(posted, "currency")));
    obj.insert("posting_date".into(), json!(text(posted, "posting_date")));
    obj.insert("source_version".into(), json!(posted.get("source_version").and_then(Value::as_i64).unwrap_or(1)));
    obj.insert("variances".into(), json!(ctx.variances));
    events::emit("stock_posted", &payload)
}

fn do_outbound_value(input: Id) -> Result<Value> {
    require_user()?;
    let move_row = require("stock_move", &input.id, "move")?;
    assert_draft(&move_row)?;
    let kind = text(&move_row, "kind").unwrap_or_default();
    if !matches!(kind, "issue" | "manufacture_consume" | "transfer" | "adjust") {
        return Err(Error::msg("outbound value is for issue, manufacture consume, transfer or adjust"));
    }
    let company_id = text(&move_row, "company").ok_or_else(|| Error::msg("move has no company"))?;
    let posting_date = text(&move_row, "posting_date").unwrap_or_default();
    let lines = lines_of(&input.id)?;
    let ctx = build_post_ctx(kind, company_id, posting_date, &lines, &move_row)?;
    Ok(json!({ "move": input.id, "total_value": ctx.total_value, "kind": kind }))
}

#[derive(Deserialize)]
struct ManufacturePair {
    consume: String,
    produce: String,
}

fn do_post_manufacture_pair(input: ManufacturePair) -> Result<Value> {
    require_user()?;
    let consume = require("stock_move", &input.consume, "consume move")?;
    let produce = require("stock_move", &input.produce, "produce move")?;
    if text(&consume, "state") == Some("posted") && text(&produce, "state") == Some("posted") {
        return Ok(json!({ "consume": consume, "produce": produce }));
    }
    if text(&consume, "kind") != Some("manufacture_consume") || text(&produce, "kind") != Some("manufacture_produce") {
        return Err(Error::msg("pair requires a manufacture_consume and manufacture_produce draft"));
    }
    assert_draft(&consume)?;
    assert_draft(&produce)?;
    let company_id = text(&consume, "company").ok_or_else(|| Error::msg("move has no company"))?.to_string();
    if text(&produce, "company") != Some(company_id.as_str()) {
        return Err(Error::msg("consume and produce must share a company"));
    }
    let posting_date = text(&consume, "posting_date").unwrap_or_default().to_string();
    assert_stock_date_open(&posting_date)?;

    const MAX_ATTEMPTS: usize = 5;
    let mut last_err = Error::msg("could not post the manufacture pair");
    for _ in 0..MAX_ATTEMPTS {
        let consume = require("stock_move", &input.consume, "consume move")?;
        let produce = require("stock_move", &input.produce, "produce move")?;
        if text(&consume, "state") == Some("posted") && text(&produce, "state") == Some("posted") {
            return Ok(json!({ "consume": consume, "produce": produce }));
        }
        let consume_lines = lines_of(&input.consume)?;
        let produce_lines = lines_of(&input.produce)?;
        if consume_lines.is_empty() || produce_lines.is_empty() {
            return Err(Error::msg("both moves need lines before posting"));
        }
        let consume_ctx = match build_post_ctx("manufacture_consume", &company_id, &posting_date, &consume_lines, &consume) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        let produce_ctx = match build_post_ctx("manufacture_produce", &company_id, &posting_date, &produce_lines, &produce) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        if !layers_unchanged(&consume_ctx) || !layers_unchanged(&produce_ctx) {
            last_err = Error::msg("stock changed while posting; try again");
            continue;
        }
        let (c_number, c_seq, c_counter) = allocate_number(&company_id, &posting_date)?;
        let (p_number, p_seq, p_counter) = allocate_number(&company_id, &posting_date)?;
        // Second allocate may bump the same counter; ensure produce number uses next after consume in one write.
        let (p_number, p_seq, p_counter) = if c_counter == p_counter {
            let next = c_seq + 1;
            (format!("STK-{}-{next:05}", parse_date(&posting_date)?.year()), next, c_counter.clone())
        } else {
            (p_number, p_seq, p_counter)
        };
        if !layers_unchanged(&consume_ctx) || !layers_unchanged(&produce_ctx) {
            last_err = Error::msg("stock changed while posting; try again");
            continue;
        }

        let consume_value = consume_ctx.total_value;
        let produce_value = produce_ctx.total_value;
        let consume_variances = consume_ctx.variances.clone();
        let produce_variances = produce_ctx.variances.clone();
        let company = consume_ctx.company_id.clone();

        let op_count = consume_ctx.creates.len()
            + consume_ctx.layer_updates.len()
            + consume_ctx.line_updates.len()
            + produce_ctx.creates.len()
            + produce_ctx.layer_updates.len()
            + produce_ctx.line_updates.len()
            + 4; // two moves + up to two counter updates
        if op_count > 50 {
            return Err(Error::msg("manufacture pair is too large for one transaction; split the BOM"));
        }

        let same_counter = c_counter.is_some() && c_counter == p_counter;
        let mut tx = db::transaction();
        // When both numbers share a counter, bump once to produce_seq+1 after both moves.
        tx = apply_ctx_to_tx(
            tx,
            consume_ctx,
            &input.consume,
            c_number.clone(),
            c_seq,
            if same_counter { None } else { c_counter.clone() },
        );
        tx = apply_ctx_to_tx(
            tx,
            produce_ctx,
            &input.produce,
            p_number.clone(),
            p_seq,
            if same_counter { None } else { p_counter },
        );
        if same_counter {
            if let Some(cid) = &c_counter {
                tx = tx.update("stock_counter", cid, &json!({ "next": p_seq + 1 }));
            }
        }
        tx.run().map_err(|e| e.or("could not post the manufacture pair"))?;

        let consume_posted = require("stock_move", &input.consume, "consume move")?;
        let produce_posted = require("stock_move", &input.produce, "produce move")?;
        let c_ctx = PostCtx {
            company_id: company.clone(),
            posting_date: posting_date.clone(),
            total_value: consume_value,
            variances: consume_variances,
            line_updates: Vec::new(),
            creates: Vec::new(),
            layer_updates: Vec::new(),
            layer_expectations: Vec::new(),
            layer_seq: HashMap::new(),
            demand_kind: None,
            demand_id: None,
        };
        let p_ctx = PostCtx {
            company_id: company,
            posting_date: posting_date.clone(),
            total_value: produce_value,
            variances: produce_variances,
            line_updates: Vec::new(),
            creates: Vec::new(),
            layer_updates: Vec::new(),
            layer_expectations: Vec::new(),
            layer_seq: HashMap::new(),
            demand_kind: None,
            demand_id: None,
        };
        emit_posted(&c_ctx, &input.consume, "manufacture_consume", &consume_posted)?;
        emit_posted(&p_ctx, &input.produce, "manufacture_produce", &produce_posted)?;
        return Ok(json!({ "consume": consume_posted, "produce": produce_posted }));
    }
    Err(last_err)
}

fn allocs_for_lines(line_ids: &[String]) -> Result<Vec<Record>> {
    let mut all = Vec::new();
    for id in line_ids {
        let mut rows: Vec<Record> = db::find("stock_layer_alloc").filter("move_line", id.as_str()).limit(500).all()?;
        all.append(&mut rows);
    }
    Ok(all)
}

fn layers_for_receipt_lines(line_ids: &[String]) -> Result<Vec<Record>> {
    let mut all = Vec::new();
    for id in line_ids {
        let mut rows: Vec<Record> = db::find("stock_layer").filter("receipt_line", id.as_str()).limit(500).all()?;
        all.append(&mut rows);
    }
    Ok(all)
}

fn do_void_move(input: VoidInput) -> Result<Record> {
    require_admin()?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the move is voided"));
    }
    let original = require("stock_move", &input.id, "move")?;
    if text(&original, "state") != Some("posted") {
        return Err(Error::msg("only a posted move can be voided"));
    }
    if db::find::<Record>("stock_move").filter("reverses", input.id.as_str()).first()?.is_some() {
        return Err(Error::msg("this move is already voided"));
    }
    let costed: Option<Record> = db::find("stock_landed_cost")
        .filter("move", input.id.as_str())
        .filter("state", "posted")
        .first()?;
    if costed.is_some() {
        return Err(Error::msg("landed cost has been added to this receipt; it cannot be voided"));
    }
    let kind = text(&original, "kind").unwrap_or_default().to_string();
    let company_id = text(&original, "company").ok_or_else(|| Error::msg("move has no company"))?.to_string();
    let posting_date = today()?;
    assert_stock_date_open(&posting_date)?;
    let currency = text(&original, "currency").unwrap_or("").to_string();
    let lines = lines_of(&input.id)?;
    let line_ids: Vec<String> = lines.iter().filter_map(|l| text(l, "id").map(str::to_string)).collect();

    let (number, seq, counter_id) = allocate_number(&company_id, &posting_date)?;
    let mut tx = db::transaction();
    let mut total_value = Decimal::zero(VALUE_SCALE);

    match kind.as_str() {
        "receipt" | "adjust" | "manufacture_produce" => {
            // Adjust increase looks like receipt (layers with receipt_line). Adjust decrease like issue.
            let inbound_layers = layers_for_receipt_lines(&line_ids)?;
            let outbound_allocs = allocs_for_lines(&line_ids)?;
            if !inbound_layers.is_empty() && !outbound_allocs.is_empty() {
                return Err(Error::msg("cannot void a move that both received and issued"));
            }
            if !inbound_layers.is_empty() {
                for layer in &inbound_layers {
                    let qty_in = qty_of(layer, "qty_in")?;
                    let remaining = qty_of(layer, "remaining_qty")?;
                    if remaining != qty_in {
                        return Err(Error::msg(format!(
                            "layer {} was already used; void the consuming moves first",
                            id_of(layer)?
                        )));
                    }
                    let value_in = value_of(layer, "value_in")?;
                    total_value = total_value.checked_add(value_in)?;
                    tx = tx.update(
                        "stock_layer",
                        id_of(layer)?,
                        &json!({
                            "remaining_qty": Decimal::zero(QTY_SCALE),
                            "remaining_value": Decimal::zero(VALUE_SCALE),
                        }),
                    );
                }
            } else {
                for alloc in &outbound_allocs {
                    let qty = qty_of(alloc, "qty")?;
                    let value = value_of(alloc, "value")?;
                    if qty.is_negative() || qty.is_zero() {
                        continue;
                    }
                    let layer = require("stock_layer", text(alloc, "layer").unwrap_or_default(), "layer")?;
                    let new_qty = qty_of(&layer, "remaining_qty")?.checked_add(qty)?;
                    let new_value = value_of(&layer, "remaining_value")?.checked_add(value)?;
                    let unit = if new_qty.is_zero() {
                        Decimal::zero(VALUE_SCALE)
                    } else {
                        new_value.div(new_qty, VALUE_SCALE)?
                    };
                    total_value = total_value.checked_add(value)?;
                    tx = tx.update(
                        "stock_layer",
                        id_of(&layer)?,
                        &json!({
                            "remaining_qty": new_qty,
                            "remaining_value": new_value,
                            "unit_cost": unit,
                        }),
                    );
                    let neg_qty = Decimal::zero(QTY_SCALE).checked_sub(qty)?;
                    let neg_value = Decimal::zero(VALUE_SCALE).checked_sub(value)?;
                    tx = tx.create(
                        "stock_layer_alloc",
                        &json!({
                            "layer": id_of(&layer)?,
                            "move_line": text(alloc, "move_line"),
                            "qty": neg_qty,
                            "value": neg_value,
                            "seq": 9000 + alloc.get("seq").and_then(Value::as_i64).unwrap_or(0),
                        }),
                    );
                }
            }
        }
        "issue" | "manufacture_consume" => {
            let outbound_allocs = allocs_for_lines(&line_ids)?;
            if outbound_allocs.is_empty() {
                return Err(Error::msg("this issue has no allocations to reverse"));
            }
            for alloc in &outbound_allocs {
                let qty = qty_of(alloc, "qty")?;
                let value = value_of(alloc, "value")?;
                if qty.is_negative() || qty.is_zero() {
                    continue;
                }
                let layer = require("stock_layer", text(alloc, "layer").unwrap_or_default(), "layer")?;
                let new_qty = qty_of(&layer, "remaining_qty")?.checked_add(qty)?;
                let new_value = value_of(&layer, "remaining_value")?.checked_add(value)?;
                let unit = if new_qty.is_zero() {
                    Decimal::zero(VALUE_SCALE)
                } else {
                    new_value.div(new_qty, VALUE_SCALE)?
                };
                total_value = total_value.checked_add(value)?;
                tx = tx.update(
                    "stock_layer",
                    id_of(&layer)?,
                    &json!({
                        "remaining_qty": new_qty,
                        "remaining_value": new_value,
                        "unit_cost": unit,
                    }),
                );
                let neg_qty = Decimal::zero(QTY_SCALE).checked_sub(qty)?;
                let neg_value = Decimal::zero(VALUE_SCALE).checked_sub(value)?;
                tx = tx.create(
                    "stock_layer_alloc",
                    &json!({
                        "layer": id_of(&layer)?,
                        "move_line": text(alloc, "move_line"),
                        "qty": neg_qty,
                        "value": neg_value,
                        "seq": 9000 + alloc.get("seq").and_then(Value::as_i64).unwrap_or(0),
                    }),
                );
            }
        }
        "transfer" => {
            // Restore source (like issue void) and remove unused destination layers (like receipt void).
            let outbound_allocs = allocs_for_lines(&line_ids)?;
            for alloc in &outbound_allocs {
                let qty = qty_of(alloc, "qty")?;
                let value = value_of(alloc, "value")?;
                if qty.is_negative() || qty.is_zero() {
                    continue;
                }
                let layer = require("stock_layer", text(alloc, "layer").unwrap_or_default(), "layer")?;
                let new_qty = qty_of(&layer, "remaining_qty")?.checked_add(qty)?;
                let new_value = value_of(&layer, "remaining_value")?.checked_add(value)?;
                let unit = if new_qty.is_zero() {
                    Decimal::zero(VALUE_SCALE)
                } else {
                    new_value.div(new_qty, VALUE_SCALE)?
                };
                total_value = total_value.checked_add(value)?;
                tx = tx.update(
                    "stock_layer",
                    id_of(&layer)?,
                    &json!({
                        "remaining_qty": new_qty,
                        "remaining_value": new_value,
                        "unit_cost": unit,
                    }),
                );
                let neg_qty = Decimal::zero(QTY_SCALE).checked_sub(qty)?;
                let neg_value = Decimal::zero(VALUE_SCALE).checked_sub(value)?;
                tx = tx.create(
                    "stock_layer_alloc",
                    &json!({
                        "layer": id_of(&layer)?,
                        "move_line": text(alloc, "move_line"),
                        "qty": neg_qty,
                        "value": neg_value,
                        "seq": 9000 + alloc.get("seq").and_then(Value::as_i64).unwrap_or(0),
                    }),
                );
            }
            let inbound_layers = layers_for_receipt_lines(&line_ids)?;
            for layer in &inbound_layers {
                let qty_in = qty_of(layer, "qty_in")?;
                let remaining = qty_of(layer, "remaining_qty")?;
                if remaining != qty_in {
                    return Err(Error::msg(format!(
                        "destination layer {} was already used; void later moves first",
                        id_of(layer)?
                    )));
                }
                tx = tx.update(
                    "stock_layer",
                    id_of(layer)?,
                    &json!({
                        "remaining_qty": Decimal::zero(QTY_SCALE),
                        "remaining_value": Decimal::zero(VALUE_SCALE),
                    }),
                );
            }
        }
        _ => return Err(Error::msg("this move kind cannot be voided")),
    }

    let void_move = json!({
        "company": company_id,
        "kind": "void",
        "state": "posted",
        "posting_date": posting_date,
        "currency": currency,
        "total_value": total_value,
        "number": number,
        "seq": seq,
        "reverses": input.id,
        "reason": input.reason.trim(),
        "source_version": 1,
    });
    tx = tx.create("stock_move", &void_move);
    tx = tx.update("stock_move", &input.id, &json!({ "state": "void" }));
    if let Some(cid) = counter_id {
        tx = tx.update("stock_counter", &cid, &json!({ "next": seq + 1 }));
    }
    let done = tx.run().map_err(|e| e.or("could not void the move"))?;
    let voided: Record = done
        .iter()
        .rev()
        .find_map(|v| {
            let r: Record = serde_json::from_value(v.clone()).ok()?;
            if text(&r, "kind") == Some("void") {
                Some(r)
            } else {
                None
            }
        })
        .or_else(|| done.iter().find_map(|v| serde_json::from_value(v.clone()).ok()))
        .ok_or_else(|| Error::msg("the void move was not returned"))?;

    let original_total = value_of(&original, "total_value")?;
    let adjust_increase = if kind == "adjust" {
        Some(lines.iter().any(|l| value_of(l, "unit_cost").map(|c| !c.is_zero()).unwrap_or(false)))
    } else {
        None
    };
    let amounts = flip_stock_amounts(&reclass_for_source(
        stock_event_amounts(&kind, original_total, &[], adjust_increase)?,
        &kind,
        text(&original, "source_kind"),
    ))?;
    let mut payload = amounts;
    let obj = payload.as_object_mut().unwrap();
    obj.insert("move".into(), json!(input.id));
    obj.insert("void_move".into(), json!(text(&voided, "id")));
    obj.insert("number".into(), json!(text(&original, "number")));
    obj.insert("kind".into(), json!(kind));
    obj.insert("company".into(), json!(company_id));
    obj.insert("currency".into(), json!(currency));
    obj.insert("posting_date".into(), json!(posting_date));
    obj.insert("source_version".into(), json!(original.get("source_version").and_then(Value::as_i64).unwrap_or(1)));
    obj.insert("reason".into(), json!(input.reason.trim()));
    events::emit("stock_voided", &payload)?;
    Ok(voided)
}

fn do_on_hand(input: OnHandQuery) -> Result<Value> {
    require_reader()?;
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    let open = open_layers(
        id_of(&company)?,
        id_of(&item)?,
        id_of(&warehouse)?,
        input.lot.as_deref().filter(|s| !s.is_empty()),
    )?;
    let layers: Vec<Layer> = open.into_iter().map(|(_, l)| l).collect();
    let (qty, value) = on_hand_of(&layers)?;
    let reserved = crate::reserve::reserved_qty(id_of(&company)?, id_of(&item)?, id_of(&warehouse)?, input.lot.as_deref())?;
    let available = qty.checked_sub(reserved)?;
    Ok(json!({
        "item": id_of(&item)?,
        "warehouse": id_of(&warehouse)?,
        "qty": qty,
        "value": value,
        "reserved": reserved,
        "available": available,
        "layers": layers.len(),
    }))
}

fn do_get_move(input: Id) -> Result<Value> {
    require_reader()?;
    let move_row = require("stock_move", &input.id, "move")?;
    let lines = lines_of(&input.id)?;
    Ok(json!({ "move": move_row, "lines": lines }))
}

/// In-crate entry points for plugins parts (counts) that build moves without going through the handler boundary.
pub(crate) fn api_draft(kind: &str, input: Value) -> Result<Record> {
    draft(kind, serde_json::from_value(input).map_err(|e| Error::msg(format!("draft: {e}")))?)
}

pub(crate) fn api_add_line(input: Value) -> Result<Record> {
    do_add_line(serde_json::from_value(input).map_err(|e| Error::msg(format!("line: {e}")))?)
}

pub(crate) fn api_post(id: &str) -> Result<Record> {
    do_post_move(Id { id: id.to_string() })
}

/// Opening stock (`opening.rs`): the same draft, line and post steps as any other move.
pub(crate) fn opening_draft(company: &str, posting_date: &str, idem_key: &str) -> Result<Record> {
    draft(
        "adjust",
        DraftInput {
            company: company.to_string(),
            posting_date: Some(posting_date.to_string()),
            currency: None,
            source_kind: Some("opening_stock".into()),
            source_id: None,
            source_version: None,
            idem_key: Some(idem_key.to_string()),
            reason: Some("Opening stock".into()),
        },
    )
}

pub(crate) fn opening_line(
    move_id: &str,
    item: &str,
    warehouse: &str,
    qty: Decimal,
    unit_cost: Decimal,
    lot: Option<&str>,
    expiry_date: Option<&str>,
) -> Result<Record> {
    do_add_line(AddLine {
        move_id: move_id.to_string(),
        item: item.to_string(),
        qty,
        warehouse: warehouse.to_string(),
        warehouse_from: None,
        lot: lot.map(str::to_string),
        unit_cost: Some(unit_cost),
        increase: Some(true),
        expiry_date: expiry_date.map(str::to_string),
    })
}

pub(crate) fn opening_post(id: &str) -> Result<Record> {
    do_post_move(Id { id: id.to_string() })
}

handler! {
    fn draft_receipt(input: DraftInput) -> Record { draft("receipt", input) }
    fn draft_issue(input: DraftInput) -> Record { draft("issue", input) }
    fn draft_transfer(input: DraftInput) -> Record { draft("transfer", input) }
    fn draft_adjust(input: DraftInput) -> Record { draft("adjust", input) }
    fn draft_manufacture_consume(input: DraftInput) -> Record { draft("manufacture_consume", input) }
    fn draft_manufacture_produce(input: DraftInput) -> Record { draft("manufacture_produce", input) }
    fn add_line(input: AddLine) -> Record { do_add_line(input) }
    fn post_move(input: Id) -> Record { do_post_move(input) }
    fn post_manufacture_pair(input: ManufacturePair) -> Value { do_post_manufacture_pair(input) }
    fn outbound_value(input: Id) -> Value { do_outbound_value(input) }
    fn void_move(input: VoidInput) -> Record { do_void_move(input) }
    fn on_hand(input: OnHandQuery) -> Value { do_on_hand(input) }
    fn get_move(input: Id) -> Value { do_get_move(input) }
}

#[cfg(test)]
mod return_tests {
    use super::*;

    fn amount(v: &Value, k: &str) -> String {
        v[k].as_str().map(str::to_string).unwrap_or_else(|| v[k].to_string())
    }

    #[test]
    fn supplier_return_clears_grni_not_cogs() {
        let total = Decimal::parse("40").unwrap();
        let base = stock_event_amounts("issue", total, &[], None).unwrap();
        let out = reclass_for_source(base, "issue", Some(PURCHASE_RETURN_SOURCE));
        assert_eq!(amount(&out, "cogs").trim_matches('"'), "0.00");
        assert_eq!(amount(&out, "clearing_debit").trim_matches('"'), "40.00");
        assert_eq!(amount(&out, "inventory_out").trim_matches('"'), "40.00");
    }

    #[test]
    fn customer_return_credits_cogs_not_grni() {
        let total = Decimal::parse("25").unwrap();
        let base = stock_event_amounts("receipt", total, &[], None).unwrap();
        let out = reclass_for_source(base, "receipt", Some(SALES_RETURN_SOURCE));
        assert_eq!(amount(&out, "clearing").trim_matches('"'), "0.00");
        assert_eq!(amount(&out, "cogs_credit").trim_matches('"'), "25.00");
    }

    #[test]
    fn plain_moves_are_untouched() {
        let total = Decimal::parse("10").unwrap();
        let base = stock_event_amounts("issue", total, &[], None).unwrap();
        let out = reclass_for_source(base.clone(), "issue", Some("erp_selling.so"));
        assert_eq!(out, base);
    }
}
