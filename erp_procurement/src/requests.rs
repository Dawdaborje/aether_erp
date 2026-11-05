//! Material requests: an internal "we need these items", approved before anyone buys.
//!
//! The kernel does the bookkeeping the model declares: the number comes from a naming series,
//! the lines are a child field written with the request, and the `workflow` in
//! `rules/material_request.json` decides who may move a request from one state to the next
//! (and records each move in the request's chatter). The handlers here only start and finish
//! that flow; ordering turns approved lines into purchase orders, one per supplier.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_user, text, today, Record, QTY_SCALE};
use crate::orders::{api_add_po_line, api_create_po};

#[derive(Deserialize)]
struct RequestLine {
    item: String,
    qty: Decimal,
    #[serde(default)]
    supplier: Option<String>,
    #[serde(default)]
    unit_price: Option<Decimal>,
}

#[derive(Deserialize)]
struct CreateRequest {
    company: String,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    needed_by: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    lines: Vec<RequestLine>,
    /// A one-line request from a form: the first line without a list.
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    qty: Option<Decimal>,
    #[serde(default)]
    unit_price: Option<Decimal>,
}

#[derive(Deserialize)]
struct Move {
    #[serde(alias = "id")]
    request: String,
}

#[derive(Deserialize)]
struct OrderInput {
    #[serde(alias = "id")]
    request: String,
    /// Supplier for lines that have none and whose item has no preferred supplier.
    #[serde(default)]
    supplier: Option<String>,
}

fn do_create(mut input: CreateRequest) -> Result<Record> {
    require_user()?;
    if let (Some(item), Some(qty)) = (input.item.take(), input.qty.take()) {
        input.lines.push(RequestLine { item, qty, supplier: None, unit_price: input.unit_price.take() });
    }
    if input.lines.is_empty() {
        return Err(Error::msg("add at least one line"));
    }
    let company = require("company.company", &input.company, "company")?;
    let mut lines = Vec::new();
    for line in &input.lines {
        let qty = line.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("quantity must be greater than zero"));
        }
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": line.item }))?;
        if item.get("active") == Some(&json!(false)) {
            return Err(Error::msg("an item on the request is not active"));
        }
        let mut row = json!({ "item": id_of(&item)?, "qty": qty });
        if let Some(supplier) = line.supplier.as_ref().filter(|s| !s.is_empty()) {
            row["supplier"] = json!(supplier);
        }
        if let Some(price) = &line.unit_price {
            if price.is_negative() {
                return Err(Error::msg("a price is not negative"));
            }
            row["unit_price"] = json!(price.clone().with_scale(crate::common::PRICE_SCALE)?);
        }
        lines.push(row);
    }
    let mut data = json!({
        "company": id_of(&company)?,
        "state": "draft",
        "request_date": today()?,
        "lines": lines,
    });
    if let Some(date) = input.needed_by.filter(|d| !d.is_empty()) {
        data["needed_by"] = json!(date);
    }
    if let Some(wh) = input.warehouse.filter(|w| !w.is_empty()) {
        data["warehouse"] = json!(wh);
    }
    if let Some(notes) = input.notes.filter(|n| !n.is_empty()) {
        data["notes"] = json!(notes);
    }
    db::create("material_request", &data).map_err(|e| e.or("could not create the request"))
}

/// A state change is a plain write: the kernel's workflow says whether this caller may make it.
fn move_to(request: &str, state: &str) -> Result<Record> {
    require_user()?;
    db::update("material_request", request, &json!({ "state": state }))?
        .ok_or_else(|| Error::msg("the request is gone"))
}

fn preferred_supplier(item: &str) -> Result<Option<String>> {
    let rows: Vec<Record> = plugins::call("erp_item", "list_item_suppliers", &json!({ "item": item }))?;
    Ok(rows
        .iter()
        .find(|r| r.get("is_preferred") == Some(&json!(true)))
        .and_then(|r| text(r, "supplier").map(str::to_string)))
}

fn do_order(input: OrderInput) -> Result<Value> {
    require_user()?;
    let request: Record = db::get_expanded("material_request", &input.request, &["lines"])?
        .ok_or_else(|| Error::msg("no such request"))?;
    if text(&request, "state") != Some("approved") {
        return Err(Error::msg("only an approved request can be ordered"));
    }
    let lines = request.get("lines").and_then(Value::as_array).cloned().unwrap_or_default();
    // Group the lines that are still to be ordered by supplier.
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for line in lines.iter().filter(|l| l.get("po").map_or(true, Value::is_null)) {
        let item = text(line, "item").unwrap_or_default();
        let supplier = match text(line, "supplier") {
            Some(s) => s.to_string(),
            None => match preferred_supplier(item)? {
                Some(s) => s,
                None => input
                    .supplier
                    .clone()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| Error::msg("an item has no preferred supplier; name a supplier to order from"))?,
            },
        };
        match groups.iter_mut().find(|(s, _)| *s == supplier) {
            Some((_, rows)) => rows.push(line.clone()),
            None => groups.push((supplier, vec![line.clone()])),
        }
    }
    if groups.is_empty() {
        return Err(Error::msg("every line is already on an order"));
    }
    let mut orders = Vec::new();
    for (supplier, rows) in groups {
        let mut head = json!({
            "company": text(&request, "company"),
            "supplier": supplier,
            "notes": format!("From request {}", text(&request, "number").unwrap_or_default()),
        });
        if let Some(wh) = text(&request, "warehouse") {
            head["warehouse"] = json!(wh);
        }
        let po = api_create_po(head)?;
        let po_id = id_of(&po)?.to_string();
        for row in &rows {
            let mut line = json!({ "po": po_id, "item": text(row, "item"), "qty": row.get("qty") });
            if let Some(price) = row.get("unit_price").filter(|p| !p.is_null()) {
                line["unit_price"] = price.clone();
            }
            api_add_po_line(line)?;
            let _: Option<Record> =
                db::update("material_request_line", id_of(row)?, &json!({ "po": po_id }))?;
        }
        orders.push(po);
    }
    let request: Record = db::update("material_request", &input.request, &json!({ "state": "ordered" }))?
        .ok_or_else(|| Error::msg("the request is gone"))?;
    Ok(json!({ "request": request, "orders": orders }))
}

#[derive(Deserialize, Default)]
struct ListFilter {
    #[serde(default)]
    state: Option<String>,
}

fn do_list(input: ListFilter) -> Result<Vec<Record>> {
    crate::common::require_reader()?;
    let mut q = db::find::<Record>("material_request").limit(500);
    if let Some(state) = input.state.filter(|s| !s.is_empty()) {
        q = q.filter("state", state.as_str());
    }
    let mut rows = q.all()?;
    rows.sort_by(|a, b| text(b, "number").cmp(&text(a, "number")));
    Ok(rows)
}

handler! {
    fn list_material_requests(input: ListFilter) -> Vec<Record> { do_list(input) }
    fn create_material_request(input: CreateRequest) -> Record { do_create(input) }
    fn submit_material_request(input: Move) -> Record { move_to(&input.request, "submitted") }
    fn approve_material_request(input: Move) -> Record { move_to(&input.request, "approved") }
    fn reject_material_request(input: Move) -> Record { move_to(&input.request, "rejected") }
    fn cancel_material_request(input: Move) -> Record { move_to(&input.request, "cancelled") }
    fn order_material_request(input: OrderInput) -> Value { do_order(input) }
}
