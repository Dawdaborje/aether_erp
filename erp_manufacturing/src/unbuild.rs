//! Unbuild: take finished goods of a completed order back out of stock and return its components.
//!
//! The finished goods leave as a `manufacture_consume` at their layer cost and the components come
//! back as a `manufacture_produce`, so work in progress nets to zero in the books. The value is
//! shared over the components in proportion to what the order consumed. Byproducts and labour are
//! not reversed.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_user, text, today, Record, QTY_SCALE, VALUE_SCALE};

#[derive(Deserialize)]
struct UnbuildInput {
    #[serde(alias = "id")]
    mo: String,
    /// Finished quantity to take apart (default: all not yet unbuilt).
    #[serde(default)]
    qty: Option<Decimal>,
    #[serde(default)]
    reason: Option<String>,
}

fn money(record: &Value, key: &str) -> Result<Decimal> {
    match record.get(key).filter(|v| !v.is_null()) {
        Some(v) => serde_json::from_value::<Decimal>(v.clone())
            .map_err(|e| Error::msg(format!("{key}: {e}")))?
            .with_scale(VALUE_SCALE),
        None => Ok(Decimal::zero(VALUE_SCALE)),
    }
}

fn do_unbuild(input: UnbuildInput) -> Result<Value> {
    require_user()?;
    let mo = require("mo", &input.mo, "manufacturing order")?;
    if text(&mo, "state") != Some("done") {
        return Err(Error::msg("only a completed order can be unbuilt"));
    }
    let mo_id = id_of(&mo)?.to_string();
    let mo_qty = qty_of(&mo, "qty")?;
    let earlier: Vec<Record> = db::find("mo_unbuild").filter("mo", mo_id.as_str()).limit(500).all()?;
    let mut already = Decimal::zero(QTY_SCALE);
    for row in &earlier {
        already = already.checked_add(qty_of(row, "qty")?)?;
    }
    let left = mo_qty.checked_sub(already)?;
    let qty = match input.qty {
        Some(q) => q.with_scale(QTY_SCALE)?,
        None => left,
    };
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("there is nothing left to unbuild"));
    }
    if qty > left {
        return Err(Error::msg("that is more than is left of the order's finished quantity"));
    }
    let consume_move = text(&mo, "consume_move").ok_or_else(|| Error::msg("the order has no consume move"))?;
    let original: Value = plugins::call("erp_inventory", "get_move", &json!({ "id": consume_move }))?;
    let lines = original.get("lines").and_then(Value::as_array).cloned().unwrap_or_default();
    if lines.is_empty() {
        return Err(Error::msg("the order's consume move has no lines"));
    }

    let company = text(&mo, "company").unwrap_or_default().to_string();
    let warehouse = text(&mo, "warehouse").unwrap_or_default().to_string();
    let fg = text(&mo, "item").unwrap_or_default().to_string();
    let date = today()?;
    let key = format!("erp_manufacturing.unbuild:{mo_id}:{}:{}", earlier.len(), qty);
    let source = json!({ "company": company, "posting_date": date, "source_kind": "erp_manufacturing.unbuild", "source_id": mo_id });

    // Finished goods out.
    let mut out_draft = source.clone();
    out_draft["idem_key"] = json!(format!("{key}:out"));
    let out: Record = plugins::call("erp_inventory", "draft_manufacture_consume", &out_draft)?;
    let out_id = id_of(&out)?.to_string();
    let _: Record = plugins::call(
        "erp_inventory",
        "add_line",
        &json!({ "move": out_id, "item": fg, "qty": qty, "warehouse": warehouse }),
    )?;
    let value: Value = plugins::call("erp_inventory", "outbound_value", &json!({ "id": out_id }))?;
    let total = money(&value, "total_value")?;

    // Components back, sharing that value by what the order consumed.
    let mut weights = Vec::new();
    let mut weight_sum = Decimal::zero(VALUE_SCALE);
    for line in &lines {
        let w = money(line, "value")?;
        weight_sum = weight_sum.checked_add(w)?;
        weights.push(w);
    }
    let mut in_draft = source;
    in_draft["idem_key"] = json!(format!("{key}:in"));
    let back: Record = plugins::call("erp_inventory", "draft_manufacture_produce", &in_draft)?;
    let back_id = id_of(&back)?.to_string();
    let ratio_num = qty;
    for (line, weight) in lines.iter().zip(&weights) {
        let item = text(line, "item").unwrap_or_default();
        let original_qty = qty_of(line, "qty")?;
        let comp_qty = original_qty.times(ratio_num, QTY_SCALE)?.div(mo_qty, QTY_SCALE)?;
        if comp_qty.is_zero() {
            continue;
        }
        let share = if weight_sum.is_zero() {
            total.div(Decimal::whole(lines.len() as i64, 0)?, VALUE_SCALE)?
        } else {
            total.times(*weight, VALUE_SCALE)?.div(weight_sum, VALUE_SCALE)?
        };
        let unit_cost = share.div(comp_qty, VALUE_SCALE)?;
        let _: Record = plugins::call(
            "erp_inventory",
            "add_line",
            &json!({
                "move": back_id, "item": item, "qty": comp_qty, "warehouse": warehouse,
                "unit_cost": unit_cost, "lot": line.get("lot").cloned().unwrap_or(Value::Null),
            }),
        )?;
    }
    let pair: Value = plugins::call(
        "erp_inventory",
        "post_manufacture_pair",
        &json!({ "consume": out_id, "produce": back_id }),
    )?;
    let row: Record = db::create(
        "mo_unbuild",
        &json!({
            "mo": mo_id,
            "qty": qty,
            "posting_date": date,
            "consume_move": out_id,
            "produce_move": back_id,
            "reason": input.reason.unwrap_or_default().trim(),
        }),
    )?;
    Ok(json!({ "unbuild": row, "moves": pair }))
}

handler! {
    fn unbuild_mo(input: UnbuildInput) -> Value { do_unbuild(input) }
}
