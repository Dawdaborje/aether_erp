//! The cart, payment (which issues the stock), voids and the till dashboard.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;
use serde_json::json;

use crate::common::{
    caller, dec, id_of, money_of, open_session_of, require, require_cashier, require_manager, require_reader, text, today, Record,
    MONEY_SCALE, PRICE_SCALE, QTY_SCALE,
};

#[derive(Deserialize)]
struct NewSale {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    customer: Option<String>,
}

#[derive(Deserialize)]
struct AddLine {
    #[serde(alias = "id")]
    sale: String,
    item: String,
    qty: Decimal,
    /// Omit to take the price from the profile's sell price list.
    #[serde(default)]
    unit_price: Option<Decimal>,
    #[serde(default)]
    discount_pct: Option<Decimal>,
    #[serde(default)]
    tax: Option<String>,
    /// For a lot or serial tracked item.
    #[serde(default)]
    lot: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Pay {
    #[serde(alias = "sale")]
    id: String,
    #[serde(default)]
    cash: Option<Decimal>,
    #[serde(default)]
    card: Option<Decimal>,
    #[serde(default)]
    other: Option<Decimal>,
}

#[derive(Deserialize)]
struct Void {
    id: String,
    reason: String,
}

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    session: Option<String>,
}

/// A cart the caller may still change, with its session and profile.
fn open_cart(id: &str) -> Result<(Record, Record, Record)> {
    let sale = require("pos_sale", id, "sale")?;
    if text(&sale, "state") != Some("draft") {
        return Err(Error::msg("only an open cart can be changed"));
    }
    let session = open_session_of(text(&sale, "session").unwrap_or_default())?;
    let profile = require("pos_profile", text(&sale, "profile").unwrap_or_default(), "profile")?;
    Ok((sale, session, profile))
}

fn do_new_sale(input: NewSale) -> Result<Record> {
    require_cashier()?;
    let session = match input.session.as_deref().filter(|s| !s.is_empty()) {
        Some(id) => open_session_of(id)?,
        None => db::find::<Record>("pos_session")
            .filter("cashier", caller()?.as_str())
            .filter("state", "open")
            .first()?
            .ok_or_else(|| Error::msg("open a session first"))?,
    };
    let profile = require("pos_profile", text(&session, "profile").unwrap_or_default(), "profile")?;
    let mut data = json!({
        "session": id_of(&session)?,
        "profile": id_of(&profile)?,
        "company": text(&profile, "company"),
        "state": "draft",
        "sale_date": today()?,
    });
    if let Some(currency) = text(&profile, "currency") {
        data["currency"] = json!(currency);
    }
    if let Some(customer) = input.customer.as_deref().filter(|c| !c.is_empty()) {
        let party = require("party.party", customer, "customer")?;
        data["customer"] = json!(id_of(&party)?);
    }
    db::create("pos_sale", &data)
}

/// Totals follow the lines, every time a line is added or removed.
fn refresh_totals(sale_id: &str) -> Result<Record> {
    let lines: Vec<Record> = db::find("pos_sale_line").filter("sale", sale_id).limit(500).all()?;
    let (mut subtotal, mut tax) = (Decimal::zero(MONEY_SCALE), Decimal::zero(MONEY_SCALE));
    for line in &lines {
        subtotal = subtotal.checked_add(money_of(line, "amount")?)?;
        tax = tax.checked_add(money_of(line, "tax_amount")?)?;
    }
    let total = subtotal.checked_add(tax.clone())?;
    db::update("pos_sale", sale_id, &json!({ "subtotal": subtotal, "tax_total": tax, "total": total }))?
        .ok_or_else(|| Error::msg("the sale is gone"))
}

