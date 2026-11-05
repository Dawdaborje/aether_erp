//! Purchase orders, receipts, bills.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, company_currency_code, id_of, money_of, price_line, price_of, qty_of, require, require_reader,
    require_supplier, require_user, sum_lines, text, today, Record, MONEY_SCALE, PRICE_SCALE, QTY_SCALE,
};
use crate::match_status::{header_release, line_release};

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
    #[serde(default)]
    payment_term: Option<String>,
    #[serde(default)]
    price_list: Option<String>,
}

#[derive(Deserialize)]
struct AddPoLine {
    #[serde(alias = "id")]
    po: String,
    item: String,
    qty: Decimal,
    /// Omit to take the price from the order's (or the default) buy price list.
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    tax: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    /// Unit the quantity and price are given in (a case); default the item's own unit.
    #[serde(default)]
    uom: Option<String>,
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

/// Per-company purchasing policy; a company with no row uses the defaults (no approval, no tolerance, match on received).
pub(crate) struct Policy {
    pub approval_limit: Decimal,
    pub over_receipt_pct: Decimal,
    pub match_on_ordered: bool,
    pub require_inspection: bool,
}

pub(crate) fn policy_for(company_id: &str) -> Result<Policy> {
    let row: Option<Record> = db::find("purchase_setting").filter("company", company_id).first()?;
    Ok(match row {
        Some(r) => Policy {
            approval_limit: crate::common::dec(&r, "po_approval_limit", MONEY_SCALE)?,
            over_receipt_pct: crate::common::dec(&r, "over_receipt_pct", 2)?,
            match_on_ordered: text(&r, "bill_match_on") == Some("ordered"),
            require_inspection: r.get("require_inspection") == Some(&json!(true)),
        },
        None => Policy {
            approval_limit: Decimal::zero(MONEY_SCALE),
            over_receipt_pct: Decimal::zero(2),
            match_on_ordered: false,
            require_inspection: false,
        },
    })
}

#[derive(Deserialize)]
struct SetPolicy {
    company: String,
    #[serde(default)]
    po_approval_limit: Option<Decimal>,
    #[serde(default)]
    over_receipt_pct: Option<Decimal>,
    #[serde(default)]
    bill_match_on: Option<String>,
    #[serde(default)]
    require_inspection: Option<bool>,
}

fn do_set_policy(input: SetPolicy) -> Result<Record> {
    crate::common::require_admin()?;
    let company = require("company.company", &input.company, "company")?;
    let mut data = json!({});
    if let Some(v) = input.po_approval_limit {
        let v = v.with_scale(MONEY_SCALE)?;
        if v.is_negative() {
            return Err(Error::msg("the approval limit must not be negative"));
        }
        data["po_approval_limit"] = json!(v);
    }
    if let Some(v) = input.over_receipt_pct {
        let v = v.with_scale(2)?;
        if v.is_negative() || v > Decimal::parse("100")?.with_scale(2)? {
            return Err(Error::msg("over-receipt tolerance is between 0 and 100 percent"));
        }
        data["over_receipt_pct"] = json!(v);
    }
    if let Some(on) = input.require_inspection {
        data["require_inspection"] = json!(on);
    }
    if let Some(m) = input.bill_match_on {
        if !matches!(m.as_str(), "received" | "ordered") {
            return Err(Error::msg("bills are matched on received or ordered quantity"));
        }
        data["bill_match_on"] = json!(m);
    }
    let existing: Option<Record> = db::find("purchase_setting").filter("company", id_of(&company)?).first()?;
    match existing {
        Some(row) => db::update("purchase_setting", id_of(&row)?, &data)?.ok_or_else(|| Error::msg("the setting is gone")),
        None => {
            data["company"] = json!(id_of(&company)?);
            db::create("purchase_setting", &data).map_err(|e| e.or("could not save the setting"))
        }
    }
}

fn do_get_policy(input: SetPolicy) -> Result<Value> {
    require_reader()?;
    let p = policy_for(&input.company)?;
    Ok(json!({
        "po_approval_limit": p.approval_limit,
        "over_receipt_pct": p.over_receipt_pct,
        "bill_match_on": if p.match_on_ordered { "ordered" } else { "received" },
        "require_inspection": p.require_inspection,
    }))
}

pub(crate) fn po_lines(po_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("po_line").filter("po", po_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

pub(crate) fn refresh_po_state(po_id: &str) -> Result<Record> {
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
    if let Some(term) = input.payment_term.filter(|t| !t.is_empty()) {
        let _ = require("erp_pricing.payment_term", &term, "payment term")?;
        data["payment_term"] = json!(term);
    }
    if let Some(list) = input.price_list.filter(|l| !l.is_empty()) {
        let _ = require("erp_pricing.price_list", &list, "price list")?;
        data["price_list"] = json!(list);
    }
    data["subtotal"] = json!(Decimal::zero(MONEY_SCALE));
    data["tax_total"] = json!(Decimal::zero(MONEY_SCALE));
    db::create("po", &data).map_err(|e| e.or("could not create the purchase order"))
}

fn do_add_po_line(input: AddPoLine) -> Result<Record> {
    require_user()?;
    let po = require("po", &input.po, "purchase order")?;
    if text(&po, "state") != Some("draft") {
        return Err(Error::msg("lines can only be added while the order is a draft"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("quantity must be greater than zero"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let item_uom = text(&item, "uom").ok_or_else(|| Error::msg("the item has no unit"))?.to_string();
    // A line may be entered in another unit (a case): `qty` and `unit_price` are kept per item unit.
    let order_uom = input.uom.clone().filter(|u| !u.is_empty() && *u != item_uom);
    let entered_qty = qty;
    let mut factor = Decimal::whole(1, 6)?;
    let mut qty = qty;
    if let Some(u) = &order_uom {
        let converted: Value = plugins::call(
            "erp_item",
            "convert_qty",
            &json!({ "item": id_of(&item)?, "qty": entered_qty, "from_uom": u, "context": "purchase" }),
        )?;
        qty = crate::common::dec(&converted, "base_qty", QTY_SCALE)?;
        if qty.is_zero() {
            return Err(Error::msg("the quantity is too small in the item's unit"));
        }
        factor = qty.div(entered_qty, 6)?;
    }
    let entered_price = input.unit_price.clone();
    let input_unit_price = match (&order_uom, &entered_price) {
        (Some(_), Some(p)) => Some(p.clone().div(factor, PRICE_SCALE)?),
        _ => entered_price.clone(),
    };
    let uom = item_uom.as_str();
    let (unit_price, list_discount) = match input_unit_price {
        Some(p) => (p.with_scale(PRICE_SCALE)?, Decimal::zero(4)),
        None => {
            let found: Value = plugins::call(
                "erp_pricing",
                "price_for",
                &json!({
                    "item": id_of(&item)?,
                    "kind": "buy",
                    "qty": qty,
                    "price_list": text(&po, "price_list"),
                    "party": text(&po, "supplier"),
                    "date": text(&po, "order_date"),
                    "currency": text(&po, "currency"),
                    "company": text(&po, "company"),
                }),
            )?;
            if found.get("found") != Some(&json!(true)) {
                return Err(Error::msg("no buy price found for this item; enter a unit price"));
            }
            (price_of(&found, "unit_price")?, crate::common::dec(&found, "discount_pct", 4)?)
        }
    };
    if unit_price.is_negative() {
        return Err(Error::msg("unit price is not negative"));
    }
    let discount_pct = input.discount_pct.unwrap_or(list_discount).with_scale(4)?;
    let tax = input.tax.filter(|t| !t.is_empty());
    let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
    let existing = po_lines(id_of(&po)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "po": id_of(&po)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": uom,
        "unit_price": unit_price,
        "discount_pct": discount_pct,
        "amount": priced.net,
        "tax_amount": priced.tax,
        "net_unit_price": priced.net_unit,
        "qty_received": Decimal::zero(QTY_SCALE),
        "qty_billed": Decimal::zero(QTY_SCALE),
    });
    if let Some(u) = &order_uom {
        data["order_uom"] = json!(u);
        data["order_qty"] = json!(entered_qty);
        data["order_unit_price"] = json!(match &entered_price {
            Some(p) => p.clone().with_scale(PRICE_SCALE)?,
            None => unit_price.times(factor, PRICE_SCALE)?,
        });
    }
    if let Some(t) = &tax {
        data["tax"] = json!(t);
    }
    if let Some(wh) = input.warehouse.or_else(|| text(&po, "warehouse").map(str::to_string)) {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    let line: Record = db::create("po_line", &data).map_err(|e| e.or("could not add the line"))?;
    let (subtotal, tax_total) = sum_lines(&po_lines(id_of(&po)?)?)?;
    let _: Option<Record> = db::update(
        "po",
        id_of(&po)?,
        &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
    )?;
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
    let policy = policy_for(company_id)?;
    let is_admin = context::current()?.has_role(crate::common::ADMIN_ROLE);
    if !policy.approval_limit.is_zero() && money_of(&po, "total")? > policy.approval_limit && !is_admin {
        // Over the limit: parked until an administrator approves (no number yet).
        return db::update("po", &input.id, &json!({ "state": "to_approve" }))?
            .ok_or_else(|| Error::msg("the order is gone"));
    }
    release_po(&po, &input.id)
}

fn release_po(po: &Record, id: &str) -> Result<Record> {
    let company_id = text(po, "company").unwrap_or_default();
    let order_date = text(po, "order_date").unwrap_or_default();
    let (number, next, counter_id) = allocate_number(company_id, "PO", order_date)?;
    db::transaction()
        .update("purchase_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("po", id, &json!({ "state": "confirmed", "number": number }))
        .run()?;
    require("po", id, "purchase order")
}

fn do_approve_po(input: Id) -> Result<Record> {
    crate::common::require_admin()?;
    let po = require("po", &input.id, "purchase order")?;
    if text(&po, "state") != Some("to_approve") {
        return Err(Error::msg("only an order waiting for approval can be approved"));
    }
    release_po(&po, &input.id)
}

fn do_reject_po(input: Id) -> Result<Record> {
    crate::common::require_admin()?;
    let po = require("po", &input.id, "purchase order")?;
    if text(&po, "state") != Some("to_approve") {
        return Err(Error::msg("only an order waiting for approval can be rejected"));
    }
    db::update("po", &input.id, &json!({ "state": "draft" }))?.ok_or_else(|| Error::msg("the order is gone"))
}

pub(crate) fn next_match_seq(po_line: &str) -> Result<i64> {
    let rows: Vec<Record> = db::find("purchase_match").filter("po_line", po_line).limit(5000).all()?;
    Ok(rows.iter().map(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1)
}

/// With the quality plugin installed, the order must have an accepted inspection. Without it
/// (the plugin is optional) there is nothing to ask and receiving goes ahead.
fn require_accepted_inspection(source_kind: &str, source_id: &str) -> Result<()> {
    if !plugins::available("erp_quality")? {
        return Ok(());
    }
    let status: Value = plugins::call(
        "erp_quality",
        "inspection_status",
        &json!({ "source_kind": source_kind, "source_id": source_id }),
    )?;
    match status.get("state").and_then(Value::as_str) {
        Some("accepted") => Ok(()),
        Some("rejected") => Err(Error::msg("the inspection of this order was rejected")),
        Some("pending") => Err(Error::msg("finish the inspection of this order first")),
        _ => Err(Error::msg("this order needs an accepted inspection before goods are received")),
    }
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
    let policy = policy_for(&company_id)?;
    if policy.require_inspection {
        require_accepted_inspection("erp_procurement.po", &input.po)?;
    }
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
        let tolerance = ordered
            .times(policy.over_receipt_pct, QTY_SCALE + 2)?
            .div(Decimal::parse("100")?, QTY_SCALE)?;
        let open = ordered.checked_add(tolerance)?.checked_sub(already)?;
        if qty > open {
            return Err(Error::msg(format!("cannot receive {qty}: only {open} open on the line")));
        }
        let warehouse = spec
            .warehouse
            .clone()
            .or_else(|| text(&po_line, "warehouse").map(str::to_string))
            .or_else(|| text(&po, "warehouse").map(str::to_string))
            .ok_or_else(|| Error::msg("name a warehouse to receive into"))?;
        // Stock is valued at the tax-exclusive, post-discount price (older lines fall back to the list price).
        let unit_price = match price_of(&po_line, "net_unit_price")? {
            p if p.is_zero() => price_of(&po_line, "unit_price")?,
            p => p,
        };
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
    let bill = require("purchase_bill", bill_id, "bill")?;
    let on_ordered = policy_for(text(&bill, "company").unwrap_or_default())?.match_on_ordered;
    let lines: Vec<Record> = db::find("purchase_bill_line").filter("bill", bill_id).limit(500).all()?;
    let mut statuses = Vec::new();
    for line in &lines {
        let po_line_id = text(line, "po_line").unwrap_or_default();
        let po_line = require("po_line", po_line_id, "order line")?;
        // Match rows are the source of truth; fall back to denormalised PO fields if empty.
        let received = {
            let from_match = match_qty(po_line_id, "receive")?.checked_sub(match_qty(po_line_id, "return")?)?;
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
            on_ordered,
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
            "subtotal": Decimal::zero(MONEY_SCALE),
            "tax_total": Decimal::zero(MONEY_SCALE),
        }),
    )?;
    if let Some(term) = text(&po, "payment_term") {
        let due: Value = plugins::call("erp_pricing", "due_date_for", &json!({ "term": term, "date": bill_date }))?;
        let _: Option<Record> =
            db::update("purchase_bill", id_of(&bill)?, &json!({ "payment_term": term, "due_date": due["due_date"] }))?;
    }
    let bill_id = id_of(&bill)?.to_string();
    let mut subtotal = Decimal::zero(MONEY_SCALE);
    let mut tax_total = Decimal::zero(MONEY_SCALE);
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
        let discount_pct = crate::common::dec(&po_line, "discount_pct", 4)?;
        let tax = text(&po_line, "tax").map(str::to_string);
        let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
        let amount = priced.net;
        subtotal = subtotal.checked_add(amount)?;
        tax_total = tax_total.checked_add(priced.tax)?;
        line_no += 1;
        let mut data = json!({
            "bill": bill_id,
            "line_no": line_no,
            "po_line": id_of(&po_line)?,
            "item": text(&po_line, "item"),
            "qty": qty,
            "unit_price": unit_price,
            "discount_pct": discount_pct,
            "amount": amount,
            "tax_amount": priced.tax,
            "can_be_paid": "exception",
        });
        if let Some(t) = &tax {
            data["tax"] = json!(t);
        }
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
        .update(
            "purchase_bill",
            &bill_id,
            &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
        )
        .run()?;
    recompute_bill_release(&bill_id)
}

fn do_approve_bill(input: Id) -> Result<Record> {
    require_user()?;
    let bill = require("purchase_bill", &input.id, "bill")?;
    if text(&bill, "state") != Some("draft") {
        return Err(Error::msg("only a draft bill can be approved"));
    }
    if text(&bill, "kind") == Some("debit_note") {
        return approve_debit_note(&input.id);
    }
    let bill = recompute_bill_release(&input.id)?;
    if text(&bill, "release_to_pay") == Some("no") {
        return Err(Error::msg("nothing is received for this bill; receive first or force a price/qty exception review"));
    }
    // exception is allowed (partial / price variance) — finance still posts; release flag stays visible
    let approved = db::update("purchase_bill", &input.id, &json!({ "state": "approved" }))?
        .ok_or_else(|| Error::msg("the bill is gone"))?;
    let total = money_of(&approved, "total")?;
    // Goods (subtotal) clear GRNI, input tax is its own line, and the full total is the payable.
    let subtotal = money_of(&approved, "subtotal")?;
    let tax_total = money_of(&approved, "tax_total")?;
    // Bills written before taxes existed carry only a total: treat it all as goods.
    let (grni_clear, tax_total) = if subtotal.is_zero() && tax_total.is_zero() { (total, tax_total) } else { (subtotal, tax_total) };
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
            "grni_clear": grni_clear,
            "tax_total": tax_total,
            "due_date": text(&approved, "due_date"),
            "variance": Decimal::zero(MONEY_SCALE),
            "release_to_pay": text(&approved, "release_to_pay"),
            "po": text(&approved, "po"),
            "source_version": approved.get("source_version").and_then(Value::as_i64).unwrap_or(1),
        }),
    )?;
    Ok(approved)
}

/// A debit note reduces what is owed: it mirrors the bill's ledger lines (debit AP, credit GRNI and tax).
fn approve_debit_note(id: &str) -> Result<Record> {
    let approved = db::update("purchase_bill", id, &json!({ "state": "approved" }))?
        .ok_or_else(|| Error::msg("the debit note is gone"))?;
    let total = money_of(&approved, "total")?;
    events::emit(
        "purchase_debit_note_approved",
        &json!({
            "bill": id,
            "number": text(&approved, "number"),
            "company": text(&approved, "company"),
            "supplier": text(&approved, "supplier"),
            "currency": text(&approved, "currency"),
            "bill_date": text(&approved, "bill_date"),
            "total": approved.get("total"),
            "ap_total": total,
            "grni_credit": money_of(&approved, "subtotal")?,
            "tax_total": money_of(&approved, "tax_total")?,
            "reverses": text(&approved, "reverses"),
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

/// In-crate entry points for the parts of the plugin that build orders (material requests).
pub(crate) fn api_create_po(input: Value) -> Result<Record> {
    do_create_po(serde_json::from_value(input).map_err(|e| Error::msg(format!("order: {e}")))?)
}

pub(crate) fn api_add_po_line(input: Value) -> Result<Record> {
    do_add_po_line(serde_json::from_value(input).map_err(|e| Error::msg(format!("line: {e}")))?)
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
    fn approve_po(input: Id) -> Record { do_approve_po(input) }
    fn reject_po(input: Id) -> Record { do_reject_po(input) }
    fn set_purchase_setting(input: SetPolicy) -> Record { do_set_policy(input) }
    fn get_purchase_setting(input: SetPolicy) -> Value { do_get_policy(input) }
}
