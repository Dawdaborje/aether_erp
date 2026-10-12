//! Request for quotation → purchase order.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, company_currency_code, id_of, price_of, qty_of, require, require_reader, require_supplier,
    require_user, text, today, Record, PRICE_SCALE, QTY_SCALE,
};
use crate::match_status::line_amount;

#[derive(Deserialize)]
struct CreateRfq {
    company: String,
    supplier: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    rfq_date: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct AddRfqLine {
    rfq: String,
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

fn rfq_lines(rfq_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("rfq_line").filter("rfq", rfq_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn do_create_rfq(input: CreateRfq) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let supplier = require_supplier(&input.supplier)?;
    let currency = match input.currency.filter(|c| !c.is_empty()) {
        Some(c) => c.to_uppercase(),
        None => company_currency_code(&company_id)?,
    };
    let rfq_date = match input.rfq_date {
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
        "rfq_date": rfq_date,
    });
    if let Some(wh) = input.warehouse {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    if let Some(notes) = input.notes.filter(|s| !s.is_empty()) {
        data["notes"] = json!(notes);
    }
    db::create("rfq", &data).map_err(|e| e.or("could not create the RFQ"))
}

fn do_add_rfq_line(input: AddRfqLine) -> Result<Record> {
    require_user()?;
    let rfq = require("rfq", &input.rfq, "RFQ")?;
    if text(&rfq, "state") != Some("draft") {
        return Err(Error::msg("lines can only be added while the RFQ is a draft"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    let unit_price = input.unit_price.with_scale(PRICE_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    if unit_price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let uom = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?;
    let existing = rfq_lines(id_of(&rfq)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "rfq": id_of(&rfq)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": uom,
        "unit_price": unit_price,
    });
    if let Some(wh) = input.warehouse.or_else(|| text(&rfq, "warehouse").map(str::to_string)) {
        data["warehouse"] = json!(wh);
    }
    db::create("rfq_line", &data).map_err(|e| e.or("could not add the RFQ line"))
}

fn do_send_rfq(input: Id) -> Result<Record> {
    require_user()?;
    let rfq = require("rfq", &input.id, "RFQ")?;
    if text(&rfq, "state") != Some("draft") {
        return Err(Error::msg("only a draft RFQ can be sent"));
    }
    if rfq_lines(&input.id)?.is_empty() {
        return Err(Error::msg("add at least one line before sending"));
    }
    let company_id = text(&rfq, "company").unwrap_or_default();
    let date = text(&rfq, "rfq_date").unwrap_or_default();
    let (number, next, counter_id) = allocate_number(company_id, "RFQ", date)?;
    db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("rfq", &input.id, &json!({ "state": "sent", "number": number }))
        .run()?;
    require("rfq", &input.id, "RFQ")
}

fn do_convert_rfq_to_po(input: Id) -> Result<Value> {
    require_user()?;
    let rfq = require("rfq", &input.id, "RFQ")?;
    if !matches!(text(&rfq, "state"), Some("sent" | "draft")) {
        return Err(Error::msg("convert a draft or sent RFQ"));
    }
    if text(&rfq, "po").is_some() {
        return Err(Error::msg("this RFQ was already converted"));
    }
    let lines = rfq_lines(&input.id)?;
    if lines.is_empty() {
        return Err(Error::msg("the RFQ has no lines"));
    }
    let company_id = text(&rfq, "company").unwrap_or_default().to_string();
    let order_date = today()?;
    let mut po_data = json!({
        "company": company_id,
        "supplier": text(&rfq, "supplier"),
        "currency": text(&rfq, "currency"),
        "state": "draft",
        "order_date": order_date,
        "total": Decimal::zero(2),
        "rfq": input.id,
    });
    if let Some(wh) = text(&rfq, "warehouse") {
        po_data["warehouse"] = json!(wh);
    }
    if let Some(notes) = text(&rfq, "notes") {
        po_data["notes"] = json!(notes);
    }
    let po: Record = db::create("po", &po_data).map_err(|e| e.or("could not create the purchase order"))?;
    let po_id = id_of(&po)?.to_string();
    let mut total = Decimal::zero(2);
    for line in &lines {
        let qty = qty_of(line, "qty")?;
        let unit_price = price_of(line, "unit_price")?;
        let amount = line_amount(qty, unit_price)?;
        total = total.checked_add(amount)?;
        let mut data = json!({
            "po": po_id,
            "line_no": line.get("line_no"),
            "item": text(line, "item"),
            "qty": qty,
            "uom": text(line, "uom"),
            "unit_price": unit_price,
            "amount": amount,
            "qty_received": Decimal::zero(QTY_SCALE),
            "qty_billed": Decimal::zero(QTY_SCALE),
        });
        if let Some(wh) = text(line, "warehouse").or_else(|| text(&rfq, "warehouse")) {
            data["warehouse"] = json!(wh);
        }
        let _: Record = db::create("po_line", &data)?;
    }
    db::transaction()
        .update("po", &po_id, &json!({ "total": total }))
        .update("rfq", &input.id, &json!({ "state": "awarded", "po": po_id }))
        .run()?;
    Ok(json!({
        "rfq": require("rfq", &input.id, "RFQ")?,
        "po": require("po", &po_id, "purchase order")?,
        "lines": crate::orders::po_lines(&po_id)?,
    }))
}

fn do_cancel_rfq(input: Id) -> Result<Record> {
    require_user()?;
    let rfq = require("rfq", &input.id, "RFQ")?;
    if text(&rfq, "state") == Some("awarded") {
        return Err(Error::msg("an awarded RFQ cannot be cancelled; cancel the purchase order instead"));
    }
    if text(&rfq, "state") == Some("cancelled") {
        return Ok(rfq);
    }
    db::update("rfq", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the RFQ is gone"))
}

fn do_get_rfq(input: Id) -> Result<Value> {
    require_reader()?;
    let rfq = require("rfq", &input.id, "RFQ")?;
    let lines = rfq_lines(&input.id)?;
    Ok(json!({ "rfq": rfq, "lines": lines }))
}

handler! {
    fn create_rfq(input: CreateRfq) -> Record { do_create_rfq(input) }
    fn add_rfq_line(input: AddRfqLine) -> Record { do_add_rfq_line(input) }
    fn send_rfq(input: Id) -> Record { do_send_rfq(input) }
    fn convert_rfq_to_po(input: Id) -> Value { do_convert_rfq_to_po(input) }
    fn cancel_rfq(input: Id) -> Record { do_cancel_rfq(input) }
    fn get_rfq(input: Id) -> Value { do_get_rfq(input) }
}