fn do_add_line(input: AddLine) -> Result<Record> {
    require_cashier()?;
    let (sale, _session, profile) = open_cart(&input.sale)?;
    let qty = input.qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("the quantity must be above zero"));
    }
    let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": input.item }))?;
    if item.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the item is not active"));
    }
    let item_id = id_of(&item)?.to_string();
    let (unit_price, list_discount) = match input.unit_price {
        Some(price) => (price.with_scale(PRICE_SCALE)?, Decimal::zero(4)),
        None => {
            let found: Value = plugins::call(
                "erp_pricing",
                "price_for",
                &json!({
                    "item": item_id,
                    "kind": "sell",
                    "qty": qty,
                    "price_list": text(&profile, "price_list"),
                    "date": text(&sale, "sale_date"),
                    "currency": text(&profile, "currency"),
                    "company": text(&sale, "company"),
                }),
            )?;
            if found.get("found") != Some(&json!(true)) {
                return Err(Error::msg("no sell price found for this item; enter a unit price"));
            }
            (dec(&found, "unit_price", PRICE_SCALE)?, dec(&found, "discount_pct", 4)?)
        }
    };
    let discount = match input.discount_pct {
        Some(d) => d.with_scale(4)?,
        None => list_discount,
    };
    let priced: Value = plugins::call(
        "erp_pricing",
        "compute_line",
        &json!({ "qty": qty, "unit_price": unit_price, "discount_pct": discount, "tax": input.tax, "side": "sales" }),
    )?;
    let sale_id = id_of(&sale)?.to_string();
    let count = db::find::<Record>("pos_sale_line").filter("sale", sale_id.as_str()).limit(500).count()?;
    let mut row = json!({
        "sale": sale_id,
        "line_no": count + 1,
        "item": item_id,
        "qty": qty,
        "unit_price": unit_price,
        "discount_pct": discount,
        "amount": dec(&priced, "net", MONEY_SCALE)?,
        "tax_amount": dec(&priced, "tax", MONEY_SCALE)?,
    });
    if let Some(tax) = input.tax.as_deref().filter(|t| !t.is_empty()) {
        row["tax"] = json!(tax);
    }
    if let Some(lot) = input.lot.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        row["lot"] = json!(lot);
    }
    let line: Record = db::create("pos_sale_line", &row)?;
    refresh_totals(&sale_id)?;
    Ok(line)
}

fn do_remove_line(input: Id) -> Result<Record> {
    require_cashier()?;
    let line = require("pos_sale_line", &input.id, "line")?;
    let sale_id = text(&line, "sale").unwrap_or_default().to_string();
    open_cart(&sale_id)?;
    let gone: Record = db::delete("pos_sale_line", &input.id)?.ok_or_else(|| Error::msg("the line is gone"))?;
    refresh_totals(&sale_id)?;
    Ok(gone)
}

fn do_sale_lines(input: Id) -> Result<Vec<Record>> {
    require_reader()?;
    db::find::<Record>("pos_sale_line").filter("sale", input.id.as_str()).order_by("line_no").limit(500).all()
}

/// Take payment and issue the stock in one step. A sale that stock cannot supply is not recorded as paid.
fn do_pay(input: Pay) -> Result<Value> {
    require_cashier()?;
    let (sale, session, profile) = open_cart(&input.id)?;
    let sale_id = id_of(&sale)?.to_string();
    let lines: Vec<Record> = db::find("pos_sale_line").filter("sale", sale_id.as_str()).order_by("line_no").limit(500).all()?;
    if lines.is_empty() {
        return Err(Error::msg("the cart is empty"));
    }
    let zero = Decimal::zero(MONEY_SCALE);
    let tender = [("cash", input.cash), ("card", input.card), ("other", input.other)];
    let mut payments = Vec::new();
    let mut paid = zero.clone();
    let mut cash = zero.clone();
    for (method, amount) in tender {
        let Some(amount) = amount else { continue };
        let amount = amount.with_scale(MONEY_SCALE)?;
        if amount.is_zero() {
            continue;
        }
        if amount.is_negative() {
            return Err(Error::msg("a payment must be above zero"));
        }
        paid = paid.checked_add(amount.clone())?;
        if method == "cash" {
            cash = amount.clone();
        }
        payments.push((method, amount));
    }
    let total = money_of(&sale, "total")?;
    if paid < total {
        return Err(Error::msg(format!("{paid} paid is short of the total {total}")));
    }
    let change = paid.checked_sub(total.clone())?;
    if change > cash {
        return Err(Error::msg("change can only be given out of cash"));
    }

    let company_id = text(&sale, "company").unwrap_or_default().to_string();
    let warehouse = text(&profile, "warehouse").unwrap_or_default().to_string();
    let date = today()?;
    let mut move_id: Option<String> = None;
    for line in &lines {
        let item_id = text(line, "item").unwrap_or_default();
        let item: Record = plugins::call("erp_item", "get_item", &json!({ "id": item_id }))?;
        if item.get("is_stocked") == Some(&json!(false)) {
            continue;
        }
        if move_id.is_none() {
            let draft: Record = plugins::call(
                "erp_inventory",
                "draft_issue",
                &json!({
                    "company": company_id,
                    "posting_date": date,
                    "currency": text(&sale, "currency"),
                    "source_kind": "erp_pos.sale",
                    "source_id": sale_id,
                    "source_version": 1,
                }),
            )?;
            move_id = Some(id_of(&draft)?.to_string());
        }
        let mut body = json!({
            "move": move_id,
            "item": item_id,
            "qty": line.get("qty"),
            "warehouse": warehouse,
        });
        if let Some(lot) = text(line, "lot") {
            body["lot"] = json!(lot);
        }
        let _: Record = plugins::call("erp_inventory", "add_line", &body)?;
    }
    if let Some(id) = &move_id {
        let _: Record = plugins::call("erp_inventory", "post_move", &json!({ "id": id }))?;
    }

    let session_id = id_of(&session)?.to_string();
    let mut tx = db::transaction().update(
        "pos_sale",
        &sale_id,
        &json!({ "state": "paid", "paid": paid, "change": change, "stock_move": move_id }),
    );
    for (method, amount) in &payments {
        tx = tx.create("pos_payment", &json!({ "sale": sale_id, "session": session_id, "method": method, "amount": amount }));
    }
    if let Err(e) = tx.run() {
        if let Some(id) = &move_id {
            let _ = plugins::call::<Record>("erp_inventory", "void_move", &json!({ "id": id, "reason": "POS sale could not be recorded" }));
        }
        return Err(e.or("could not record the sale; the stock issue was taken back where the ledger allows it"));
    }
    let done = require("pos_sale", &sale_id, "sale")?;
    events::emit(
        "pos_sale_paid",
        &json!({
            "sale": sale_id,
            "number": text(&done, "number"),
            "company": company_id,
            "session": session_id,
            "currency": text(&done, "currency"),
            "sale_date": text(&done, "sale_date"),
            "subtotal": done.get("subtotal"),
            "tax_total": done.get("tax_total"),
            "total": done.get("total"),
            "cash": cash,
            "change": change,
            "stock_move": move_id,
        }),
    )?;
    Ok(json!({ "id": sale_id, "number": text(&done, "number"), "total": done.get("total"), "change": done.get("change") }))
}

