//! Append-only stock reservations.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_reader, require_user, text, Record};
use crate::moves::open_layers;
use crate::rules::{on_hand as on_hand_of, Layer, QTY_SCALE};

#[derive(Deserialize)]
struct ReserveInput {
    company: String,
    item: String,
    warehouse: String,
    qty: Decimal,
    demand_kind: String,
    demand_id: String,
    #[serde(default)]
    lot: Option<String>,
}

#[derive(Deserialize)]
struct ReservedQuery {
    company: String,
    item: String,
    warehouse: String,
    #[serde(default)]
    lot: Option<String>,
    /// With both, the answer also says how much of the hold belongs to that demand.
    #[serde(default)]
    demand_kind: Option<String>,
    #[serde(default)]
    demand_id: Option<String>,
}

pub(crate) fn reserved_qty(company: &str, item: &str, warehouse: &str, lot: Option<&str>) -> Result<Decimal> {
    let mut rows: Vec<Record> = db::find("stock_reservation")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .limit(5000)
        .all()?;
    if rows.len() >= 5000 {
        return Err(Error::msg("too many reservation rows for this item and warehouse"));
    }
    if let Some(lot) = lot {
        rows.retain(|r| text(r, "lot") == Some(lot));
    } else {
        rows.retain(|r| text(r, "lot").unwrap_or("").is_empty());
    }
    rows.iter().try_fold(Decimal::zero(QTY_SCALE), |acc, r| acc.checked_add(qty_of(r, "qty")?))
}

/// Net qty reserved for one demand (reserve rows minus releases).
pub(crate) fn reserved_for_demand(
    company: &str,
    item: &str,
    warehouse: &str,
    lot: Option<&str>,
    demand_kind: &str,
    demand_id: &str,
) -> Result<Decimal> {
    let mut rows: Vec<Record> = db::find("stock_reservation")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .filter("demand_kind", demand_kind)
        .filter("demand_id", demand_id)
        .limit(5000)
        .all()?;
    if let Some(lot) = lot {
        rows.retain(|r| text(r, "lot") == Some(lot));
    } else {
        rows.retain(|r| text(r, "lot").unwrap_or("").is_empty());
    }
    let net = rows.iter().try_fold(Decimal::zero(QTY_SCALE), |acc, r| acc.checked_add(qty_of(r, "qty")?))?;
    if net.is_negative() {
        Ok(Decimal::zero(QTY_SCALE))
    } else {
        Ok(net)
    }
}

fn next_seq(company: &str, item: &str, warehouse: &str) -> Result<i64> {
    let rows: Vec<Record> = db::find("stock_reservation")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .limit(5000)
        .all()?;
    Ok(rows.iter().map(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1)
}

fn do_reserve(input: ReserveInput) -> Result<Record> {
    require_user()?;
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("reserve quantity must be greater than zero"));
    }
    if input.demand_kind.trim().is_empty() || input.demand_id.trim().is_empty() {
        return Err(Error::msg("name the demand (kind and id)"));
    }
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    let company_id = id_of(&company)?.to_string();
    let item_id = id_of(&item)?.to_string();
    let warehouse_id = id_of(&warehouse)?.to_string();
    let lot = input.lot.filter(|s| !s.is_empty());

    let open = open_layers(&company_id, &item_id, &warehouse_id, lot.as_deref())?;
    let layers: Vec<Layer> = open.into_iter().map(|(_, l)| l).collect();
    let (on_hand, _) = on_hand_of(&layers)?;
    let reserved = reserved_qty(&company_id, &item_id, &warehouse_id, lot.as_deref())?;
    let available = on_hand.checked_sub(reserved)?;
    if qty > available {
        return Err(Error::msg(format!("not enough available stock: need {qty}, available {available}")));
    }

    let seq = next_seq(&company_id, &item_id, &warehouse_id)?;
    let mut data = json!({
        "company": company_id,
        "item": item_id,
        "warehouse": warehouse_id,
        "qty": qty,
        "demand_kind": input.demand_kind.trim(),
        "demand_id": input.demand_id.trim(),
        "seq": seq,
    });
    if let Some(lot) = &lot {
        data["lot"] = json!(lot);
    }
    db::create("stock_reservation", &data).map_err(|e| e.or("could not reserve"))
}

fn do_release(input: ReserveInput) -> Result<Record> {
    require_user()?;
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("release quantity must be greater than zero"));
    }
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    let company_id = id_of(&company)?.to_string();
    let item_id = id_of(&item)?.to_string();
    let warehouse_id = id_of(&warehouse)?.to_string();
    let lot = input.lot.filter(|s| !s.is_empty());

    // Open reserved for this demand
    let mut rows: Vec<Record> = db::find("stock_reservation")
        .filter("company", &company_id)
        .filter("item", &item_id)
        .filter("warehouse", &warehouse_id)
        .filter("demand_kind", input.demand_kind.trim())
        .filter("demand_id", input.demand_id.trim())
        .limit(5000)
        .all()?;
    if let Some(lot) = lot.as_deref() {
        rows.retain(|r| text(r, "lot") == Some(lot));
    } else {
        rows.retain(|r| text(r, "lot").unwrap_or("").is_empty());
    }
    let open = rows.iter().try_fold(Decimal::zero(QTY_SCALE), |acc, r| acc.checked_add(qty_of(r, "qty")?))?;
    if qty > open {
        return Err(Error::msg(format!("cannot release {qty}: only {open} reserved for this demand")));
    }

    let seq = next_seq(&company_id, &item_id, &warehouse_id)?;
    let neg = Decimal::zero(QTY_SCALE).checked_sub(qty)?;
    let mut data = json!({
        "company": company_id,
        "item": item_id,
        "warehouse": warehouse_id,
        "qty": neg,
        "demand_kind": input.demand_kind.trim(),
        "demand_id": input.demand_id.trim(),
        "seq": seq,
    });
    if let Some(lot) = &lot {
        data["lot"] = json!(lot);
    }
    db::create("stock_reservation", &data).map_err(|e| e.or("could not release"))
}

fn do_reserved(input: ReservedQuery) -> Result<Value> {
    require_reader()?;
    let company = require("company.company", &input.company, "company")?;
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    let warehouse = require("stock_warehouse", &input.warehouse, "warehouse")?;
    let qty = reserved_qty(
        id_of(&company)?,
        id_of(&item)?,
        id_of(&warehouse)?,
        input.lot.as_deref().filter(|s| !s.is_empty()),
    )?;
    let mut out = json!({
        "item": id_of(&item)?,
        "warehouse": id_of(&warehouse)?,
        "reserved": qty,
    });
    if let (Some(kind), Some(id)) = (input.demand_kind.as_deref(), input.demand_id.as_deref()) {
        out["for_demand"] = json!(reserved_for_demand(
            id_of(&company)?,
            id_of(&item)?,
            id_of(&warehouse)?,
            input.lot.as_deref().filter(|s| !s.is_empty()),
            kind,
            id,
        )?);
    }
    Ok(out)
}

handler! {
    fn reserve(input: ReserveInput) -> Record { do_reserve(input) }
    fn release(input: ReserveInput) -> Record { do_release(input) }
    fn reserved(input: ReservedQuery) -> Value { do_reserved(input) }
}
