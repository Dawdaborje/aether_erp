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
    /// Put RFQs for the same need to several suppliers in one group to compare and award them together.
    #[serde(default)]
    request_group: Option<String>,
}

#[derive(Deserialize)]
struct SetRfqPrice {
    line: String,
    unit_price: Decimal,
}

#[derive(Deserialize)]
struct Group {
    request_group: String,
}

#[derive(Deserialize)]
struct AddRfqLine {
    #[serde(alias = "id")]
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
    if let Some(group) = input.request_group.as_ref().filter(|g| !g.trim().is_empty()) {
        data["request_group"] = json!(group.trim());
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
            "net_unit_price": unit_price,
            "amount": amount,
            "tax_amount": Decimal::zero(2),
            "qty_received": Decimal::zero(QTY_SCALE),
            "qty_billed": Decimal::zero(QTY_SCALE),
        });
        if let Some(wh) = text(line, "warehouse").or_else(|| text(&rfq, "warehouse")) {
            data["warehouse"] = json!(wh);
        }
        let _: Record = db::create("po_line", &data)?;
    }
    db::transaction()
        .update("po", &po_id, &json!({ "total": total, "subtotal": total, "tax_total": Decimal::zero(2) }))
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

/// Record the supplier's quoted price on a line of a sent RFQ.
fn do_set_rfq_price(input: SetRfqPrice) -> Result<Record> {
    require_user()?;
    let line = require("rfq_line", &input.line, "RFQ line")?;
    let rfq = require("rfq", text(&line, "rfq").unwrap_or_default(), "RFQ")?;
    if !matches!(text(&rfq, "state"), Some("draft" | "sent")) {
        return Err(Error::msg("prices can only be recorded on a draft or sent RFQ"));
    }
    let price = input.unit_price.with_scale(PRICE_SCALE)?;
    if price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    db::update("rfq_line", &input.line, &json!({ "unit_price": price }))?.ok_or_else(|| Error::msg("the line is gone"))
}

fn group_rfqs(group: &str) -> Result<Vec<Record>> {
    let rows: Vec<Record> = db::find("rfq").filter("request_group", group).limit(50).all()?;
    if rows.is_empty() {
        return Err(Error::msg("there are no RFQs in this group"));
    }
    Ok(rows)
}

/// Prices per item across the suppliers of one request group, with the cheapest marked.
fn do_compare_rfqs(input: Group) -> Result<Value> {
    require_reader()?;
    let rfqs = group_rfqs(&input.request_group)?;
    let mut items: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
    let mut totals: Vec<Value> = Vec::new();
    for rfq in &rfqs {
        if text(rfq, "state") == Some("cancelled") {
            continue;
        }
        let mut total = Decimal::zero(2);
        for line in rfq_lines(id_of(rfq)?)? {
            let amount = line_amount(qty_of(&line, "qty")?, price_of(&line, "unit_price")?)?;
            total = total.checked_add(amount)?;
            items.entry(text(&line, "item").unwrap_or_default().to_string()).or_default().push(json!({
                "rfq": id_of(rfq)?,
                "supplier": text(rfq, "supplier"),
                "qty": line.get("qty"),
                "unit_price": line.get("unit_price"),
                "amount": amount,
            }));
        }
        totals.push(json!({ "rfq": id_of(rfq)?, "supplier": text(rfq, "supplier"), "state": text(rfq, "state"), "total": total }));
    }
    let mut matrix = Vec::new();
    for (item, quotes) in items {
        let best = quotes
            .iter()
            .filter_map(|q| serde_json::from_value::<Decimal>(q["unit_price"].clone()).ok().map(|p| (p, q["rfq"].clone())))
            .min_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, rfq)| rfq);
        matrix.push(json!({ "item": item, "quotes": quotes, "cheapest_rfq": best }));
    }
    totals.sort_by(|a, b| {
        let ta = serde_json::from_value::<Decimal>(a["total"].clone()).ok();
        let tb = serde_json::from_value::<Decimal>(b["total"].clone()).ok();
        ta.cmp(&tb)
    });
    Ok(json!({ "request_group": input.request_group, "items": matrix, "totals": totals }))
}

/// Award one RFQ: it becomes the purchase order and the group's other open RFQs are cancelled.
fn do_award_rfq(input: Id) -> Result<Value> {
    require_user()?;
    let rfq = require("rfq", &input.id, "RFQ")?;
    let group = text(&rfq, "request_group").map(str::to_string);
    let awarded = do_convert_rfq_to_po(Id { id: input.id.clone() })?;
    let mut cancelled = Vec::new();
    if let Some(group) = group {
        for other in group_rfqs(&group)? {
            let other_id = id_of(&other)?.to_string();
            if other_id != input.id && matches!(text(&other, "state"), Some("draft" | "sent")) {
                let c: Option<Record> = db::update("rfq", &other_id, &json!({ "state": "cancelled" }))?;
                if let Some(c) = c {
                    cancelled.push(c);
                }
            }
        }
    }
    Ok(json!({ "awarded": awarded, "cancelled": cancelled }))
}

handler! {
    fn set_rfq_price(input: SetRfqPrice) -> Record { do_set_rfq_price(input) }
    fn compare_rfqs(input: Group) -> Value { do_compare_rfqs(input) }
    fn award_rfq(input: Id) -> Value { do_award_rfq(input) }
    fn create_rfq(input: CreateRfq) -> Record { do_create_rfq(input) }
    fn add_rfq_line(input: AddRfqLine) -> Record { do_add_rfq_line(input) }
    fn send_rfq(input: Id) -> Record { do_send_rfq(input) }
    fn convert_rfq_to_po(input: Id) -> Value { do_convert_rfq_to_po(input) }
    fn cancel_rfq(input: Id) -> Record { do_cancel_rfq(input) }
    fn get_rfq(input: Id) -> Value { do_get_rfq(input) }
}
