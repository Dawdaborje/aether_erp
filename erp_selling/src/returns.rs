//! Customer returns (stock back in at the cost it left with) and credit notes.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, dec, id_of, price_line, price_of, qty_of, require, require_user, text, today, Record, MONEY_SCALE,
    PRICE_SCALE, QTY_SCALE,
};
use crate::orders::refresh_so_state;

/// Source kind `erp_inventory` recognises: a customer return credits COGS instead of clearing GRNI.
const RETURN_SOURCE: &str = "erp_selling.return";

#[derive(Deserialize)]
struct ReturnLine {
    so_line: String,
    qty: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
    /// The delivery line the goods came from; defaults to the most recent delivery of the order line.
    #[serde(default)]
    delivery_line: Option<String>,
}

#[derive(Deserialize)]
struct ReturnFromCustomer {
    so: String,
    #[serde(default)]
    return_date: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    lines: Vec<ReturnLine>,
}

#[derive(Deserialize)]
struct CreditLine {
    invoice_line: String,
    qty: Decimal,
}

#[derive(Deserialize)]
struct CreateCreditNote {
    invoice: String,
    #[serde(default)]
    invoice_date: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    lines: Vec<CreditLine>,
}

/// Unit cost the goods carried when they were delivered, from the delivery's posted stock move.
fn delivered_unit_cost(so_line_id: &str, item_id: &str, delivery_line: Option<&str>) -> Result<Decimal> {
    let line: Record = match delivery_line {
        Some(id) => require("sales_delivery_line", id, "delivery line")?,
        None => {
            let mut rows: Vec<Record> = db::find("sales_delivery_line").filter("so_line", so_line_id).limit(500).all()?;
            rows.sort_by(|a, b| text(a, "delivery").cmp(&text(b, "delivery")));
            rows.pop().ok_or_else(|| Error::msg("nothing was delivered on this order line"))?
        }
    };
    let delivery = require("sales_delivery", text(&line, "delivery").unwrap_or_default(), "delivery")?;
    let move_id = text(&delivery, "stock_move").ok_or_else(|| Error::msg("the delivery has no stock move"))?;
    let got: Value = plugins::call("erp_inventory", "get_move", &json!({ "id": move_id }))?;
    let lines = got.get("lines").and_then(Value::as_array).cloned().unwrap_or_default();
    for l in &lines {
        if text(l, "item") != Some(item_id) {
            continue;
        }
        let cost = dec(l, "unit_cost", PRICE_SCALE)?;
        if !cost.is_zero() {
            return Ok(cost);
        }
        let qty = dec(l, "qty", QTY_SCALE)?;
        let value = dec(l, "value", PRICE_SCALE)?;
        if !qty.is_zero() {
            let abs_qty = if qty.is_negative() { Decimal::zero(QTY_SCALE).checked_sub(qty)? } else { qty };
            let abs_value = if value.is_negative() { Decimal::zero(PRICE_SCALE).checked_sub(value)? } else { value };
            return abs_value.div(abs_qty, PRICE_SCALE);
        }
    }
    Err(Error::msg("could not find the delivered cost for this item"))
}

