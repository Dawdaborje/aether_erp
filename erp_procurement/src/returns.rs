//! Supplier returns (stock back out) and debit notes (bill reversal).

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, dec, id_of, price_line, price_of, qty_of, require, require_user, text, today, Record,
    MONEY_SCALE, PRICE_SCALE, QTY_SCALE,
};
use crate::orders::{next_match_seq, refresh_po_state};

/// Source kind `erp_inventory` recognises: a supplier return clears GRNI instead of posting COGS.
const RETURN_SOURCE: &str = "erp_procurement.return";

#[derive(Deserialize)]
struct ReturnLine {
    po_line: String,
    qty: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct ReturnToSupplier {
    po: String,
    #[serde(default)]
    return_date: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    lines: Vec<ReturnLine>,
}

#[derive(Deserialize)]
struct DebitLine {
    bill_line: String,
    qty: Decimal,
}

#[derive(Deserialize)]
struct CreateDebitNote {
    bill: String,
    #[serde(default)]
    bill_date: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    lines: Vec<DebitLine>,
}

fn do_return_to_supplier(input: ReturnToSupplier) -> Result<Value> {
    require_user()?;
    let po = require("po", &input.po, "purchase order")?;
    if matches!(text(&po, "state"), Some("draft" | "cancelled")) {
        return Err(Error::msg("only a confirmed order has received goods to return"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to return"));
    }
    let company_id = text(&po, "company").unwrap_or_default().to_string();
    let return_date = match input.return_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let stock_move: Record = plugins::call(
        "erp_inventory",
        "draft_issue",
        &json!({
            "company": company_id,
            "posting_date": return_date,
            "currency": text(&po, "currency"),
            "source_kind": RETURN_SOURCE,
            "source_id": input.po,
            "source_version": 1,
            "reason": input.reason,
        }),
    )?;
    let move_id = id_of(&stock_move)?.to_string();
    let (number, next, counter_id) = allocate_number(&company_id, "RET", &return_date)?;
    let mut doc = json!({
        "company": company_id,
        "po": input.po,
        "supplier": text(&po, "supplier"),
        "stock_move": move_id,
        "state": "draft",
        "return_date": return_date,
        "number": number,
    });
    if let Some(reason) = input.reason.as_ref().filter(|r| !r.trim().is_empty()) {
        doc["reason"] = json!(reason.trim());
    }
    let ret: Record = db::create("purchase_return", &doc)?;
    let ret_id = id_of(&ret)?.to_string();

    struct Planned {
        po_line_id: String,
        received: Decimal,
        returned: Decimal,
        qty: Decimal,
    }
    let mut planned: Vec<Planned> = Vec::new();
    for (idx, spec) in input.lines.iter().enumerate() {
        let po_line = require("po_line", &spec.po_line, "order line")?;
        if text(&po_line, "po") != Some(input.po.as_str()) {
            return Err(Error::msg("order line belongs to another purchase order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("return quantity must be greater than zero"));
        }
        let received = qty_of(&po_line, "qty_received")?;
        let this_line_planned = planned
            .iter()
            .filter(|p| p.po_line_id == id_of(&po_line).unwrap_or_default())
            .try_fold(Decimal::zero(QTY_SCALE), |acc, p| acc.checked_add(p.qty))?;
        if qty.checked_add(this_line_planned)? > received {
            return Err(Error::msg(format!("cannot return {qty}: only {received} received on the line")));
        }
        let warehouse = spec
            .warehouse
            .clone()
            .or_else(|| text(&po_line, "warehouse").map(str::to_string))
            .or_else(|| text(&po, "warehouse").map(str::to_string))
            .ok_or_else(|| Error::msg("name a warehouse to return from"))?;
        let item_id = text(&po_line, "item").unwrap_or_default();
        let _: Record = plugins::call(
            "erp_inventory",
            "add_line",
            &json!({ "move": move_id, "item": item_id, "qty": qty, "warehouse": warehouse }),
        )?;
        let _: Record = db::create(
            "purchase_return_line",
            &json!({
                "return": ret_id,
                "line_no": idx as i64 + 1,
                "po_line": id_of(&po_line)?,
                "item": item_id,
                "qty": qty,
                "warehouse": warehouse,
            }),
        )?;
        planned.push(Planned {
            po_line_id: id_of(&po_line)?.to_string(),
            received,
            returned: qty_of(&po_line, "qty_returned")?,
            qty,
        });
    }

    // Stock first: nothing changes on the order if the post is refused (e.g. not enough on hand).
    let posted: Record = plugins::call("erp_inventory", "post_move", &json!({ "id": move_id }))?;
    let mut tx = db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("purchase_return", &ret_id, &json!({ "state": "posted" }));
    let mut running: std::collections::HashMap<String, (Decimal, Decimal)> = std::collections::HashMap::new();
    for row in &planned {
        let entry = running.entry(row.po_line_id.clone()).or_insert((row.received, row.returned));
        entry.0 = entry.0.checked_sub(row.qty)?;
        entry.1 = entry.1.checked_add(row.qty)?;
    }
    for (line_id, (received, returned)) in &running {
        tx = tx.update("po_line", line_id, &json!({ "qty_received": received, "qty_returned": returned }));
    }
    tx.run().map_err(|e| e.or("could not finish the return"))?;
    for row in &planned {
        let seq = next_match_seq(&row.po_line_id)?;
        let _: Record = db::create(
            "purchase_match",
            &json!({ "company": company_id, "po_line": row.po_line_id, "qty": row.qty, "kind": "return", "seq": seq }),
        )?;
    }
    let po = refresh_po_state(&input.po)?;
    Ok(json!({ "return": require("purchase_return", &ret_id, "return")?, "stock_move": posted, "po": po }))
}

/// A debit note reverses part of an approved bill: lines repeat the bill line's price, discount and tax.
fn do_create_debit_note(input: CreateDebitNote) -> Result<Record> {
    require_user()?;
    let bill = require("purchase_bill", &input.bill, "bill")?;
    if text(&bill, "kind") == Some("debit_note") {
        return Err(Error::msg("a debit note cannot be reversed again"));
    }
    if text(&bill, "state") != Some("approved") {
        return Err(Error::msg("only an approved bill can be reversed by a debit note"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to credit"));
    }
    let company_id = text(&bill, "company").unwrap_or_default().to_string();
    let bill_date = match input.bill_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let (number, next, counter_id) = allocate_number(&company_id, "DN", &bill_date)?;
    let mut header = json!({
        "company": company_id,
        "supplier": text(&bill, "supplier"),
        "po": text(&bill, "po"),
        "currency": text(&bill, "currency"),
        "state": "draft",
        "kind": "debit_note",
        "reverses": id_of(&bill)?,
        "bill_date": bill_date,
        "total": Decimal::zero(MONEY_SCALE),
        "subtotal": Decimal::zero(MONEY_SCALE),
        "tax_total": Decimal::zero(MONEY_SCALE),
        "release_to_pay": "yes",
        "source_version": 1,
        "number": number,
    });
    if let Some(reason) = input.reason.as_ref().filter(|r| !r.trim().is_empty()) {
        header["reason"] = json!(reason.trim());
    }
    let note: Record = db::create("purchase_bill", &header)?;
    let note_id = id_of(&note)?.to_string();

    let mut subtotal = Decimal::zero(MONEY_SCALE);
    let mut tax_total = Decimal::zero(MONEY_SCALE);
    let mut credited_now: std::collections::HashMap<String, Decimal> = std::collections::HashMap::new();
    for (idx, spec) in input.lines.iter().enumerate() {
        let bill_line = require("purchase_bill_line", &spec.bill_line, "bill line")?;
        if text(&bill_line, "bill") != Some(input.bill.as_str()) {
            return Err(Error::msg("bill line belongs to another bill"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("credit quantity must be greater than zero"));
        }
        let line_id = id_of(&bill_line)?.to_string();
        let earlier: Vec<Record> = db::find("purchase_bill_line").filter("reverses_line", line_id.as_str()).limit(500).all()?;
        let already = earlier.iter().try_fold(Decimal::zero(QTY_SCALE), |acc, r| acc.checked_add(qty_of(r, "qty")?))?;
        let in_this = credited_now.get(&line_id).copied().unwrap_or_else(|| Decimal::zero(QTY_SCALE));
        let open = qty_of(&bill_line, "qty")?.checked_sub(already)?.checked_sub(in_this)?;
        if qty > open {
            return Err(Error::msg(format!("cannot credit {qty}: only {open} of the bill line is not yet credited")));
        }
        let unit_price = price_of(&bill_line, "unit_price")?;
        let tax = text(&bill_line, "tax").map(str::to_string);
        let discount_pct = dec(&bill_line, "discount_pct", 4)?;
        let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
        subtotal = subtotal.checked_add(priced.net)?;
        tax_total = tax_total.checked_add(priced.tax)?;
        let mut data = json!({
            "bill": note_id,
            "line_no": idx as i64 + 1,
            "po_line": text(&bill_line, "po_line"),
            "item": text(&bill_line, "item"),
            "qty": qty,
            "unit_price": unit_price.with_scale(PRICE_SCALE)?,
            "discount_pct": discount_pct,
            "amount": priced.net,
            "tax_amount": priced.tax,
            "can_be_paid": "yes",
            "reverses_line": line_id,
        });
        if let Some(t) = &tax {
            data["tax"] = json!(t);
        }
        let _: Record = db::create("purchase_bill_line", &data)?;
        credited_now.insert(line_id, in_this.checked_add(qty)?);
        // The order line is billable again for what was credited.
        if let Some(po_line_id) = text(&bill_line, "po_line") {
            let po_line = require("po_line", po_line_id, "order line")?;
            let billed = qty_of(&po_line, "qty_billed")?.checked_sub(qty)?;
            let _: Option<Record> = db::update("po_line", po_line_id, &json!({ "qty_billed": billed }))?;
        }
    }
    db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update(
            "purchase_bill",
            &note_id,
            &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
        )
        .run()?;
    require("purchase_bill", &note_id, "debit note")
}

handler! {
    fn return_to_supplier(input: ReturnToSupplier) -> Value { do_return_to_supplier(input) }
    fn create_debit_note(input: CreateDebitNote) -> Record { do_create_debit_note(input) }
}
