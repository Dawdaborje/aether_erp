//! BOMs and manufacturing orders.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_reader, require_user, text, today, Record, PCT_SCALE, QTY_SCALE, VALUE_SCALE};
use crate::rules::{explode, BomComponent};

#[derive(Deserialize)]
struct CreateBom {
    company: String,
    code: String,
    item: String,
    #[serde(default)]
    qty: Option<Decimal>,
}

#[derive(Deserialize)]
struct AddBomLine {
    #[serde(alias = "id")]
    bom: String,
    item: String,
    qty: Decimal,
    #[serde(default)]
    scrap_pct: Option<Decimal>,
    #[serde(default)]
    is_byproduct: Option<bool>,
    /// Make this component from another BOM (multi-level).
    #[serde(default)]
    sub_bom: Option<String>,
}

#[derive(Deserialize)]
struct CreateMo {
    company: String,
    bom: String,
    qty: Decimal,
    warehouse: String,
    #[serde(default)]
    planned_date: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

pub(crate) fn bom_lines(bom_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("bom_line").filter("bom", bom_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

pub(crate) fn components_of(bom_id: &str) -> Result<Vec<BomComponent>> {
    bom_lines(bom_id)?
        .into_iter()
        .map(|row| {
            Ok(BomComponent {
                item_id: text(&row, "item").unwrap_or_default().to_string(),
                qty_per: qty_of(&row, "qty")?,
                scrap_pct: crate::common::dec(&row, "scrap_pct", PCT_SCALE)?,
                is_byproduct: row.get("is_byproduct") == Some(&json!(true)),
            })
        })
        .collect()
}

pub(crate) fn mo_components(mo_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("mo_component").filter("mo", mo_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn snapshot_components(mo_id: &str) -> Result<Vec<BomComponent>> {
    mo_components(mo_id)?
        .into_iter()
        .map(|row| {
            Ok(BomComponent {
                item_id: text(&row, "item").unwrap_or_default().to_string(),
                qty_per: qty_of(&row, "qty_per")?,
                scrap_pct: crate::common::dec(&row, "scrap_pct", PCT_SCALE)?,
                is_byproduct: row.get("is_byproduct") == Some(&json!(true)),
            })
        })
        .collect()
}

pub(crate) fn exploded_for_mo(mo: &Record) -> Result<Vec<crate::rules::ExplodedLine>> {
    let bom_qty = match qty_of(mo, "bom_qty") {
        Ok(q) if !q.is_zero() => q,
        _ => {
            let bom = require("bom", text(mo, "bom").unwrap_or_default(), "BOM")?;
            qty_of(&bom, "qty")?
        }
    };
    let components = match snapshot_components(id_of(mo)?) {
        Ok(c) if !c.is_empty() => c,
        _ => components_of(text(mo, "bom").unwrap_or_default())?,
    };
    explode(bom_qty, qty_of(mo, "qty")?, &components)
}

fn do_create_bom(input: CreateBom) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("is_stocked") == Some(&json!(false)) {
        return Err(Error::msg("the finished item must be stocked"));
    }
    let qty = input.qty.unwrap_or_else(|| Decimal::parse("1").unwrap()).with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("BOM output quantity must be greater than zero"));
    }
    let uom = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?;
    db::create(
        "bom",
        &json!({
            "code": input.code.trim(),
            "company": id_of(&company)?,
            "item": id_of(&item)?,
            "qty": qty,
            "uom": uom,
            "active": true,
            "version": 1,
        }),
    )
    .map_err(|e| e.or("could not create the BOM (the code may be taken)"))
}

fn do_add_bom_line(input: AddBomLine) -> Result<Record> {
    require_user()?;
    let bom = require("bom", &input.bom, "BOM")?;
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("component quantity must be greater than zero"));
    }
    let scrap = input.scrap_pct.unwrap_or_else(|| Decimal::zero(PCT_SCALE)).with_scale(PCT_SCALE)?;
    if scrap.is_negative() {
        return Err(Error::msg("scrap percent is not negative"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if text(&item, "id") == text(&bom, "item") && input.is_byproduct != Some(true) {
        return Err(Error::msg("the finished item is not a material line"));
    }
    let uom = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?;
    let existing = bom_lines(id_of(&bom)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "bom": id_of(&bom)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": uom,
        "scrap_pct": scrap,
        "is_byproduct": input.is_byproduct.unwrap_or(false),
    });
    if let Some(sub_id) = input.sub_bom.filter(|s| !s.is_empty()) {
        if input.is_byproduct == Some(true) {
            return Err(Error::msg("a byproduct cannot be made from a BOM"));
        }
        let sub = require("bom", &sub_id, "sub-assembly BOM")?;
        if text(&sub, "item") != Some(id_of(&item)?) {
            return Err(Error::msg("the sub-assembly BOM makes a different item than this line"));
        }
        if crate::multilevel::reaches(&sub_id, id_of(&bom)?)? {
            return Err(Error::msg("a BOM cannot contain itself, directly or through its sub-assemblies"));
        }
        data["sub_bom"] = json!(sub_id);
    }
    db::create("bom_line", &data).map_err(|e| e.or("could not add the BOM line"))
}

fn do_create_mo(input: CreateMo) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let bom = require("bom", &input.bom, "BOM")?;
    if bom.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the BOM is not active"));
    }
    if text(&bom, "company") != Some(id_of(&company)?) {
        return Err(Error::msg("the BOM belongs to another company"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("order quantity must be greater than zero"));
    }
    if bom_lines(id_of(&bom)?)?.is_empty() {
        return Err(Error::msg("the BOM has no lines"));
    }
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let mut data = json!({
        "company": id_of(&company)?,
        "bom": id_of(&bom)?,
        "item": text(&bom, "item"),
        "qty": qty,
        "warehouse": id_of(&warehouse)?,
        "state": "draft",
    });
    if let Some(d) = input.planned_date {
        parse_date(&d)?;
        data["planned_date"] = json!(d);
    }
    db::create("mo", &data).map_err(|e| e.or("could not create the manufacturing order"))
}

fn do_confirm_mo(input: Id) -> Result<Record> {
    require_user()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    if text(&mo, "state") != Some("draft") {
        return Err(Error::msg("only a draft order can be confirmed"));
    }
    let bom = require("bom", text(&mo, "bom").unwrap_or_default(), "BOM")?;
    let bom_qty = qty_of(&bom, "qty")?;
    let lines = bom_lines(id_of(&bom)?)?;
    if lines.is_empty() {
        return Err(Error::msg("the BOM has no lines"));
    }
    if !mo_components(&input.id)?.is_empty() {
        return Err(Error::msg("this order already has a BOM snapshot"));
    }
    for row in &lines {
        let _: Record = db::create(
            "mo_component",
            &json!({
                "mo": input.id,
                "line_no": row.get("line_no"),
                "item": text(row, "item"),
                "qty_per": qty_of(row, "qty")?,
                "scrap_pct": crate::common::dec(row, "scrap_pct", PCT_SCALE)?,
                "is_byproduct": row.get("is_byproduct") == Some(&json!(true)),
            }),
        )?;
    }
    crate::routing::snapshot_operations(&input.id, id_of(&bom)?, qty_of(&mo, "qty")?, bom_qty)?;
    // The kernel numbers the order as it enters `confirmed` (see `sequence` on `mo.number`).
    let _: Option<Record> = db::update("mo", &input.id, &json!({ "state": "confirmed", "bom_qty": bom_qty }))?;
    require("mo", &input.id, "manufacturing order")
}

fn do_reserve_mo(input: Id) -> Result<Record> {
    require_user()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    if text(&mo, "state") == Some("reserved") {
        return Ok(mo);
    }
    if text(&mo, "state") != Some("confirmed") {
        return Err(Error::msg("reserve a confirmed order"));
    }
    let exploded = exploded_for_mo(&mo)?;
    let company = text(&mo, "company").unwrap_or_default();
    let warehouse = text(&mo, "warehouse").unwrap_or_default();
    for line in exploded.iter().filter(|l| !l.is_byproduct) {
        let _: Record = plugins::call(
            "erp_inventory",
            "reserve",
            &json!({
                "company": company,
                "item": line.item_id,
                "warehouse": warehouse,
                "qty": line.qty,
                "demand_kind": "erp_manufacturing.mo",
                "demand_id": input.id,
            }),
        )?;
    }
    db::update("mo", &input.id, &json!({ "state": "reserved" }))?.ok_or_else(|| Error::msg("the order is gone"))
}

fn release_reservations(mo_id: &str, company: &str, warehouse: &str, exploded: &[crate::rules::ExplodedLine]) -> Result<()> {
    for line in exploded.iter().filter(|l| !l.is_byproduct) {
        let _: Record = plugins::call(
            "erp_inventory",
            "release",
            &json!({
                "company": company,
                "item": line.item_id,
                "warehouse": warehouse,
                "qty": line.qty,
                "demand_kind": "erp_manufacturing.mo",
                "demand_id": mo_id,
            }),
        )?;
    }
    Ok(())
}

fn do_complete_mo(input: Id) -> Result<Value> {
    require_user()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    if text(&mo, "state") == Some("done") {
        return Ok(json!({
            "mo": mo,
            "consume_move": text(&mo, "consume_move"),
            "produce_move": text(&mo, "produce_move"),
        }));
    }
    if text(&mo, "consume_move").is_some() || text(&mo, "produce_move").is_some() {
        return Err(Error::msg("this order already has stock moves; void them before completing again"));
    }
    if !matches!(text(&mo, "state"), Some("confirmed" | "reserved" | "in_progress")) {
        return Err(Error::msg("complete a confirmed or reserved order"));
    }
    crate::multilevel::assert_children_done(&input.id)?;
    let labour_cost = crate::routing::labour_cost(&input.id)?;
    let mo_qty = qty_of(&mo, "qty")?;
    let exploded = exploded_for_mo(&mo)?;
    let company = text(&mo, "company").unwrap_or_default().to_string();
    let warehouse = text(&mo, "warehouse").unwrap_or_default().to_string();
    let posting_date = today()?;
    let materials: Vec<_> = exploded.iter().filter(|l| !l.is_byproduct).cloned().collect();
    let byproducts: Vec<_> = exploded.iter().filter(|l| l.is_byproduct).cloned().collect();
    if materials.is_empty() {
        return Err(Error::msg("the BOM has no materials to consume"));
    }

    let was_reserved = text(&mo, "state") == Some("reserved");
    let _: Option<Record> = db::update("mo", &input.id, &json!({ "state": "in_progress" }))?;

    let consume_key = format!("erp_manufacturing.mo:{}:consume", input.id);
    let produce_key = format!("erp_manufacturing.mo:{}:produce", input.id);

    let consume: Record = plugins::call(
        "erp_inventory",
        "draft_manufacture_consume",
        &json!({
            "company": company,
            "posting_date": posting_date,
            "source_kind": "erp_manufacturing.mo",
            "source_id": input.id,
            "source_version": 1,
            "idem_key": consume_key,
        }),
    )?;
    let consume_id = id_of(&consume)?.to_string();
    if text(&consume, "state") != Some("posted") {
        let existing: Value = plugins::call("erp_inventory", "get_move", &json!({ "id": consume_id }))?;
        let has_lines = existing.get("lines").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
        if !has_lines {
            for line in &materials {
                let _: Record = plugins::call(
                    "erp_inventory",
                    "add_line",
                    &json!({
                        "move": consume_id,
                        "item": line.item_id,
                        "qty": line.qty,
                        "warehouse": warehouse,
                    }),
                )?;
            }
        }
    }

    let value: Value = plugins::call("erp_inventory", "outbound_value", &json!({ "id": consume_id }))?;
    let consume_value = match value.get("total_value") {
        Some(v) if !v.is_null() => {
            let parsed: Decimal = serde_json::from_value(v.clone()).map_err(|e| Error::msg(format!("consume value: {e}")))?;
            parsed.with_scale(VALUE_SCALE)?
        }
        _ => return Err(Error::msg("consume value missing")),
    };
    // Finished goods carry materials plus the labour and overhead absorbed by the operations.
    let unit_cost = if mo_qty.is_zero() {
        Decimal::zero(VALUE_SCALE)
    } else {
        consume_value.checked_add(labour_cost)?.div(mo_qty, VALUE_SCALE)?
    };

    let produce: Record = plugins::call(
        "erp_inventory",
        "draft_manufacture_produce",
        &json!({
            "company": company,
            "posting_date": posting_date,
            "source_kind": "erp_manufacturing.mo",
            "source_id": input.id,
            "source_version": 2,
            "idem_key": produce_key,
        }),
    )?;
    let produce_id = id_of(&produce)?.to_string();
    let fg = text(&mo, "item").unwrap_or_default();
    if text(&produce, "state") != Some("posted") {
        let existing: Value = plugins::call("erp_inventory", "get_move", &json!({ "id": produce_id }))?;
        let has_lines = existing.get("lines").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
        if !has_lines {
            let _: Record = plugins::call(
                "erp_inventory",
                "add_line",
                &json!({
                    "move": produce_id,
                    "item": fg,
                    "qty": mo_qty,
                    "warehouse": warehouse,
                    "unit_cost": unit_cost,
                }),
            )?;
            for line in &byproducts {
                let _: Record = plugins::call(
                    "erp_inventory",
                    "add_line",
                    &json!({
                        "move": produce_id,
                        "item": line.item_id,
                        "qty": line.qty,
                        "warehouse": warehouse,
                        "unit_cost": Decimal::zero(VALUE_SCALE),
                    }),
                )?;
            }
        }
    }

    let pair: Value = plugins::call(
        "erp_inventory",
        "post_manufacture_pair",
        &json!({ "consume": consume_id, "produce": produce_id }),
    )?;
    let consume_posted = pair.get("consume").cloned().unwrap_or(Value::Null);
    let produce_posted = pair.get("produce").cloned().unwrap_or(Value::Null);

    if was_reserved {
        release_reservations(&input.id, &company, &warehouse, &exploded)?;
    }

    let done = db::update(
        "mo",
        &input.id,
        &json!({
            "state": "done",
            "consume_move": consume_id,
            "produce_move": produce_id,
            "labour_cost": labour_cost,
        }),
    )?
    .ok_or_else(|| Error::msg("the order is gone"))?;

    events::emit(
        "mo_completed",
        &json!({
            "mo": input.id,
            "number": text(&done, "number"),
            "item": fg,
            "qty": mo_qty,
            "company": company,
            "consume_move": consume_id,
            "produce_move": produce_id,
            "consume_value": consume_value,
            "labour_cost": labour_cost,
            "currency": crate::common::company_currency_code(&company)?,
            "posting_date": posting_date,
            "source_version": 1,
        }),
    )?;

    Ok(json!({ "mo": done, "consume": consume_posted, "produce": produce_posted }))
}

fn do_cancel_mo(input: Id) -> Result<Record> {
    require_user()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    if matches!(text(&mo, "state"), Some("done" | "cancelled")) {
        return Err(Error::msg("this order cannot be cancelled"));
    }
    if text(&mo, "state") == Some("reserved") {
        let exploded = exploded_for_mo(&mo)?;
        release_reservations(
            &input.id,
            text(&mo, "company").unwrap_or_default(),
            text(&mo, "warehouse").unwrap_or_default(),
            &exploded,
        )?;
    }
    if text(&mo, "state") == Some("in_progress") {
        return Err(Error::msg("void the stock moves before cancelling an order in progress"));
    }
    db::update("mo", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the order is gone"))
}

fn do_get_mo(input: Id) -> Result<Value> {
    require_reader()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    let bom = require("bom", text(&mo, "bom").unwrap_or_default(), "BOM")?;
    let lines = bom_lines(id_of(&bom)?)?;
    let snapshot = mo_components(&input.id)?;
    let exploded = exploded_for_mo(&mo)?;
    Ok(json!({
        "mo": mo,
        "bom": bom,
        "bom_lines": lines,
        "mo_components": snapshot,
        "exploded": exploded,
    }))
}

fn do_get_bom(input: Id) -> Result<Value> {
    require_reader()?;
    let bom = require("bom", &input.id, "BOM")?;
    let lines = bom_lines(&input.id)?;
    Ok(json!({ "bom": bom, "lines": lines }))
}

handler! {
    fn create_bom(input: CreateBom) -> Record { do_create_bom(input) }
    fn add_bom_line(input: AddBomLine) -> Record { do_add_bom_line(input) }
    fn create_mo(input: CreateMo) -> Record { do_create_mo(input) }
    fn confirm_mo(input: Id) -> Record { do_confirm_mo(input) }
    fn reserve_mo(input: Id) -> Record { do_reserve_mo(input) }
    fn complete_mo(input: Id) -> Value { do_complete_mo(input) }
    fn cancel_mo(input: Id) -> Record { do_cancel_mo(input) }
    fn get_mo(input: Id) -> Value { do_get_mo(input) }
    fn get_bom(input: Id) -> Value { do_get_bom(input) }
}
