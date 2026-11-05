//! Landed cost: freight, duty and handling added to a received lot's cost after the fact.
//!
//! Ledger history is not rewritten. The charges are appended as allocation rows, the open layers the receipt
//! created carry the extra value forward, and the part for goods already issued goes to cost of sales through
//! a `stock_revalued` event.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_user, text, today, value_of, Record};
use crate::rules::{share_out, split_on_hand, VALUE_SCALE};

#[derive(Deserialize)]
struct Create {
    company: String,
    #[serde(rename = "move")]
    move_id: String,
    #[serde(default)]
    reference: Option<String>,
    #[serde(default)]
    basis: Option<String>,
}

#[derive(Deserialize)]
struct AddCharge {
    #[serde(alias = "id")]
    landed: String,
    description: String,
    amount: Decimal,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn do_create(input: Create) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let mv = require("stock_move", &input.move_id, "move")?;
    if text(&mv, "kind") != Some("receipt") || text(&mv, "state") != Some("posted") {
        return Err(Error::msg("landed cost goes on a posted receipt"));
    }
    if text(&mv, "company") != Some(id_of(&company)?) {
        return Err(Error::msg("the receipt belongs to another company"));
    }
    let basis = input.basis.as_deref().unwrap_or("value");
    if !matches!(basis, "value" | "qty") {
        return Err(Error::msg("basis is value or qty"));
    }
    let mut data = json!({
        "company": id_of(&company)?,
        "move": input.move_id,
        "basis": basis,
        "state": "draft",
        "total": Decimal::zero(2),
    });
    if let Some(r) = input.reference.filter(|r| !r.trim().is_empty()) {
        data["reference"] = json!(r.trim());
    }
    db::create("stock_landed_cost", &data).map_err(|e| e.or("could not create the landed cost"))
}

fn charges_of(landed_id: &str) -> Result<Vec<Record>> {
    db::find("stock_landed_charge").filter("landed", landed_id).limit(500).all()
}

fn total_of(charges: &[Record]) -> Result<Decimal> {
    charges
        .iter()
        .try_fold(Decimal::zero(2), |acc, c| acc.checked_add(crate::common::dec(c, "amount", 2)?))
}

fn do_add_charge(input: AddCharge) -> Result<Record> {
    require_user()?;
    let landed = require("stock_landed_cost", &input.landed, "landed cost")?;
    if text(&landed, "state") != Some("draft") {
        return Err(Error::msg("charges can only be added while the landed cost is a draft"));
    }
    let amount = input.amount.with_scale(2)?;
    if amount.is_zero() || amount.is_negative() {
        return Err(Error::msg("a charge must be greater than zero"));
    }
    let line: Record = db::create(
        "stock_landed_charge",
        &json!({ "landed": input.landed, "description": input.description.trim(), "amount": amount }),
    )?;
    let total = total_of(&charges_of(&input.landed)?)?;
    let _: Option<Record> = db::update("stock_landed_cost", &input.landed, &json!({ "total": total }))?;
    Ok(line)
}

/// The open layer a receipt line's stock sits in. FIFO and standard layers are one per line; an average pool
/// is found by item, warehouse and lot.
fn layer_for(line: &Record, company: &str) -> Result<Option<Record>> {
    let line_id = id_of(line)?;
    if let Some(l) = db::find::<Record>("stock_layer").filter("receipt_line", line_id).first()? {
        return Ok(Some(l));
    }
    let item = text(line, "item").unwrap_or_default();
    let warehouse = text(line, "warehouse").unwrap_or_default();
    let lot = text(line, "lot").filter(|l| !l.is_empty());
    let rows: Vec<Record> = db::find("stock_layer")
        .filter("company", company)
        .filter("item", item)
        .filter("warehouse", warehouse)
        .limit(2000)
        .all()?;
    Ok(rows.into_iter().find(|r| {
        text(r, "valuation_method") == Some("average") && text(r, "lot").filter(|l| !l.is_empty()) == lot
    }))
}

