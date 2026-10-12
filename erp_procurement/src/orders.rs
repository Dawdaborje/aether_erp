//! Purchase orders, receipts, bills.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, company_currency_code, id_of, money_of, price_of, qty_of, require, require_reader, require_supplier,
    require_user, text, today, Record, MONEY_SCALE, PRICE_SCALE, QTY_SCALE,
};
use crate::match_status::{header_release, line_amount, line_release};

#[derive(Deserialize)]
struct CreatePo {
    company: String,
    supplier: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    order_date: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct AddPoLine {
    po: String,
    item: String,
    qty: Decimal,
    unit_price: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct ReceiveLine {
    po_line: String,
    qty: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct ReceivePo {
    po: String,
    #[serde(default)]
    receipt_date: Option<String>,
    lines: Vec<ReceiveLine>,
}

#[derive(Deserialize)]
struct BillLineIn {
    po_line: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    receipt_line: Option<String>,
}

#[derive(Deserialize)]
struct CreateBill {
    po: String,
    #[serde(default)]
    bill_date: Option<String>,
    lines: Vec<BillLineIn>,
}

pub(crate) fn po_lines(po_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("po_line").filter("po", po_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn refresh_po_state(po_id: &str) -> Result<Record> {
    let po = require("po", po_id, "purchase order")?;
    if matches!(text(&po, "state"), Some("cancelled" | "closed" | "draft")) {
        return Ok(po);
    }
    let lines = po_lines(po_id)?;
    if lines.is_empty() {
        return Ok(po);
    }
    let mut any_recv = false;
    let mut all_recv = true;
    for line in &lines {
        let qty = qty_of(line, "qty")?;
        let recv = qty_of(line, "qty_received")?;
        if !recv.is_zero() {
            any_recv = true;
        }
        if recv < qty {
            all_recv = false;
        }
    }
    let state = if all_recv && any_recv {
        "received"
    } else if any_recv {
        "partially_received"
    } else {
        "confirmed"
    };
    db::update("po", po_id, &json!({ "state": state }))?.ok_or_else(|| Error::msg("the order is gone"))
}

fn do_create_po(input: CreatePo) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let supplier = require_supplier(&input.supplier)?;
    let currency = match input.currency.filter(|c| !c.is_empty()) {
        Some(c) => c.to_uppercase(),
        None => company_currency_code(&company_id)?,
    };
    let order_date = match input.order_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let mut data = json!({
        "company": company_id,
        "supplier": id_of(&supplier)?,
        "currency": currency,
        "state": "draft",
        "order_date": order_date,
        "total": Decimal::zero(MONEY_SCALE),
    });
    if let Some(wh) = input.warehouse {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    if let Some(notes) = input.notes.filter(|s| !s.is_empty()) {
        data["notes"] = json!(notes);
    }
    db::create("po", &data).map_err(|e| e.or("could not create the purchase order"))
}

fn do_add_po_line(input: AddPoLine) -> Result<Record> {
    require_user()?;
    let po = require("po", &input.po, "purchase order")?;
    if text(&po, "state") != Some("draft") {
        return Err(Error::msg("lines can only be added while the order is a draft"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    let unit_price = input.unit_price.with_scale(PRICE_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    if unit_price.is_negative() {
        return Err(Error::msg("unit price is not negative"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let uom = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?;
    let amount = line_amount(qty, unit_price)?;
    let existing = po_lines(id_of(&po)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "po": id_of(&po)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": uom,
        "unit_price": unit_price,
        "amount": amount,
        "qty_received": Decimal::zero(QTY_SCALE),
        "qty_billed": Decimal::zero(QTY_SCALE),
    });
    if let Some(wh) = input.warehouse.or_else(|| text(&po, "warehouse").map(str::to_string)) {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    let line: Record = db::create("po_line", &data).map_err(|e| e.or("could not add the line"))?;
    let total = existing.iter().try_fold(amount, |acc, r| acc.checked_add(money_of(r, "amount")?))?;
    let _: Option<Record> = db::update("po", id_of(&po)?, &json!({ "total": total }))?;
    Ok(line)
}

fn do_confirm_po(input: Id) -> Result<Record> {
    require_user()?;
    let po = require("po", &input.id, "purchase order")?;
    if text(&po, "state") != Some("draft") {
        return Err(Error::msg("only a draft order can be confirmed"));
    }
    let lines = po_lines(&input.id)?;
    if lines.is_empty() {
        return Err(Error::msg("add at least one line before confirming"));
    }
    let company_id = text(&po, "company").unwrap_or_default();
    let order_date = text(&po, "order_date").unwrap_or_default();
    let (number, next, counter_id) = allocate_number(company_id, "PO", order_date)?;
    db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("po", &input.id, &json!({ "state": "confirmed", "number": number }))
        .run()?;
    require("po", &input.id, "purchase order")
}

fn next_match_seq(po_line: &str) -> Result<i64> {
    let rows: Vec<Record> = db::find("purchase_match").filter("po_line", po_line).limit(5000).all()?;
    Ok(rows.iter().map(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1)
}

fn do_receive_po(input: ReceivePo) -> Result<Value> {
    require_user()?;
    let po = require("po", &input.po, "purchase order")?;
    if !matches!(text(&po, "state"), Some("confirmed" | "partially_received")) {
        return Err(Error::msg("receive against a confirmed or partially received order"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to receive"));
    }
    let company_id = text(&po, "company").unwrap_or_default().to_string();
    let supplier_id = text(&po, "supplier").unwrap_or_default().to_string();
    let receipt_date = match input.receipt_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };

    let stock_move: Record = plugins::call(
        "erp_inventory",
        "draft_receipt",
        &json!({
            "company": company_id,
            "posting_date": receipt_date,
            "currency": text(&po, "currency"),
            "source_kind": "erp_procurement.po",
            "source_id": input.po,
            "source_version": 1,
        }),
    )?;
    let move_id = id_of(&stock_move)?.to_string();

    let (number, next, counter_id) = allocate_number(&company_id, "PR", &receipt_date)?;
    let receipt: Record = db::create(
        "purchase_receipt",
        &json!({
            "company": company_id,
            "po": input.po,
            "supplier": supplier_id,
            "stock_move": move_id,
            "state": "draft",
            "receipt_date": receipt_date,
            "number": number,
        }),
    )?;
    let receipt_id = id_of(&receipt)?.to_string();

    struct PlannedRecv {
        po_line_id: String,
        qty: Decimal,
        already: Decimal,
    }
    let mut planned = Vec::new();
    let mut line_no = 0i64;
    for spec in &input.lines {
        let po_line = require("po_line", &spec.po_line, "order line")?;
        if text(&po_line, "po") != Some(input.po.as_str()) {
            return Err(Error::msg("order line belongs to another purchase order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("receive quantity must be greater than zero"));
        }
        let ordered = qty_of(&po_line, "qty")?;
        let already = qty_of(&po_line, "qty_received")?;
        let open = ordered.checked_sub(already)?;
        if qty > open {
            return Err(Error::msg(format!("cannot receive {qty}: only {open} open on the line")));
        }
        let warehouse = spec
            .warehouse
            .clone()
            .or_else(|| text(&po_line, "warehouse").map(str::to_string))
            .or_else(|| text(&po, "warehouse").map(str::to_string))
            .ok_or_else(|| Error::msg("name a warehouse to receive into"))?;
        let unit_price = price_of(&po_line, "unit_price")?;
        let item_id = text(&po_line, "item").unwrap_or_default();

        let _: Record = plugins::call(
            "erp_inventory",
            "add_line",
            &json!({
                "move": move_id,
                "item": item_id,
                "qty": qty,
                "warehouse": warehouse,
                "unit_cost": unit_price,
            }),
        )?;

        line_no += 1;
        let rline: Record = db::create(
            "purchase_receipt_line",
            &json!({
                "receipt": receipt_id,
                "line_no": line_no,
                "po_line": id_of(&po_line)?,
                "item": item_id,
                "qty": qty,
                "warehouse": warehouse,
                "unit_price": unit_price,
            }),
        )?;
        let seq = next_match_seq(id_of(&po_line)?)?;
        let _: Record = db::create(
            "purchase_match",
            &json!({
                "company": company_id,
                "po_line": id_of(&po_line)?,
                "receipt_line": id_of(&rline)?,
                "qty": qty,
                "kind": "receive",
                "seq": seq,
            }),
        )?;
        planned.push(PlannedRecv {
            po_line_id: id_of(&po_line)?.to_string(),
            qty,
            already,
        });
    }

    // Stock first — qty_received stays unchanged if post fails.
    let posted: Record = plugins::call("erp_inventory", "post_move", &json!({ "id": move_id }))?;
    let mut tx = db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("purchase_receipt", &receipt_id, &json!({ "state": "posted" }));
    for row in &planned {
        let new_recv = row.already.checked_add(row.qty)?;
        tx = tx.update("po_line", &row.po_line_id, &json!({ "qty_received": new_recv }));
    }
    tx.run().map_err(|e| e.or("could not finish the receipt"))?;
    let po = refresh_po_state(&input.po)?;
    Ok(json!({ "receipt": require("purchase_receipt", &receipt_id, "receipt")?, "stock_move": posted, "po": po }))
}

fn match_qty(po_line_id: &str, kind: &str) -> Result<Decimal> {
    let rows: Vec<Record> = db::find("purchase_match")
        .filter("po_line", po_line_id)
        .filter("kind", kind)
        .limit(2000)
        .all()?;
    rows.iter().try_fold(Decimal::zero(QTY_SCALE), |acc, r| acc.checked_add(qty_of(r, "qty")?))
}

fn recompute_bill_release(bill_id: &str) -> Result<Record> {
    let _bill = require("purchase_bill", bill_id, "bill")?;
    let lines: Vec<Record> = db::find("purchase_bill_line").filter("bill", bill_id).limit(500).all()?;
    let mut statuses = Vec::new();
    for line in &lines {
        let po_line_id = text(line, "po_line").unwrap_or_default();
        let po_line = require("po_line", po_line_id, "order line")?;
        // Match rows are the source of truth; fall back to denormalised PO fields if empty.
        let received = {
            let from_match = match_qty(po_line_id, "receive")?;
            if from_match.is_zero() {
                qty_of(&po_line, "qty_received")?
            } else {
                from_match
            }
        };
        let status = line_release(
            qty_of(&po_line, "qty")?,
            received,
            qty_of(line, "qty")?,
            price_of(&po_line, "unit_price")?,
            price_of(line, "unit_price")?,
            false,
        )?;
        let _: Option<Record> = db::update("purchase_bill_line", id_of(line)?, &json!({ "can_be_paid": status.as_str() }))?;
        statuses.push(status);
    }
    let header = header_release(&statuses);
    db::update("purchase_bill", bill_id, &json!({ "release_to_pay": header.as_str() }))?
        .ok_or_else(|| Error::msg("the bill is gone"))
}

fn do_create_bill(input: CreateBill) -> Result<Record> {
    require_user()?;
    let po = require("po", &input.po, "purchase order")?;
    if matches!(text(&po, "state"), Some("draft" | "cancelled")) {
        return Err(Error::msg("bill a confirmed order"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to bill"));
    }
    let company_id = text(&po, "company").unwrap_or_default().to_string();
    let bill_date = match input.bill_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let (number, next, counter_id) = allocate_number(&company_id, "BILL", &bill_date)?;
    let bill: Record = db::create(
        "purchase_bill",
        &json!({
            "company": company_id,
            "supplier": text(&po, "supplier"),
            "po": input.po,
            "currency": text(&po, "currency"),
            "state": "draft",
            "bill_date": bill_date,
            "total": Decimal::zero(MONEY_SCALE),
            "release_to_pay": "exception",
            "source_version": 1,
            "number": number,
        }),
    )?;
    let bill_id = id_of(&bill)?.to_string();
    let mut total = Decimal::zero(MONEY_SCALE);
    let mut line_no = 0i64;
    for spec in &input.lines {
        let po_line = require("po_line", &spec.po_line, "order line")?;
        if text(&po_line, "po") != Some(input.po.as_str()) {
            return Err(Error::msg("order line belongs to another purchase order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("bill quantity must be greater than zero"));
        }
        let ordered = qty_of(&po_line, "qty")?;
        let received = qty_of(&po_line, "qty_received")?;
        let billed = qty_of(&po_line, "qty_billed")?;
        let open_bill = if received < ordered { received } else { ordered }.checked_sub(billed)?;
        if qty > open_bill {
            return Err(Error::msg(format!(
                "cannot bill {qty}: only {open_bill} open to bill on the line (received {received}, already billed {billed})"
            )));
        }
        let unit_price = spec.unit_price.unwrap_or(price_of(&po_line, "unit_price")?).with_scale(PRICE_SCALE)?;
        let amount = line_amount(qty, unit_price)?;
        total = total.checked_add(amount)?;
        line_no += 1;
        let mut data = json!({
            "bill": bill_id,
            "line_no": line_no,
            "po_line": id_of(&po_line)?,
            "item": text(&po_line, "item"),
            "qty": qty,
            "unit_price": unit_price,
            "amount": amount,
            "can_be_paid": "exception",
        });
        if let Some(rl) = &spec.receipt_line {
            let _ = require("purchase_receipt_line", rl, "receipt line")?;
            data["receipt_line"] = json!(rl);
        }
        let bline: Record = db::create("purchase_bill_line", &data)?;
        let seq = next_match_seq(id_of(&po_line)?)?;
        let _: Record = db::create(
            "purchase_match",
            &json!({
                "company": company_id,
                "po_line": id_of(&po_line)?,
                "bill_line": id_of(&bline)?,
                "receipt_line": spec.receipt_line,
                "qty": qty,
                "kind": "bill",
                "seq": seq,
            }),
        )?;
        let billed = qty_of(&po_line, "qty_billed")?.checked_add(qty)?;
        let _: Option<Record> = db::update("po_line", id_of(&po_line)?, &json!({ "qty_billed": billed }))?;
    }
    db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("purchase_bill", &bill_id, &json!({ "total": total }))
        .run()?;
    recompute_bill_release(&bill_id)
}

fn do_approve_bill(input: Id) -> Result<Record> {
    require_user()?;
    let bill = require("purchase_bill", &input.id, "bill")?;
    if text(&bill, "state") != Some("draft") {
        return Err(Error::msg("only a draft bill can be approved"));
    }
    let bill = recompute_bill_release(&input.id)?;
    if text(&bill, "release_to_pay") == Some("no") {
        return Err(Error::msg("nothing is received for this bill; receive first or force a price/qty exception review"));
    }
    // exception is allowed (partial / price variance) — finance still posts; release flag stays visible
    let approved = db::update("purchase_bill", &input.id, &json!({ "state": "approved" }))?
        .ok_or_else(|| Error::msg("the bill is gone"))?;
    let total = money_of(&approved, "total")?;
    // v0.1: full bill total clears GRNI and credits AP (no tax/variance split yet).
    events::emit(
        "purchase_bill_approved",
        &json!({
            "bill": input.id,
            "number": text(&approved, "number"),
            "company": text(&approved, "company"),
            "supplier": text(&approved, "supplier"),
            "currency": text(&approved, "currency"),
            "bill_date": text(&approved, "bill_date"),
            "total": approved.get("total"),
            "ap_total": total,
            "grni_clear": total,
            "variance": Decimal::zero(MONEY_SCALE),
            "release_to_pay": text(&approved, "release_to_pay"),
            "po": text(&approved, "po"),
            "source_version": approved.get("source_version").and_then(Value::as_i64).unwrap_or(1),
        }),
    )?;
    Ok(approved)
}

fn do_get_po(input: Id) -> Result<Value> {
    require_reader()?;
    let po = require("po", &input.id, "purchase order")?;
    let lines = po_lines(&input.id)?;
    Ok(json!({ "po": po, "lines": lines }))
}

fn do_cancel_po(input: Id) -> Result<Record> {
    require_user()?;
    let po = require("po", &input.id, "purchase order")?;
    if text(&po, "state") == Some("received") {
        return Err(Error::msg("a fully received order is closed, not cancelled"));
    }
    let lines = po_lines(&input.id)?;
    if lines.iter().any(|l| qty_of(l, "qty_received").map(|q| !q.is_zero()).unwrap_or(false)) {
        return Err(Error::msg("cancel only when nothing has been received"));
    }
    db::update("po", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the order is gone"))
}

handler! {
    fn create_po(input: CreatePo) -> Record { do_create_po(input) }
    fn add_po_line(input: AddPoLine) -> Record { do_add_po_line(input) }
    fn confirm_po(input: Id) -> Record { do_confirm_po(input) }
    fn cancel_po(input: Id) -> Record { do_cancel_po(input) }
    fn receive_po(input: ReceivePo) -> Value { do_receive_po(input) }
    fn create_bill(input: CreateBill) -> Record { do_create_bill(input) }
    fn approve_bill(input: Id) -> Record { do_approve_bill(input) }
    fn get_po(input: Id) -> Value { do_get_po(input) }
}