/// A manager takes a paid sale back while its session is still open: the stock goes back, the sale stops counting.
fn do_void(input: Void) -> Result<Record> {
    require_manager()?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the sale is voided"));
    }
    let sale = require("pos_sale", &input.id, "sale")?;
    if text(&sale, "state") != Some("paid") {
        return Err(Error::msg("only a paid sale can be voided"));
    }
    open_session_of(text(&sale, "session").unwrap_or_default()).map_err(|_| Error::msg("the session is closed: a void after closing needs a return"))?;
    if let Some(move_id) = text(&sale, "stock_move").filter(|m| !m.is_empty()) {
        let _: Record = plugins::call("erp_inventory", "void_move", &json!({ "id": move_id, "reason": input.reason.trim() }))?;
    }
    let voided = db::update("pos_sale", &input.id, &json!({ "state": "void", "void_reason": input.reason.trim() }))?
        .ok_or_else(|| Error::msg("the sale is gone"))?;
    events::emit(
        "pos_sale_voided",
        &json!({
            "sale": input.id,
            "number": text(&voided, "number"),
            "company": text(&voided, "company"),
            "total": voided.get("total"),
            "reason": input.reason.trim(),
        }),
    )?;
    Ok(voided)
}

fn do_list_sales(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("pos_sale").limit(500);
    if let Some(state) = f.state.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("state", state);
    }
    if let Some(session) = f.session.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("session", session);
    }
    q.all()
}

/// Sales and takings of the open session(s) the caller can see.
fn do_dashboard(_: Filter) -> Result<Value> {
    require_reader()?;
    let mut sessions = db::find::<Record>("pos_session").filter("state", "open");
    if !crate::common::is_manager() {
        sessions = sessions.filter("cashier", caller()?.as_str());
    }
    let sessions: Vec<Record> = sessions.limit(100).all()?;
    let (mut count, mut carts) = (0u32, 0u32);
    let mut total = Decimal::zero(MONEY_SCALE);
    for session in &sessions {
        let sales: Vec<Record> = db::find("pos_sale").filter("session", id_of(session)?).limit(1000).all()?;
        for sale in &sales {
            match text(sale, "state") {
                Some("paid") => {
                    count += 1;
                    total = total.checked_add(money_of(sale, "total")?)?;
                }
                Some("draft") => carts += 1,
                _ => {}
            }
        }
    }
    Ok(json!({ "open_sessions": sessions.len(), "sales": count, "takings": total, "open_carts": carts }))
}

handler! {
    fn new_sale(input: NewSale) -> Record { do_new_sale(input) }
    fn add_sale_line(input: AddLine) -> Record { do_add_line(input) }
    fn remove_sale_line(input: Id) -> Record { do_remove_line(input) }
    fn list_sale_lines(input: Id) -> Vec<Record> { do_sale_lines(input) }
    fn pay_sale(input: Pay) -> Value { do_pay(input) }
    fn void_sale(input: Void) -> Record { do_void(input) }
    fn list_sales(input: Filter) -> Vec<Record> { do_list_sales(input) }
    fn pos_dashboard(input: Filter) -> Value { do_dashboard(input) }
}
