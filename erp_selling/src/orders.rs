//! Sales orders, deliveries, invoices.

use aether_sdk::dates::parse_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    allocate_number, company_currency_code, dec, id_of, money_of, price_line, price_of, qty_of, require, require_customer,
    require_reader, require_user, sum_lines, text, today, Record, MONEY_SCALE, PRICE_SCALE, QTY_SCALE,
};

#[derive(Deserialize)]
struct CreateSo {
    company: String,
    customer: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    order_date: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    deal: Option<String>,
    #[serde(default)]
    payment_term: Option<String>,
    #[serde(default)]
    price_list: Option<String>,
}

#[derive(Deserialize)]
struct CreateSoFromDeal {
    deal: String,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct AddSoLine {
    #[serde(alias = "id")]
    so: String,
    item: String,
    qty: Decimal,
    /// Omit to take the price from the order's (or the default) sell price list.
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
struct DeliverLine {
    so_line: String,
    qty: Decimal,
    #[serde(default)]
    warehouse: Option<String>,
    /// Lot or serial to deliver from; omit for a tracked item to take the held lots, then earliest expiry first.
    #[serde(default)]
    lot: Option<String>,
}

#[derive(Deserialize)]
struct DeliverSo {
    so: String,
    #[serde(default)]
    delivery_date: Option<String>,
    lines: Vec<DeliverLine>,
}

#[derive(Deserialize)]
struct InvoiceLineIn {
    so_line: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    delivery_line: Option<String>,
}

#[derive(Deserialize)]
struct CreateInvoice {
    so: String,
    #[serde(default)]
    invoice_date: Option<String>,
    lines: Vec<InvoiceLineIn>,
}

fn so_lines(so_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("so_line").filter("so", so_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

pub(crate) fn refresh_so_state(so_id: &str) -> Result<Record> {
    let so = require("so", so_id, "sales order")?;
    if matches!(text(&so, "state"), Some("cancelled" | "closed" | "draft")) {
        return Ok(so);
    }
    let lines = so_lines(so_id)?;
    if lines.is_empty() {
        return Ok(so);
    }
    let mut any = false;
    let mut all = true;
    for line in &lines {
        let qty = qty_of(line, "qty")?;
        let delivered = qty_of(line, "qty_delivered")?;
        if !delivered.is_zero() {
            any = true;
        }
        if delivered < qty {
            all = false;
        }
    }
    let state = if all && any {
        "delivered"
    } else if any {
        "partially_delivered"
    } else {
        "confirmed"
    };
    let updated = db::update("so", so_id, &json!({ "state": state }))?.ok_or_else(|| Error::msg("the order is gone"))?;
    if state == "delivered" {
        // Nothing is left to deliver, so no hold should remain (a line quantity lowered after the hold, say).
        let company = text(&updated, "company").unwrap_or_default().to_string();
        for need in open_demand(&updated, &lines, false)? {
            let _ = release_for_order(&company, &need.item, &need.warehouse, so_id, None, need.tracked, None);
        }
    }
    Ok(updated)
}

fn do_create_so(input: CreateSo) -> Result<Record> {
    require_user()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let customer = require_customer(&input.customer)?;
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
        "customer": id_of(&customer)?,
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
    if let Some(deal) = input.deal.filter(|d| !d.is_empty()) {
        data["deal"] = json!(deal.trim());
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
    db::create("so", &data).map_err(|e| e.or("could not create the sales order"))
}

fn party_for_deal(deal: &Record) -> Result<String> {
    if let Some(customer_id) = text(deal, "customer") {
        let customer: Record = plugins::call("crm_customer", "get_customer", &json!({ "id": customer_id }))?;
        if let Some(party) = text(&customer, "party") {
            return Ok(party.to_string());
        }
    }
    if let Some(org) = text(deal, "org_party") {
        return Ok(org.to_string());
    }
    if let Some(contact) = text(deal, "contact_party") {
        return Ok(contact.to_string());
    }
    Err(Error::msg("the deal needs a customer party before a sales order can be created"))
}

fn do_create_so_from_deal(input: CreateSoFromDeal) -> Result<Record> {
    require_user()?;
    let deal: Record = plugins::call("crm_deal", "get_deal", &json!({ "id": input.deal }))?;
    let company = text(&deal, "company").ok_or_else(|| Error::msg("deal has no company"))?;
    let customer = party_for_deal(&deal)?;
    let currency = match text(&deal, "currency") {
        Some(id) => match require("currency.currency", id, "currency") {
            Ok(row) => text(&row, "code").map(str::to_string),
            Err(_) => Some(id.to_string()), // already a code
        },
        None => None,
    };
    let notes = input.notes.or_else(|| text(&deal, "title").map(|t| format!("From deal: {t}")));
    // Multiple SOs per deal are allowed; win stays on crm_deal.
    do_create_so(CreateSo {
        company: company.to_string(),
        customer,
        currency,
        order_date: None,
        warehouse: input.warehouse,
        notes,
        deal: Some(id_of(&deal)?.to_string()),
        payment_term: None,
        price_list: None,
    })
}

fn do_add_so_line(input: AddSoLine) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if text(&so, "state") != Some("draft") {
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
            &json!({ "item": id_of(&item)?, "qty": entered_qty, "from_uom": u, "context": "sales" }),
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
    let (unit_price, list_discount) =
        crate::common::resolve_sell_price(&so, "order_date", id_of(&item)?, qty, input_unit_price)?;
    if unit_price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    let discount_pct = input.discount_pct.unwrap_or(list_discount).with_scale(4)?;
    let tax = input.tax.filter(|t| !t.is_empty());
    let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
    let existing = so_lines(id_of(&so)?)?;
    let line_no = existing.iter().map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0)).max().unwrap_or(0) + 1;
    let mut data = json!({
        "so": id_of(&so)?,
        "line_no": line_no,
        "item": id_of(&item)?,
        "qty": qty,
        "uom": uom,
        "unit_price": unit_price,
        "discount_pct": discount_pct,
        "amount": priced.net,
        "tax_amount": priced.tax,
        "qty_delivered": Decimal::zero(QTY_SCALE),
        "qty_invoiced": Decimal::zero(QTY_SCALE),
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
    if let Some(wh) = input.warehouse.or_else(|| text(&so, "warehouse").map(str::to_string)) {
        data["warehouse"] = json!(wh);
    }
    let line = db::create("so_line", &data).map_err(|e| e.or("could not add the line"))?;
    let (subtotal, tax_total) = sum_lines(&so_lines(id_of(&so)?)?)?;
    let _: Option<Record> = db::update(
        "so",
        id_of(&so)?,
        &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
    )?;
    Ok(line)
}

/// One order line's stock need: item, warehouse and what is still to deliver.
struct Need {
    item: String,
    warehouse: String,
    open: Decimal,
    tracked: bool,
}

/// Stocked lines summed per (item, warehouse); with `only_open`, just those still to deliver. Non-stocked items and lines with no warehouse
/// are left out.
fn open_demand(so: &Record, lines: &[Record], only_open: bool) -> Result<Vec<Need>> {
    let mut rows: Vec<Need> = Vec::new();
    for line in lines {
        let open = qty_of(line, "qty")?.checked_sub(qty_of(line, "qty_delivered")?)?;
        let Some(warehouse) = text(line, "warehouse").or_else(|| text(so, "warehouse")) else { continue };
        if only_open && (open.is_zero() || open.is_negative()) {
            continue;
        }
        let item_id = text(line, "item").unwrap_or_default();
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": item_id }))?;
        if item.get("is_stocked") == Some(&json!(false)) {
            continue;
        }
        let tracked = !matches!(text(&item, "tracking"), None | Some("none"));
        match rows.iter_mut().find(|r| r.item == item_id && r.warehouse == warehouse) {
            Some(r) => r.open = r.open.checked_add(open)?,
            None => rows.push(Need { item: item_id.to_string(), warehouse: warehouse.to_string(), open, tracked }),
        }
    }
    Ok(rows)
}

/// A lot (or serial) on hand with how much of it is held and what is free.
struct LotView {
    lot: String,
    /// Held for this order.
    held: Decimal,
    /// On hand and not held by any order.
    free: Decimal,
}

/// Unexpired lots of an item in a warehouse, earliest expiry first (no expiry last), with this order's hold.
fn lot_views(company: &str, item: &str, warehouse: &str, so_id: &str) -> Result<Vec<LotView>> {
    let stock: Value = plugins::call(
        "erp_inventory",
        "suggest_lots",
        &json!({ "company": company, "item": item, "warehouse": warehouse }),
    )?;
    let mut lots: Vec<(Option<String>, LotView)> = Vec::new();
    for row in stock.get("lots").and_then(Value::as_array).into_iter().flatten() {
        if row.get("expired") == Some(&json!(true)) {
            continue;
        }
        let Some(lot) = text(row, "lot") else { continue };
        let held: Value = plugins::call(
            "erp_inventory",
            "reserved",
            &json!({
                "company": company, "item": item, "warehouse": warehouse, "lot": lot,
                "demand_kind": "erp_selling.so", "demand_id": so_id,
            }),
        )?;
        let on_hand = qty_field(row, "qty")?;
        let all_held = qty_field(&held, "reserved")?;
        let mine = qty_field(&held, "for_demand")?;
        let free = on_hand.checked_sub(all_held)?;
        let free = if free.is_negative() { Decimal::zero(QTY_SCALE) } else { free };
        lots.push((text(row, "expiry_date").map(str::to_string), LotView { lot: lot.to_string(), held: mine, free }));
    }
    lots.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => x.cmp(y).then(a.1.lot.cmp(&b.1.lot)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.lot.cmp(&b.1.lot),
    });
    Ok(lots.into_iter().map(|(_, v)| v).collect())
}

/// What to take from which lot to cover `qty` for the order: lots it already holds first, then the free stock
/// of the others, each earliest expiry first. Returns the picks and the part no lot covers.
fn plan_lots(views: &[LotView], qty: Decimal) -> Result<(Vec<(String, Decimal)>, Decimal)> {
    let mut left = qty;
    let mut picks = Vec::new();
    for pass_held in [true, false] {
        for v in views.iter().filter(|v| (v.held > Decimal::zero(QTY_SCALE)) == pass_held) {
            if left.is_zero() {
                break;
            }
            let usable = v.held.checked_add(v.free)?;
            let take = if usable < left { usable } else { left };
            if take > Decimal::zero(QTY_SCALE) {
                picks.push((v.lot.clone(), take));
                left = left.checked_sub(take)?;
            }
        }
    }
    Ok((picks, left))
}

/// Hold the free stock for a confirmed order, as much as there is. A shortfall is not an error: the order is
/// still valid and replenishment sees the demand. Lot and serial items are held lot by lot, earliest expiry
/// first. Returns what was held and what is short, per item.
fn reserve_for_order(so: &Record) -> Result<Vec<Value>> {
    let so_id = id_of(so)?;
    let company = text(so, "company").unwrap_or_default();
    let mut report = Vec::new();
    for need in open_demand(so, &so_lines(so_id)?, true)? {
        let (item, warehouse) = (need.item.as_str(), need.warehouse.as_str());
        let zero = Decimal::zero(QTY_SCALE);
        let mut taken: Vec<(Option<String>, Decimal)> = Vec::new();
        let short;
        if need.tracked {
            let views = lot_views(company, item, warehouse, so_id)?;
            let mut held = zero.clone();
            for v in &views {
                held = held.checked_add(v.held.clone())?;
            }
            let want = need.open.checked_sub(held)?;
            if want.is_zero() || want.is_negative() {
                continue;
            }
            // Only free stock is new to hold; what the order already holds is counted in `held`.
            let fresh: Vec<LotView> =
                views.into_iter().map(|v| LotView { lot: v.lot, held: Decimal::zero(QTY_SCALE), free: v.free }).collect();
            let (picks, left) = plan_lots(&fresh, want)?;
            taken = picks.into_iter().map(|(l, q)| (Some(l), q)).collect();
            short = left;
        } else {
            let held = held_for_order(company, item, warehouse, so_id, None)?;
            let want = need.open.checked_sub(held)?;
            if want.is_zero() || want.is_negative() {
                continue;
            }
            let stock: Value = plugins::call(
                "erp_inventory",
                "on_hand",
                &json!({ "company": company, "item": item, "warehouse": warehouse }),
            )?;
            let free = qty_field(&stock, "available")?;
            let take = if free < want { free } else { want };
            taken.push((None, take.clone()));
            short = want.checked_sub(take)?;
        }
        let mut reserved = zero.clone();
        for (lot, qty) in &taken {
            if *qty > zero {
                let mut body = json!({
                    "company": company, "item": item, "warehouse": warehouse, "qty": qty,
                    "demand_kind": "erp_selling.so", "demand_id": so_id,
                });
                if let Some(l) = lot {
                    body["lot"] = json!(l);
                }
                let _: Record = plugins::call("erp_inventory", "reserve", &body)?;
                reserved = reserved.checked_add(qty.clone())?;
            }
        }
        report.push(json!({ "item": item, "warehouse": warehouse, "reserved": reserved, "short": short }));
    }
    Ok(report)
}

fn held_for_order(company: &str, item: &str, warehouse: &str, so_id: &str, lot: Option<&str>) -> Result<Decimal> {
    let mut body = json!({
        "company": company, "item": item, "warehouse": warehouse,
        "demand_kind": "erp_selling.so", "demand_id": so_id,
    });
    if let Some(l) = lot {
        body["lot"] = json!(l);
    }
    let held: Value = plugins::call("erp_inventory", "reserved", &body)?;
    qty_field(&held, "for_demand")
}

fn qty_field(value: &Value, field: &str) -> Result<Decimal> {
    match value.get(field).filter(|v| !v.is_null()) {
        Some(v) => serde_json::from_value::<Decimal>(v.clone())
            .map_err(|e| Error::msg(format!("{field}: {e}")))?
            .with_scale(QTY_SCALE),
        None => Ok(Decimal::zero(QTY_SCALE)),
    }
}

/// Give back up to `upto` of the order's hold on one item (all of it when `None`). `lot` names the lot of a
/// tracked item; with `None` on a tracked item every lot the order holds is released.
fn release_for_order(
    company: &str,
    item: &str,
    warehouse: &str,
    so_id: &str,
    lot: Option<&str>,
    tracked: bool,
    upto: Option<Decimal>,
) -> Result<()> {
    let zero = Decimal::zero(QTY_SCALE);
    let mut targets: Vec<Option<String>> = vec![lot.map(str::to_string)];
    if tracked && lot.is_none() {
        targets = lot_views_all(company, item, warehouse)?.into_iter().map(Some).collect();
    }
    let mut left = upto;
    for target in targets {
        let held = held_for_order(company, item, warehouse, so_id, target.as_deref())?;
        let give = match &left {
            Some(q) if *q < held => q.clone(),
            _ => held,
        };
        if give > zero {
            let mut body = json!({
                "company": company, "item": item, "warehouse": warehouse, "qty": give,
                "demand_kind": "erp_selling.so", "demand_id": so_id,
            });
            if let Some(l) = &target {
                body["lot"] = json!(l);
            }
            let _: Record = plugins::call("erp_inventory", "release", &body)?;
            if let Some(q) = left.as_mut() {
                *q = q.checked_sub(give)?;
            }
        }
    }
    Ok(())
}

/// Every lot code of an item on hand in the warehouse, expired ones included (a hold may sit on one).
fn lot_views_all(company: &str, item: &str, warehouse: &str) -> Result<Vec<String>> {
    let stock: Value = plugins::call(
        "erp_inventory",
        "suggest_lots",
        &json!({ "company": company, "item": item, "warehouse": warehouse }),
    )?;
    Ok(stock
        .get("lots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| text(r, "lot").map(str::to_string))
        .collect())
}

fn do_confirm_so(input: Id) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    if text(&so, "state") != Some("draft") {
        return Err(Error::msg("only a draft order can be confirmed"));
    }
    if so_lines(&input.id)?.is_empty() {
        return Err(Error::msg("add at least one line before confirming"));
    }
    crate::quotes::enforce_credit(&so)?;
    let company_id = text(&so, "company").unwrap_or_default();
    let date = text(&so, "order_date").unwrap_or_default();
    let (number, next, counter_id) = allocate_number(company_id, "SO", date)?;
    db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("so", &input.id, &json!({ "state": "confirmed", "number": number }))
        .run()?;
    let confirmed = require("so", &input.id, "sales order")?;
    if crate::quotes::reserve_on_confirm(company_id)? {
        // The order is confirmed either way; a failed hold is visible on the stock card and can be redone.
        let _ = reserve_for_order(&confirmed);
    }
    Ok(confirmed)
}

/// Hold stock for an order that was confirmed before the setting was on, or top up after a stock receipt.
fn do_reserve_so(input: Id) -> Result<Value> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    if !matches!(text(&so, "state"), Some("confirmed" | "partially_delivered")) {
        return Err(Error::msg("hold stock for a confirmed order"));
    }
    Ok(json!({ "so": input.id, "lines": reserve_for_order(&so)? }))
}

fn do_deliver_so(input: DeliverSo) -> Result<Value> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if matches!(text(&so, "state"), Some("draft" | "cancelled" | "closed")) {
        return Err(Error::msg("deliver a confirmed order"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to deliver"));
    }
    crate::quotes::require_accepted_inspection(&so)?;
    let company_id = text(&so, "company").unwrap_or_default().to_string();
    let customer_id = text(&so, "customer").unwrap_or_default().to_string();
    let delivery_date = match input.delivery_date {
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
            "posting_date": delivery_date,
            "currency": text(&so, "currency"),
            "source_kind": "erp_selling.so",
            "source_id": input.so,
            "source_version": 1,
        }),
    )?;
    let move_id = id_of(&stock_move)?.to_string();
    let (number, next, counter_id) = allocate_number(&company_id, "SD", &delivery_date)?;
    let delivery: Record = db::create(
        "sales_delivery",
        &json!({
            "company": company_id,
            "so": input.so,
            "customer": customer_id,
            "stock_move": move_id,
            "state": "draft",
            "delivery_date": delivery_date,
            "number": number,
        }),
    )?;
    let delivery_id = id_of(&delivery)?.to_string();

    struct Planned {
        so_line_id: String,
        qty: Decimal,
        already: Decimal,
        item: String,
        warehouse: String,
        /// Where it came from: one entry per lot for a tracked item, a single lot-less entry otherwise.
        picks: Vec<(Option<String>, Decimal)>,
    }
    let mut planned = Vec::new();
    let mut line_no = 0i64;
    for spec in &input.lines {
        let so_line = require("so_line", &spec.so_line, "order line")?;
        if text(&so_line, "so") != Some(input.so.as_str()) {
            return Err(Error::msg("order line belongs to another sales order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("deliver quantity must be greater than zero"));
        }
        let ordered = qty_of(&so_line, "qty")?;
        let already = qty_of(&so_line, "qty_delivered")?;
        let open = ordered.checked_sub(already)?;
        if qty > open {
            return Err(Error::msg(format!("cannot deliver {qty}: only {open} open on the line")));
        }
        let warehouse = spec
            .warehouse
            .clone()
            .or_else(|| text(&so_line, "warehouse").map(str::to_string))
            .or_else(|| text(&so, "warehouse").map(str::to_string))
            .ok_or_else(|| Error::msg("name a warehouse to deliver from"))?;
        let item_id = text(&so_line, "item").unwrap_or_default();
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": item_id }))?;
        let picks: Vec<(Option<String>, Decimal)> = if matches!(text(&item, "tracking"), None | Some("none")) {
            vec![(None, qty.clone())]
        } else if let Some(lot) = spec.lot.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
            vec![(Some(lot.to_string()), qty.clone())]
        } else {
            // Not named: the lots the order holds first, then the rest earliest expiry first.
            let views = lot_views(&company_id, item_id, &warehouse, &input.so)?;
            let (plan, short) = plan_lots(&views, qty.clone())?;
            if short > Decimal::zero(QTY_SCALE) {
                return Err(Error::msg(format!(
                    "not enough unexpired stock by lot to deliver {qty}: {short} short; name a lot or receive more"
                )));
            }
            plan.into_iter().map(|(l, q)| (Some(l), q)).collect()
        };
        for (lot, part) in &picks {
            let mut line = json!({ "move": move_id, "item": item_id, "qty": part, "warehouse": warehouse });
            let mut row = json!({
                "delivery": delivery_id,
                "line_no": line_no + 1,
                "so_line": id_of(&so_line)?,
                "item": item_id,
                "qty": part,
                "warehouse": warehouse,
            });
            if let Some(l) = lot {
                line["lot"] = json!(l);
                row["lot"] = json!(l);
            }
            let _: Record = plugins::call("erp_inventory", "add_line", &line)?;
            line_no += 1;
            let _: Record = db::create("sales_delivery_line", &row)?;
        }
        planned.push(Planned {
            so_line_id: id_of(&so_line)?.to_string(),
            qty,
            already,
            item: item_id.to_string(),
            warehouse: warehouse.clone(),
            picks,
        });
    }

    let posted: Record = plugins::call("erp_inventory", "post_move", &json!({ "id": move_id }))?;
    let mut tx = db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update("sales_delivery", &delivery_id, &json!({ "state": "posted" }));
    for row in &planned {
        let new_d = row.already.checked_add(row.qty)?;
        tx = tx.update("so_line", &row.so_line_id, &json!({ "qty_delivered": new_d }));
    }
    tx.run().map_err(|e| e.or("could not finish the delivery"))?;
    // The goods are out: what they were holding goes back. Stock is already posted, so a failure here
    // must not fail the delivery.
    for row in &planned {
        for (lot, part) in &row.picks {
            let tracked = lot.is_some();
            let _ = release_for_order(&company_id, &row.item, &row.warehouse, &input.so, lot.as_deref(), tracked, Some(part.clone()));
        }
    }
    let so = refresh_so_state(&input.so)?;
    Ok(json!({
        "delivery": require("sales_delivery", &delivery_id, "delivery")?,
        "stock_move": posted,
        "so": so,
    }))
}

fn do_create_invoice(input: CreateInvoice) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.so, "sales order")?;
    if matches!(text(&so, "state"), Some("draft" | "cancelled")) {
        return Err(Error::msg("invoice a confirmed order"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("name what to invoice"));
    }
    let company_id = text(&so, "company").unwrap_or_default().to_string();
    let invoice_date = match input.invoice_date {
        Some(d) => {
            parse_date(&d)?;
            d
        }
        None => today()?,
    };
    let (number, next, counter_id) = allocate_number(&company_id, "SINV", &invoice_date)?;
    let invoice: Record = db::create(
        "sales_invoice",
        &json!({
            "company": company_id,
            "customer": text(&so, "customer"),
            "so": input.so,
            "currency": text(&so, "currency"),
            "state": "draft",
            "invoice_date": invoice_date,
            "total": Decimal::zero(MONEY_SCALE),
            "subtotal": Decimal::zero(MONEY_SCALE),
            "tax_total": Decimal::zero(MONEY_SCALE),
            "source_version": 1,
            "number": number,
        }),
    )?;
    let invoice_id = id_of(&invoice)?.to_string();
    if let Some(term) = text(&so, "payment_term") {
        let due: Value = plugins::call("erp_pricing", "due_date_for", &json!({ "term": term, "date": invoice_date }))?;
        let _: Option<Record> =
            db::update("sales_invoice", &invoice_id, &json!({ "payment_term": term, "due_date": due["due_date"] }))?;
    }
    let mut subtotal = Decimal::zero(MONEY_SCALE);
    let mut tax_total = Decimal::zero(MONEY_SCALE);
    let mut line_no = 0i64;
    for spec in &input.lines {
        let so_line = require("so_line", &spec.so_line, "order line")?;
        if text(&so_line, "so") != Some(input.so.as_str()) {
            return Err(Error::msg("order line belongs to another sales order"));
        }
        let qty = spec.qty.with_scale(QTY_SCALE)?;
        if qty.is_zero() || qty.is_negative() {
            return Err(Error::msg("invoice quantity must be greater than zero"));
        }
        let delivered = qty_of(&so_line, "qty_delivered")?;
        let invoiced = qty_of(&so_line, "qty_invoiced")?;
        let open = delivered.checked_sub(invoiced)?;
        if qty > open {
            return Err(Error::msg(format!(
                "cannot invoice {qty}: only {open} delivered and not yet invoiced"
            )));
        }
        let unit_price = spec.unit_price.unwrap_or(price_of(&so_line, "unit_price")?).with_scale(PRICE_SCALE)?;
        let discount_pct = dec(&so_line, "discount_pct", 4)?;
        let tax = text(&so_line, "tax").map(str::to_string);
        let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
        subtotal = subtotal.checked_add(priced.net)?;
        tax_total = tax_total.checked_add(priced.tax)?;
        line_no += 1;
        let mut data = json!({
            "invoice": invoice_id,
            "line_no": line_no,
            "so_line": id_of(&so_line)?,
            "item": text(&so_line, "item"),
            "qty": qty,
            "unit_price": unit_price,
            "discount_pct": discount_pct,
            "amount": priced.net,
            "tax_amount": priced.tax,
        });
        if let Some(t) = &tax {
            data["tax"] = json!(t);
        }
        if let Some(dl) = &spec.delivery_line {
            let _ = require("sales_delivery_line", dl, "delivery line")?;
            data["delivery_line"] = json!(dl);
        }
        let _: Record = db::create("sales_invoice_line", &data)?;
        let new_inv = invoiced.checked_add(qty)?;
        let _: Option<Record> = db::update("so_line", id_of(&so_line)?, &json!({ "qty_invoiced": new_inv }))?;
    }
    db::transaction()
        .update("sales_counter", &counter_id, &json!({ "next": next + 1 }))
        .update(
            "sales_invoice",
            &invoice_id,
            &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
        )
        .run()?;
    require("sales_invoice", &invoice_id, "invoice")
}

/// A credit note reduces what the customer owes: it mirrors the invoice's ledger lines.
fn emit_credit_note(id: &str, approved: Record) -> Result<Record> {
    let total = money_of(&approved, "total")?;
    events::emit(
        "sales_credit_note_approved",
        &json!({
            "invoice": id,
            "number": text(&approved, "number"),
            "company": text(&approved, "company"),
            "customer": text(&approved, "customer"),
            "currency": text(&approved, "currency"),
            "invoice_date": text(&approved, "invoice_date"),
            "total": approved.get("total"),
            "ar_total": total,
            "revenue": money_of(&approved, "subtotal")?,
            "tax_total": money_of(&approved, "tax_total")?,
            "reverses": text(&approved, "reverses"),
            "so": text(&approved, "so"),
            "source_version": approved.get("source_version").and_then(Value::as_i64).unwrap_or(1),
        }),
    )?;
    Ok(approved)
}

fn do_approve_invoice(input: Id) -> Result<Record> {
    require_user()?;
    let invoice = require("sales_invoice", &input.id, "invoice")?;
    if text(&invoice, "state") != Some("draft") {
        return Err(Error::msg("only a draft invoice can be approved"));
    }
    let approved = db::update("sales_invoice", &input.id, &json!({ "state": "approved" }))?
        .ok_or_else(|| Error::msg("the invoice is gone"))?;
    if text(&approved, "kind") == Some("credit_note") {
        return emit_credit_note(&input.id, approved);
    }
    let total = money_of(&approved, "total")?;
    let subtotal = money_of(&approved, "subtotal")?;
    let tax_total = money_of(&approved, "tax_total")?;
    // Invoices written before taxes existed carry only a total: treat it all as revenue.
    let revenue = if subtotal.is_zero() && tax_total.is_zero() { total } else { subtotal };
    events::emit(
        "sales_invoice_approved",
        &json!({
            "invoice": input.id,
            "number": text(&approved, "number"),
            "company": text(&approved, "company"),
            "customer": text(&approved, "customer"),
            "currency": text(&approved, "currency"),
            "invoice_date": text(&approved, "invoice_date"),
            "total": approved.get("total"),
            "ar_total": total,
            "revenue": revenue,
            "tax_total": tax_total,
            "due_date": text(&approved, "due_date"),
            "so": text(&approved, "so"),
            "source_version": approved.get("source_version").and_then(Value::as_i64).unwrap_or(1),
        }),
    )?;
    Ok(approved)
}

fn do_get_so(input: Id) -> Result<Value> {
    require_reader()?;
    let so = require("so", &input.id, "sales order")?;
    let lines = so_lines(&input.id)?;
    Ok(json!({ "so": so, "lines": lines }))
}

fn do_cancel_so(input: Id) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    if text(&so, "state") == Some("delivered") {
        return Err(Error::msg("a fully delivered order is closed, not cancelled"));
    }
    let lines = so_lines(&input.id)?;
    if lines.iter().any(|l| qty_of(l, "qty_delivered").map(|q| !q.is_zero()).unwrap_or(false)) {
        return Err(Error::msg("cancel only when nothing has been delivered"));
    }
    let cancelled = db::update("so", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the order is gone"))?;
    let company = text(&so, "company").unwrap_or_default();
    for need in open_demand(&so, &lines, true)? {
        let _ = release_for_order(company, &need.item, &need.warehouse, &input.id, None, need.tracked, None);
    }
    Ok(cancelled)
}

// ---- the order as one document: header edit, line edit / remove, deliver and invoice everything left ----

#[derive(Deserialize)]
struct UpdateSo {
    id: String,
    #[serde(default)]
    order_date: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    payment_term: Option<String>,
    #[serde(default)]
    price_list: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

fn require_draft(so: &Record, what: &str) -> Result<()> {
    if text(so, "state") != Some("draft") {
        return Err(Error::msg(format!("{what} can only be changed while the order is a draft (state: {})", text(so, "state").unwrap_or("unknown"))));
    }
    Ok(())
}

fn do_update_so(input: UpdateSo) -> Result<Record> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    require_draft(&so, "the header")?;
    let mut data = json!({});
    if let Some(d) = input.order_date.filter(|d| !d.is_empty()) {
        parse_date(&d)?;
        data["order_date"] = json!(d);
    }
    if let Some(wh) = input.warehouse.filter(|w| !w.is_empty()) {
        let _ = require("erp_inventory.stock_warehouse", &wh, "warehouse")?;
        data["warehouse"] = json!(wh);
    }
    if let Some(term) = input.payment_term.filter(|t| !t.is_empty()) {
        let _ = require("erp_pricing.payment_term", &term, "payment term")?;
        data["payment_term"] = json!(term);
    }
    if let Some(list) = input.price_list.filter(|l| !l.is_empty()) {
        let _ = require("erp_pricing.price_list", &list, "price list")?;
        data["price_list"] = json!(list);
    }
    if let Some(notes) = input.notes {
        data["notes"] = json!(notes);
    }
    db::update("so", &input.id, &data)?.ok_or_else(|| Error::msg("the order is gone"))
}

/// The order flattened for the document form.
fn do_get_so_doc(input: Id) -> Result<Record> {
    require_reader()?;
    require("so", &input.id, "sales order")
}

/// One line as the edit dialog shows it: quantity and price in the unit the line was entered in.
fn do_get_so_line(input: Id) -> Result<Value> {
    require_reader()?;
    let line = require("so_line", &input.id, "order line")?;
    let entered = line.get("order_uom").is_some();
    let pick = |own: &str, entered_field: &str| -> Value {
        if entered { line.get(entered_field).cloned().unwrap_or(Value::Null) } else { line.get(own).cloned().unwrap_or(Value::Null) }
    };
    Ok(json!({
        "id": line.get("id"),
        "item": line.get("item"),
        "qty": pick("qty", "order_qty"),
        "uom": line.get("order_uom").or_else(|| line.get("uom")),
        "unit_price": pick("unit_price", "order_unit_price"),
        "discount_pct": line.get("discount_pct"),
        "tax": line.get("tax"),
        "warehouse": line.get("warehouse"),
    }))
}

fn recompute_so(so_id: &str) -> Result<()> {
    let (subtotal, tax_total) = sum_lines(&so_lines(so_id)?)?;
    let _: Option<Record> = db::update(
        "so",
        so_id,
        &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct UpdateSoLine {
    id: String,
    item: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    tax: Option<String>,
    #[serde(default)]
    warehouse: Option<String>,
    #[serde(default)]
    uom: Option<String>,
}

/// Change a line of a draft order. The new line is built first (so a bad value changes nothing), then it takes
/// the old line's place.
fn do_update_so_line(input: UpdateSoLine) -> Result<Record> {
    require_user()?;
    let old = require("so_line", &input.id, "order line")?;
    let so_id = text(&old, "so").ok_or_else(|| Error::msg("the line has no order"))?.to_string();
    let so = require("so", &so_id, "sales order")?;
    require_draft(&so, "lines")?;
    let added = do_add_so_line(AddSoLine {
        so: so_id.clone(),
        item: input.item,
        qty: input.qty,
        unit_price: input.unit_price,
        discount_pct: input.discount_pct,
        tax: input.tax,
        warehouse: input.warehouse,
        uom: input.uom,
    })?;
    let kept = fold_line("so_line", &old, &added, &["so", "line_no"])?;
    recompute_so(&so_id)?;
    Ok(kept)
}


/// Move the values of a freshly built line onto an existing one and drop the new row, so the existing line keeps
/// its id (and its history) through an edit. Values the new row does not have are cleared.
fn fold_line(model: &str, old: &Record, new: &Record, keep: &[&str]) -> Result<Record> {
    let (Some(old_obj), Some(new_obj)) = (old.as_object(), new.as_object()) else {
        return Err(Error::msg("the line is not a record"));
    };
    let mut patch = serde_json::Map::new();
    for (k, v) in new_obj {
        if k != "id" && !keep.contains(&k.as_str()) {
            patch.insert(k.clone(), v.clone());
        }
    }
    for k in old_obj.keys() {
        if k != "id" && !keep.contains(&k.as_str()) && !new_obj.contains_key(k) {
            patch.insert(k.clone(), Value::Null);
        }
    }
    let updated = db::update(model, id_of(old)?, &Value::Object(patch))?.ok_or_else(|| Error::msg("the line is gone"))?;
    let _: Option<Record> = db::delete(model, id_of(new)?)?;
    Ok(updated)
}

fn do_remove_so_line(input: Id) -> Result<Record> {
    require_user()?;
    let line = require("so_line", &input.id, "order line")?;
    let so_id = text(&line, "so").ok_or_else(|| Error::msg("the line has no order"))?.to_string();
    let so = require("so", &so_id, "sales order")?;
    require_draft(&so, "lines")?;
    let gone = db::delete("so_line", &input.id)?.ok_or_else(|| Error::msg("the line is gone"))?;
    recompute_so(&so_id)?;
    Ok(gone)
}

/// Deliver whatever is still open on every line, from the order's own warehouse and the usual lot choice.
fn do_deliver_all_so(input: Id) -> Result<Value> {
    require_user()?;
    let so = require("so", &input.id, "sales order")?;
    let lines = so_lines(&input.id)?;
    let mut todo = Vec::new();
    for line in &lines {
        let left = qty_of(line, "qty")?.checked_sub(qty_of(line, "qty_delivered")?)?;
        if !left.is_zero() && !left.is_negative() {
            let warehouse = text(line, "warehouse").or_else(|| text(&so, "warehouse")).map(str::to_string);
            if warehouse.is_none() {
                return Err(Error::msg("set a warehouse on the order (or on each line) before delivering"));
            }
            todo.push(DeliverLine { so_line: id_of(line)?.to_string(), qty: left, warehouse, lot: None });
        }
    }
    if todo.is_empty() {
        return Err(Error::msg("everything on the order is already delivered"));
    }
    do_deliver_so(DeliverSo { so: input.id, delivery_date: None, lines: todo })
}

/// Invoice everything delivered and not yet invoiced.
fn do_invoice_all_so(input: Id) -> Result<Record> {
    require_user()?;
    let lines = so_lines(&input.id)?;
    let mut todo = Vec::new();
    for line in &lines {
        let left = qty_of(line, "qty_delivered")?.checked_sub(qty_of(line, "qty_invoiced")?)?;
        if !left.is_zero() && !left.is_negative() {
            todo.push(InvoiceLineIn { so_line: id_of(line)?.to_string(), qty: left, unit_price: None, delivery_line: None });
        }
    }
    if todo.is_empty() {
        return Err(Error::msg("nothing delivered is waiting to be invoiced"));
    }
    do_create_invoice(CreateInvoice { so: input.id, invoice_date: None, lines: todo })
}

// ---- the sales invoice as one document ----

#[derive(Deserialize)]
struct UpdateInvoice {
    id: String,
    #[serde(default)]
    invoice_date: Option<String>,
    #[serde(default)]
    due_date: Option<String>,
}

/// A draft invoice that is not a credit note; everything else is read-only.
fn editable_invoice(id: &str) -> Result<Record> {
    let invoice = require("sales_invoice", id, "invoice")?;
    if text(&invoice, "state") != Some("draft") {
        return Err(Error::msg(format!(
            "an invoice can only be changed while it is a draft (state: {})",
            text(&invoice, "state").unwrap_or("unknown")
        )));
    }
    if text(&invoice, "kind") == Some("credit_note") {
        return Err(Error::msg("credit notes are not edited here"));
    }
    Ok(invoice)
}

fn invoice_lines(invoice_id: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("sales_invoice_line").filter("invoice", invoice_id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn recompute_invoice(invoice_id: &str) -> Result<()> {
    let (subtotal, tax_total) = sum_lines(&invoice_lines(invoice_id)?)?;
    let _: Option<Record> = db::update(
        "sales_invoice",
        invoice_id,
        &json!({ "subtotal": subtotal, "tax_total": tax_total, "total": subtotal.checked_add(tax_total)? }),
    )?;
    Ok(())
}

fn do_update_invoice(input: UpdateInvoice) -> Result<Record> {
    require_user()?;
    editable_invoice(&input.id)?;
    let mut data = json!({});
    if let Some(d) = input.invoice_date.filter(|d| !d.is_empty()) {
        parse_date(&d)?;
        data["invoice_date"] = json!(d);
    }
    if let Some(d) = input.due_date.filter(|d| !d.is_empty()) {
        parse_date(&d)?;
        data["due_date"] = json!(d);
    }
    db::update("sales_invoice", &input.id, &data)?.ok_or_else(|| Error::msg("the invoice is gone"))
}

fn do_get_invoice_doc(input: Id) -> Result<Record> {
    require_reader()?;
    require("sales_invoice", &input.id, "invoice")
}

fn do_get_invoice_line(input: Id) -> Result<Record> {
    require_reader()?;
    require("sales_invoice_line", &input.id, "invoice line")
}

#[derive(Deserialize)]
struct UpdateInvoiceLine {
    id: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
}

/// Change the quantity or price of a draft invoice line. The quantity may go up to what is delivered and not yet
/// invoiced on the order line (counting this line's own quantity back in).
fn do_update_invoice_line(input: UpdateInvoiceLine) -> Result<Record> {
    require_user()?;
    let line = require("sales_invoice_line", &input.id, "invoice line")?;
    let invoice_id = text(&line, "invoice").ok_or_else(|| Error::msg("the line has no invoice"))?.to_string();
    editable_invoice(&invoice_id)?;
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("invoice quantity must be greater than zero"));
    }
    let so_line = require("so_line", text(&line, "so_line").unwrap_or_default(), "order line")?;
    let old_qty = qty_of(&line, "qty")?;
    let invoiced_elsewhere = qty_of(&so_line, "qty_invoiced")?.checked_sub(old_qty)?;
    let open = qty_of(&so_line, "qty_delivered")?.checked_sub(invoiced_elsewhere)?;
    if qty > open {
        return Err(Error::msg(format!("cannot invoice {qty}: only {open} delivered and not invoiced elsewhere")));
    }
    let unit_price = input.unit_price.unwrap_or(price_of(&line, "unit_price")?).with_scale(PRICE_SCALE)?;
    if unit_price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    let discount_pct = dec(&line, "discount_pct", 4)?;
    let tax = text(&line, "tax").map(str::to_string);
    let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
    let updated = db::update(
        "sales_invoice_line",
        &input.id,
        &json!({ "qty": qty, "unit_price": unit_price, "amount": priced.net, "tax_amount": priced.tax }),
    )?
    .ok_or_else(|| Error::msg("the line is gone"))?;
    let _: Option<Record> =
        db::update("so_line", id_of(&so_line)?, &json!({ "qty_invoiced": invoiced_elsewhere.checked_add(qty)? }))?;
    recompute_invoice(&invoice_id)?;
    Ok(updated)
}

#[derive(Deserialize)]
struct AddInvoiceLine {
    #[serde(alias = "id")]
    invoice: String,
    so_line: String,
    qty: Decimal,
    #[serde(default)]
    unit_price: Option<Decimal>,
}

/// Add a line to a draft invoice from an order line that still has delivered, uninvoiced quantity.
fn do_add_invoice_line(input: AddInvoiceLine) -> Result<Record> {
    require_user()?;
    let invoice = editable_invoice(&input.invoice)?;
    let so_line = require("so_line", &input.so_line, "order line")?;
    if text(&so_line, "so") != text(&invoice, "so") {
        return Err(Error::msg("that order line belongs to another sales order"));
    }
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("invoice quantity must be greater than zero"));
    }
    let invoiced = qty_of(&so_line, "qty_invoiced")?;
    let open = qty_of(&so_line, "qty_delivered")?.checked_sub(invoiced)?;
    if qty > open {
        return Err(Error::msg(format!("cannot invoice {qty}: only {open} delivered and not yet invoiced")));
    }
    let unit_price = input.unit_price.unwrap_or(price_of(&so_line, "unit_price")?).with_scale(PRICE_SCALE)?;
    if unit_price.is_negative() {
        return Err(Error::msg("unit price must not be negative"));
    }
    let discount_pct = dec(&so_line, "discount_pct", 4)?;
    let tax = text(&so_line, "tax").map(str::to_string);
    let priced = price_line(qty, unit_price, discount_pct, tax.as_deref())?;
    let line_no = invoice_lines(&input.invoice)?
        .iter()
        .map(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0))
        .max()
        .unwrap_or(0)
        + 1;
    let mut data = json!({
        "invoice": input.invoice,
        "line_no": line_no,
        "so_line": id_of(&so_line)?,
        "item": text(&so_line, "item"),
        "qty": qty,
        "unit_price": unit_price,
        "discount_pct": discount_pct,
        "amount": priced.net,
        "tax_amount": priced.tax,
    });
    if let Some(t) = &tax {
        data["tax"] = json!(t);
    }
    let line: Record = db::create("sales_invoice_line", &data).map_err(|e| e.or("could not add the line"))?;
    let _: Option<Record> = db::update("so_line", id_of(&so_line)?, &json!({ "qty_invoiced": invoiced.checked_add(qty)? }))?;
    recompute_invoice(&input.invoice)?;
    Ok(line)
}

/// Order lines of this invoice's order with something delivered and not yet invoiced, for the add-line picker.
fn do_list_invoiceable_lines(input: Id) -> Result<Vec<Value>> {
    require_reader()?;
    let invoice = require("sales_invoice", &input.id, "invoice")?;
    let mut out = Vec::new();
    for line in so_lines(text(&invoice, "so").unwrap_or_default())? {
        let open = qty_of(&line, "qty_delivered")?.checked_sub(qty_of(&line, "qty_invoiced")?)?;
        if open.is_zero() || open.is_negative() {
            continue;
        }
        let item = text(&line, "item").unwrap_or_default();
        let name = db::get::<Record>("erp_item.item", item)?
            .and_then(|i| text(&i, "name").map(str::to_string))
            .unwrap_or_else(|| item.to_string());
        out.push(json!({
            "id": line.get("id"),
            "display_name": format!("#{} {}: {} to invoice", line.get("line_no").and_then(Value::as_i64).unwrap_or(0), name, open),
        }));
    }
    Ok(out)
}

/// Give a line's quantity back to the order (it can be invoiced again) and delete the line.
fn release_invoice_line(line: &Record) -> Result<()> {
    if let Some(sl) = text(line, "so_line") {
        if let Some(so_line) = db::get::<Record>("so_line", sl)? {
            let back = qty_of(&so_line, "qty_invoiced")?.checked_sub(qty_of(line, "qty")?)?;
            let back = if back.is_negative() { Decimal::zero(QTY_SCALE) } else { back };
            let _: Option<Record> = db::update("so_line", sl, &json!({ "qty_invoiced": back }))?;
        }
    }
    let _: Option<Record> = db::delete("sales_invoice_line", id_of(line)?)?;
    Ok(())
}

fn do_remove_invoice_line(input: Id) -> Result<Record> {
    require_user()?;
    let line = require("sales_invoice_line", &input.id, "invoice line")?;
    let invoice_id = text(&line, "invoice").ok_or_else(|| Error::msg("the line has no invoice"))?.to_string();
    editable_invoice(&invoice_id)?;
    if invoice_lines(&invoice_id)?.len() <= 1 {
        return Err(Error::msg("this is the last line: use Discard draft to drop the whole invoice"));
    }
    release_invoice_line(&line)?;
    recompute_invoice(&invoice_id)?;
    Ok(line)
}

/// Drop a draft invoice: its quantities go back to the order to be invoiced again.
fn do_discard_invoice(input: Id) -> Result<Record> {
    require_user()?;
    editable_invoice(&input.id)?;
    for line in &invoice_lines(&input.id)? {
        release_invoice_line(line)?;
    }
    recompute_invoice(&input.id)?;
    db::update("sales_invoice", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the invoice is gone"))
}

/// In-crate entry points so quotations can open orders without crossing the handler boundary.
pub(crate) fn api_create_so(input: Value) -> Result<Record> {
    do_create_so(serde_json::from_value(input).map_err(|e| Error::msg(format!("order: {e}")))?)
}

pub(crate) fn api_add_so_line(input: Value) -> Result<Record> {
    do_add_so_line(serde_json::from_value(input).map_err(|e| Error::msg(format!("line: {e}")))?)
}

handler! {
    fn create_so(input: CreateSo) -> Record { do_create_so(input) }
    fn create_so_from_deal(input: CreateSoFromDeal) -> Record { do_create_so_from_deal(input) }
    fn add_so_line(input: AddSoLine) -> Record { do_add_so_line(input) }
    fn confirm_so(input: Id) -> Record { do_confirm_so(input) }
    fn cancel_so(input: Id) -> Record { do_cancel_so(input) }
    fn reserve_so(input: Id) -> Value { do_reserve_so(input) }
    fn deliver_so(input: DeliverSo) -> Value { do_deliver_so(input) }
    fn create_invoice(input: CreateInvoice) -> Record { do_create_invoice(input) }
    fn approve_invoice(input: Id) -> Record { do_approve_invoice(input) }
    fn get_so(input: Id) -> Value { do_get_so(input) }
    fn update_so(input: UpdateSo) -> Record { do_update_so(input) }
    fn get_so_doc(input: Id) -> Record { do_get_so_doc(input) }
    fn get_so_line(input: Id) -> Value { do_get_so_line(input) }
    fn update_so_line(input: UpdateSoLine) -> Record { do_update_so_line(input) }
    fn remove_so_line(input: Id) -> Record { do_remove_so_line(input) }
    fn deliver_all_so(input: Id) -> Value { do_deliver_all_so(input) }
    fn update_invoice(input: UpdateInvoice) -> Record { do_update_invoice(input) }
    fn get_invoice_doc(input: Id) -> Record { do_get_invoice_doc(input) }
    fn get_invoice_line(input: Id) -> Record { do_get_invoice_line(input) }
    fn update_invoice_line(input: UpdateInvoiceLine) -> Record { do_update_invoice_line(input) }
    fn remove_invoice_line(input: Id) -> Record { do_remove_invoice_line(input) }
    fn add_invoice_line(input: AddInvoiceLine) -> Record { do_add_invoice_line(input) }
    fn list_invoiceable_lines(input: Id) -> Vec<Value> { do_list_invoiceable_lines(input) }
    fn discard_invoice(input: Id) -> Record { do_discard_invoice(input) }
    fn invoice_all_so(input: Id) -> Record { do_invoice_all_so(input) }
}