fn do_post(input: Id) -> Result<Value> {
    require_user()?;
    let landed = require("stock_landed_cost", &input.id, "landed cost")?;
    if text(&landed, "state") != Some("draft") {
        return Err(Error::msg("only a draft landed cost can be posted"));
    }
    let charges = charges_of(&input.id)?;
    let total = total_of(&charges)?;
    if total.is_zero() {
        return Err(Error::msg("add at least one charge"));
    }
    let company = text(&landed, "company").unwrap_or_default().to_string();
    let move_id = text(&landed, "move").unwrap_or_default().to_string();
    let receipt = require("stock_move", &move_id, "receipt")?;
    let mut lines: Vec<Record> = db::find("stock_move_line").filter("move", move_id.as_str()).limit(200).all()?;
    lines.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    if lines.is_empty() {
        return Err(Error::msg("the receipt has no lines"));
    }
    let bases: Vec<Decimal> = lines
        .iter()
        .map(|l| match text(&landed, "basis") {
            Some("qty") => qty_of(l, "qty"),
            _ => value_of(l, "value"),
        })
        .collect::<Result<_>>()?;
    let shares = share_out(total, &bases, VALUE_SCALE)?;

    let mut capitalised_total = Decimal::zero(VALUE_SCALE);
    let mut expensed_total = Decimal::zero(VALUE_SCALE);
    let mut variance_total = Decimal::zero(VALUE_SCALE);
    let mut tx = db::transaction();
    for (line, share) in lines.iter().zip(shares) {
        let layer = layer_for(line, &company)?;
        let (capitalised, expensed, standard) = match &layer {
            Some(layer) => {
                let standard = text(layer, "valuation_method") == Some("standard");
                if standard {
                    // A standard-cost layer keeps its standard; the whole charge is a purchase price variance.
                    (Decimal::zero(VALUE_SCALE), Decimal::zero(VALUE_SCALE), true)
                } else {
                    let remaining = qty_of(layer, "remaining_qty")?;
                    let received = qty_of(line, "qty")?;
                    let (c, e) = split_on_hand(share, remaining, received, VALUE_SCALE)?;
                    (c, e, false)
                }
            }
            None => (Decimal::zero(VALUE_SCALE), share, false),
        };
        if standard {
            variance_total = variance_total.checked_add(share)?;
        } else {
            capitalised_total = capitalised_total.checked_add(capitalised)?;
            expensed_total = expensed_total.checked_add(expensed)?;
        }
        if let Some(layer) = &layer {
            if !standard && !capitalised.is_zero() {
                let remaining_qty = qty_of(layer, "remaining_qty")?;
                let new_value = value_of(layer, "remaining_value")?.checked_add(capitalised)?;
                let unit_cost = if remaining_qty.is_zero() { value_of(layer, "unit_cost")? } else { new_value.div(remaining_qty, VALUE_SCALE)? };
                tx = tx.update(
                    "stock_layer",
                    id_of(layer)?,
                    &json!({ "remaining_value": new_value, "unit_cost": unit_cost }),
                );
            }
        }
        let mut alloc = json!({
            "landed": input.id,
            "move_line": id_of(line)?,
            "amount": share,
            "capitalised": if standard { Decimal::zero(VALUE_SCALE) } else { capitalised },
            "expensed": if standard { share } else { expensed },
        });
        if let Some(layer) = &layer {
            alloc["layer"] = json!(id_of(layer)?);
        }
        tx = tx.create("stock_landed_alloc", &alloc);
    }
    let on = today()?;
    tx = tx.update("stock_landed_cost", &input.id, &json!({ "state": "posted", "posted_on": on }));
    tx.run().map_err(|e| e.or("could not post the landed cost"))?;

    let g = |d: Decimal| d.round_to(2);
    events::emit(
        "stock_revalued",
        &json!({
            "landed_cost": input.id,
            "move": move_id,
            "kind": "landed_cost",
            "number": text(&receipt, "number"),
            "company": company,
            "currency": text(&receipt, "currency"),
            "posting_date": on,
            "source_version": 1,
            "total_value": g(total),
            "inventory_in": g(capitalised_total),
            "cogs": g(expensed_total),
            "variance_debit": g(variance_total),
            "clearing": g(total),
        }),
    )?;
    Ok(json!({
        "landed_cost": require("stock_landed_cost", &input.id, "landed cost")?,
        "capitalised": capitalised_total,
        "expensed": expensed_total,
        "variance": variance_total,
    }))
}

fn do_get(input: Id) -> Result<Value> {
    crate::common::require_reader()?;
    let landed = require("stock_landed_cost", &input.id, "landed cost")?;
    let charges = charges_of(&input.id)?;
    let allocations: Vec<Record> = db::find("stock_landed_alloc").filter("landed", input.id.as_str()).limit(500).all()?;
    Ok(json!({ "landed_cost": landed, "charges": charges, "allocations": allocations }))
}

handler! {
    fn create_landed_cost(input: Create) -> Record { do_create(input) }
    fn add_landed_charge(input: AddCharge) -> Record { do_add_charge(input) }
    fn post_landed_cost(input: Id) -> Value { do_post(input) }
    fn get_landed_cost(input: Id) -> Value { do_get(input) }
}