fn do_return_from_customer(input: ReturnFromCustomer) -> Result<Value> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if matches!(text(&so, "state"), Some("draft" | "cancelled")) {
        return Err(Error::msg("only a confirmed order has delivered goods to return"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what is returned"));
    }
    let company_id = text(&so, "company").unwrap_or_default().to_string();
    let return_date = match input.return_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    // Validate and price everything before the first write.
    struct Planned {
        so_line_id: String,
        item_id: String,
        qty: Decimal,
        unit_cost: Decimal,
        warehouse: String,
    }
    let mut planned: Vec<Planned> = Vec::new();
    for spec in &input.lines {
        let so_line = require("so_line", &spec.so_line, "order line")?;
        if text(&so_line, "so") != Some(input.so.as_str()) {
            return Err(Error::msg("order line belongs to another sales order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("return quantity must be greater than zero"));
        }
        let so_line_id = id_of(&so_line)?.to_string();
        let earlier = planned
            .iter()
            .filter(|p| p.so_line_id == so_line_id)
            .try_fold(Decimal::zero(QTY_SCALE), |acc, p| acc.checked_add(p.qty))?;
        let delivered = qty_of(&so_line, "qty_delivered")?;
        if qty.checked_add(earlier)? > delivered {
            return Err(Error::msg(format!("cannot take back {qty}: only {delivered} delivered on the line")));
        }
        let item_id = text(&so_line, "item").unwrap_or_default().to_string();
        let warehouse = spec
            .warehouse
            .clone()
            .or_else(|| text(&so_line, "warehouse").map(str::to_string))
            .or_else(|| text(&so, "warehouse").map(str::to_string))
            .ok_or_else(|| Error::msg("name a warehouse to return into"))?;
        let unit_cost = delivered_unit_cost(&so_line_id, &item_id, spec.delivery_line.as_deref())?;
        planned.push(Planned { so_line_id, item_id, qty, unit_cost, warehouse });
    }

    let stock_move: Record = plugins::call(
        "erp_inventory",
        "draft_receipt",
        &json!({
            "company": company_id,
            "posting_date": return_date,
            "currency": text(&so, "currency"),
            "source_kind": RETURN_SOURCE,
            "source_id": input.so,
            "source_version": 1,
            "reason": input.reason,
        }),
    )?;
    let move_id = id_of(&stock_move)?.to_string();
    let (number, next, counter_id) = allocate_number(&company_id, "SRET", &return_date)?;
    let mut doc = json!({
        "company": company_id,
        "so": input.so,
        "customer": text(&so, "customer"),
        "stock_move": move_id,
        "state": "draft",
        "return_date": return_date,
        "number": number,
    });
    if let Some(reason) = input.reason.as_ref().filter(|r| !r.trim().is_empty()) {
        doc["reason"] = json!(reason.trim());
    }
    let ret: Record = db::create("sales_return", &doc)?;
    let ret_id = id_of(&ret)?.to_string();
    for (idx, row) in planned.iter().enumerate() {
        let _: Record = plugins::call(
            "erp_inventory",
            "add_line",
            &json!({
                "move": move_id,
                "item": row.item_id,
                "qty": row.qty,
                "warehouse": row.warehouse,
                "unit_cost": row.unit_cost,
            }),
        )?;
        let _: Record = db::create(
            "sales_return_line",
            &json!({
                "return": ret_id,
                "line_no": idx as i64 + 1,
                "so_line": row.so_line_id,
                "item": row.item_id,
                "qty": row.qty,
                "unit_cost": row.unit_cost,
                "warehouse": row.warehouse,
            }),
        )?;
    }
    let posted: Record = plugins::call("erp_inventory", "post_move", &json!({ "id": move_id }))?;

    let mut totals: std::collections::HashMap<String, Decimal> = std::collections::HashMap::new();
    for row in &planned {
        let t = totals.entry(row.so_line_id.clone()).or_insert_with(|| Decimal::zero(QTY_SCALE));
        *t = t.checked_add(row.qty)?;
    }
    let mut tx = db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("sales_return", &ret_id, &json!({ "state": "posted" }));
    for (line_id, qty) in &totals {
        let line = require("so_line", line_id, "order line")?;
        tx = tx.update(
            "so_line",
            line_id,
            &json!({
                "qty_delivered": qty_of(&line, "qty_delivered")?.checked_sub(*qty)?,
                "qty_returned": qty_of(&line, "qty_returned")?.checked_add(*qty)?,
            }),
        );
    }
    tx.run().map_err(|e| e.or("could not finish the return"))?;
    let so = refresh_so_state(&input.so)?;
    Ok(json!({ "return": require("sales_return", &ret_id, "return")?, "stock_move": posted, "so": so }))
}

/// A credit note reverses part of an approved invoice at the invoice line's price, discount and tax.
fn do_create_credit_note(input: CreateCreditNote) -> Result<Record> {
    require_user()?;
    let invoice = require("sales_invoice", &input.invoice, "invoice")?;
    if text(&invoice, "kind") == Some("credit_note") {
        return Err(Error::msg("a credit note cannot be reversed again"));
    }
    if text(&invoice, "state") != Some("approved") {
        return Err(Error::msg("only an approved invoice can be reversed by a credit note"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to credit"));
    }
    let company_id = text(&invoice, "company").unwrap_or_default().to_string();
    let invoice_date = match input.invoice_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let (number, next, counter_id) = allocate_number(&company_id, "CN", &invoice_date)?;
    let mut header = json!({
        "company": company_id,
        "customer": text(&invoice, "customer"),
        "so": text(&invoice, "so"),
        "currency": text(&invoice, "currency"),
        "state": "draft",
        "kind": "credit_note",
        "reverses": id_of(&invoice)?,
        "invoice_date": invoice_date,
        "total": Decimal::zero(MONEY_SCALE),
        "subtotal": Decimal::zero(MONEY_SCALE),
        "tax_total": Decimal::zero(MONEY_SCALE),
        "source_version": 1,
        "number": number,
    });
    if let Some(reason) = input.reason.as_ref().filter(|r| !r.trim().is_empty()) {
        header["reason"] = json!(reason.trim());
    }
    if let Some(term) = text(&invoice, "payment_term") {
        header["payment_term"] = json!(term);
    }
    let note: Record = db::create("sales_invoice", &header)?;
    let note_id = id_of(&note)?.to_string();

    let mut subtotal = Decimal::zero(MONEY_SCALE);
    let mut tax_total = Decimal::zero(MONEY_SCALE);
    let mut credited_now: std::collections::HashMap<String, Decimal> = std::collections::HashMap::new();
    for (idx, spec) in input.lines.iter().enumerate() {
        let inv_line = require("sales_invoice_line", &spec.invoice_line, "invoice line")?;
        if text(&inv_line, "invoice") != Some(input.invoice.as_str()) {
            return Err(Error::msg("invoice line belongs to another invoice"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("credit quantity must be greater than zero"));
        }
        let line_id = id_of(&inv_line)?.to_string();
        let earlier: Vec<Record> = db::find("sales_invoice_line").filter("reverses_line", line_id.as_str()).limit(500).all()?;
        let already = earlier.iter().try_fold(Decimal::zero(QTY_SCALE), |acc, r| acc.checked_add(qty_of(r, "qty")?))?;
        let in_this = credited_now.get(&line_id).copied().unwrap_or_else(|| Decimal::zero(QTY_SCALE));
        let open = qty_of(&inv_line, "qty")?.checked_sub(already)?.checked_sub(in_this)?;
        if qty > open {
            return Err(Error::msg(format!("cannot credit {qty}: only {open} of the invoice line is not yet credited")));
        }
        let unit_price = price_of(&inv_line, "unit_price")?;
        let tax = text(&inv_line, "tax").map(str::to_string);
        let discount_pct = dec(&inv_line, "discount_pct", 4)?;
        let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
        subtotal = subtotal.checked_add(priced.net)?;
        tax_total = tax_total.checked_add(priced.tax)?;
        let mut data = json!({
            "invoice": note_id,
            "line_no": idx as i64 + 1,
            "so_line": text(&inv_line, "so_line"),
            "item": text(&inv_line, "item"),
            "qty": qty,
            "unit_price": unit_price,
            "discount_pct": discount_pct,
            "amount": priced.net,
            "tax_amount": priced.tax,
            "reverses_line": line_id,
        });
        if let Some(t) = &tax {
            data["tax"] = json!(t);
        }
        let _: Record = db::create("sales_invoice_line", &data)?;
        credited_now.insert(line_id, in_this.checked_add(qty)?);
        if let Some(so_line_id) = text(&inv_line, "so_line") {
            let so_line = require("so_line", so_line_id, "order line")?;
            let invoiced = qty_of(&so_line, "qty_invoiced")?.checked_sub(qty)?;
            let _: Option<Record> = db::update("so_line", so_line_id, &json!({ "qty_invoiced": invoiced }))?;
        }
    }
    db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update(
            "sales_invoice",
            &note_id,
            &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
        )
        .run()?;
    require("sales_invoice", &note_id, "credit note")
}

handler! {
    fn return_from_customer(input: ReturnFromCustomer) -> Value { do_return_from_customer(input) }
    fn create_credit_note(input: CreateCreditNote) -> Record { do_create_credit_note(input) }
}
