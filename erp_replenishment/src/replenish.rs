//! Reorder rules, shortfall requests and their conversion into purchase / manufacturing orders.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, qty_of, require, require_admin, require_reader, text, today, Record, QTY_SCALE};
use crate::rules::{reorder_qty, shortfall};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct RuleInput {
    company: String,
    item: String,
    warehouse: String,
    min_qty: Decimal,
    max_qty: Decimal,
    #[serde(default)]
    multiple: Option<Decimal>,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    supplier: Option<String>,
    #[serde(default)]
    bom: Option<String>,
    #[serde(default)]
    lead_days: Option<i64>,
}

#[derive(Deserialize)]
struct SetRule {
    id: String,
    #[serde(default)]
    min_qty: Option<Decimal>,
    #[serde(default)]
    max_qty: Option<Decimal>,
    #[serde(default)]
    multiple: Option<Decimal>,
    #[serde(default)]
    supplier: Option<String>,
    #[serde(default)]
    bom: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct Evaluate {
    company: String,
    #[serde(default)]
    warehouse: Option<String>,
}

#[derive(Deserialize)]
struct ForSo {
    so: String,
}

#[derive(Deserialize)]
struct ListRequests {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct Convert {
    id: String,
    #[serde(default)]
    supplier: Option<String>,
    #[serde(default)]
    bom: Option<String>,
    /// Buy route only when no buy price list covers the item.
    #[serde(default)]
    unit_price: Option<Decimal>,
}

fn do_create_rule(input: RuleInput) -> Result<Record> {
    require_admin()?;
    let company = require("company.company", &input.company, "company")?;
    let item = require("erp_item.item", &input.item, "item")?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let min = input.min_qty.with_scale(QTY_SCALE)?;
    let max = input.max_qty.with_scale(QTY_SCALE)?;
    if min.is_negative() || max.is_negative() {
        return Err(Error::msg("min and max must not be negative"));
    }
    if max < min {
        return Err(Error::msg("max must not be below min"));
    }
    let route = input.route.as_deref().unwrap_or("buy");
    if !matches!(route, "buy" | "make") {
        return Err(Error::msg("route is buy or make"));
    }
    if item.get("is_stocked") == Some(&json!(false)) {
        return Err(Error::msg("only a stocked item can have a reorder rule"));
    }
    let mut data = json!({
        "company": id_of(&company)?,
        "item": id_of(&item)?,
        "warehouse": id_of(&warehouse)?,
        "min_qty": min,
        "max_qty": max,
        "multiple": input.multiple.unwrap_or_else(|| Decimal::parse("1").unwrap()).with_scale(QTY_SCALE)?,
        "route": route,
        "lead_days": input.lead_days.unwrap_or(0),
        "active": true,
    });
    if let Some(s) = input.supplier.filter(|s| !s.is_empty()) {
        let party = require("party.party", &s, "supplier")?;
        data["supplier"] = json!(id_of(&party)?);
    }
    if let Some(b) = input.bom.filter(|b| !b.is_empty()) {
        let _ = require("erp_manufacturing.bom", &b, "BOM")?;
        data["bom"] = json!(b);
    }
    db::create("reorder_rule", &data).map_err(|e| e.or("could not create the rule (one rule per item and warehouse)"))
}

fn do_set_rule(input: SetRule) -> Result<Record> {
    require_admin()?;
    let current = require("reorder_rule", &input.id, "rule")?;
    let min = match input.min_qty {
        Some(v) => v.with_scale(QTY_SCALE)?,
        None => qty_of(&current, "min_qty")?,
    };
    let max = match input.max_qty {
        Some(v) => v.with_scale(QTY_SCALE)?,
        None => qty_of(&current, "max_qty")?,
    };
    if max < min || min.is_negative() {
        return Err(Error::msg("max must not be below min, and neither may be negative"));
    }
    let mut changes = json!({ "min_qty": min, "max_qty": max });
    if let Some(m) = input.multiple {
        changes["multiple"] = json!(m.with_scale(QTY_SCALE)?);
    }
    if let Some(s) = input.supplier {
        changes["supplier"] = if s.is_empty() { Value::Null } else { json!(s) };
    }
    if let Some(b) = input.bom {
        changes["bom"] = if b.is_empty() { Value::Null } else { json!(b) };
    }
    if let Some(a) = input.active {
        changes["active"] = json!(a);
    }
    db::update("reorder_rule", &input.id, &changes)?.ok_or_else(|| Error::msg("the rule is gone"))
}

fn available_at(company: &str, item: &str, warehouse: &str) -> Result<Decimal> {
    let on_hand: Value = plugins::call(
        "erp_inventory",
        "on_hand",
        &json!({ "company": company, "item": item, "warehouse": warehouse }),
    )?;
    qty_of(&on_hand, "available")
}

/// Quantity already on its way into `warehouse`: open purchase-order lines plus open manufacturing orders.
fn incoming(company: &str, item: &str, warehouse: &str) -> Result<Decimal> {
    let mut total = Decimal::zero(QTY_SCALE);
    let lines: Vec<Record> = db::find("erp_procurement.po_line").filter("item", item).limit(2000).all()?;
    for line in &lines {
        let po_id = text(line, "po").unwrap_or_default();
        let po = require("erp_procurement.po", po_id, "purchase order")?;
        if !matches!(text(&po, "state"), Some("confirmed" | "partially_received")) || text(&po, "company") != Some(company) {
            continue;
        }
        let target = text(line, "warehouse").or_else(|| text(&po, "warehouse"));
        if target != Some(warehouse) {
            continue;
        }
        let open = qty_of(line, "qty")?.checked_sub(qty_of(line, "qty_received")?)?;
        if !open.is_negative() {
            total = total.checked_add(open)?;
        }
    }
    let orders: Vec<Record> = db::find("erp_manufacturing.mo")
        .filter("item", item)
        .filter("warehouse", warehouse)
        .filter("company", company)
        .limit(2000)
        .all()?;
    for mo in &orders {
        if matches!(text(mo, "state"), Some("confirmed" | "reserved" | "in_progress")) {
            total = total.checked_add(qty_of(mo, "qty")?)?;
        }
    }
    Ok(total)
}

fn open_requests_for(rule_id: &str) -> Result<usize> {
    let rows: Vec<Record> = db::find("replenish_request")
        .filter("rule", rule_id)
        .filter("state", "open")
        .limit(5)
        .all()?;
    Ok(rows.len())
}

fn do_evaluate(input: Evaluate) -> Result<Value> {
    require_admin()?;
    let company = require("company.company", &input.company, "company")?;
    let company_id = id_of(&company)?.to_string();
    let mut q = db::find::<Record>("reorder_rule").filter("company", company_id.as_str()).filter("active", true).limit(1000);
    if let Some(w) = input.warehouse.filter(|w| !w.is_empty()) {
        q = q.filter("warehouse", w);
    }
    let rules = q.all()?;
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for rule in &rules {
        let rule_id = id_of(rule)?.to_string();
        let item = text(rule, "item").unwrap_or_default().to_string();
        let warehouse = text(rule, "warehouse").unwrap_or_default().to_string();
        let available = available_at(&company_id, &item, &warehouse)?;
        let coming = incoming(&company_id, &item, &warehouse)?;
        let qty = reorder_qty(
            available,
            coming,
            qty_of(rule, "min_qty")?,
            qty_of(rule, "max_qty")?,
            qty_of(rule, "multiple")?,
        )?;
        let Some(qty) = qty else {
            continue;
        };
        if open_requests_for(&rule_id)? > 0 {
            skipped.push(json!({ "rule": rule_id, "reason": "an open request already exists" }));
            continue;
        }
        let mut data = json!({
            "company": company_id,
            "item": item,
            "warehouse": warehouse,
            "qty": qty,
            "route": text(rule, "route").unwrap_or("buy"),
            "state": "open",
            "reason": "reorder",
            "rule": rule_id,
        });
        if let Some(s) = text(rule, "supplier") {
            data["supplier"] = json!(s);
        }
        created.push(db::create::<Record>("replenish_request", &data)?);
    }
    Ok(json!({ "evaluated": rules.len(), "created": created, "skipped": skipped }))
}

/// Shortfall of a confirmed sales order against free stock: one request per short line.
fn do_request_for_so(input: ForSo) -> Result<Value> {
    require_admin()?;
    let so = require("erp_selling.so", &input.so, "sales order")?;
    if !matches!(text(&so, "state"), Some("confirmed" | "partially_delivered")) {
        return Err(Error::msg("check shortfall for a confirmed order"));
    }
    let company_id = text(&so, "company").unwrap_or_default().to_string();
    let lines: Vec<Record> = db::find("erp_selling.so_line").filter("so", input.so.as_str()).limit(500).all()?;
    let mut created = Vec::new();
    for line in &lines {
        let line_id = id_of(line)?.to_string();
        let open = qty_of(line, "qty")?.checked_sub(qty_of(line, "qty_delivered")?)?;
        if open.is_zero() || open.is_negative() {
            continue;
        }
        let item = text(line, "item").unwrap_or_default().to_string();
        let warehouse = match text(line, "warehouse").or_else(|| text(&so, "warehouse")) {
            Some(w) => w.to_string(),
            None => continue,
        };
        let existing: Option<Record> = db::find("replenish_request")
            .filter("demand_id", line_id.as_str())
            .filter("state", "open")
            .first()?;
        if existing.is_some() {
            continue;
        }
        let available = available_at(&company_id, &item, &warehouse)?;
        let coming = incoming(&company_id, &item, &warehouse)?;
        let missing = shortfall(open, available.checked_add(coming)?)?;
        if missing.is_zero() {
            continue;
        }
        let rule: Option<Record> = db::find("reorder_rule")
            .filter("company", company_id.as_str())
            .filter("item", item.as_str())
            .filter("warehouse", warehouse.as_str())
            .filter("active", true)
            .first()?;
        let route = rule.as_ref().and_then(|r| text(r, "route")).unwrap_or("buy");
        let mut data = json!({
            "company": company_id,
            "item": item,
            "warehouse": warehouse,
            "qty": missing,
            "route": route,
            "state": "open",
            "reason": "demand",
            "demand_kind": "erp_selling.so_line",
            "demand_id": line_id,
        });
        if let Some(r) = &rule {
            data["rule"] = json!(id_of(r)?);
            if let Some(s) = text(r, "supplier") {
                data["supplier"] = json!(s);
            }
        }
        created.push(db::create::<Record>("replenish_request", &data)?);
    }
    Ok(json!({ "so": input.so, "created": created }))
}

fn do_list_requests(input: ListRequests) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("replenish_request").limit(500);
    if let Some(state) = input.state.filter(|s| !s.is_empty()) {
        q = q.filter("state", state);
    }
    q.all()
}

fn finish(request_id: &str, kind: &str, result_id: &str) -> Result<Record> {
    db::update(
        "replenish_request",
        request_id,
        &json!({ "state": "converted", "result_kind": kind, "result_id": result_id }),
    )?
    .ok_or_else(|| Error::msg("the request is gone"))
}

fn do_convert(input: Convert) -> Result<Value> {
    require_admin()?;
    let request = require("replenish_request", &input.id, "request")?;
    if text(&request, "state") != Some("open") {
        return Err(Error::msg("only an open request can be converted"));
    }
    let company = text(&request, "company").unwrap_or_default().to_string();
    let item = text(&request, "item").unwrap_or_default().to_string();
    let warehouse = text(&request, "warehouse").unwrap_or_default().to_string();
    let qty = qty_of(&request, "qty")?;
    let rule = match text(&request, "rule") {
        Some(id) => Some(require("reorder_rule", id, "rule")?),
        None => None,
    };
    match text(&request, "route") {
        Some("make") => {
            let bom = match input.bom.filter(|b| !b.is_empty()).or_else(|| rule.as_ref().and_then(|r| text(r, "bom").map(str::to_string))) {
                Some(b) => b,
                None => {
                    let found: Option<Record> = db::find("erp_manufacturing.bom")
                        .filter("item", item.as_str())
                        .filter("company", company.as_str())
                        .filter("active", true)
                        .first()?;
                    let found = found.ok_or_else(|| Error::msg("no active BOM for this item; name one"))?;
                    id_of(&found)?.to_string()
                }
            };
            let mo: Record = plugins::call(
                "erp_manufacturing",
                "create_mo",
                &json!({ "company": company, "bom": bom, "qty": qty, "warehouse": warehouse, "planned_date": text(&request, "needed_by") }),
            )?;
            let done = finish(&input.id, "mo", id_of(&mo)?)?;
            Ok(json!({ "request": done, "mo": mo }))
        }
        _ => {
            let supplier = input
                .supplier
                .filter(|s| !s.is_empty())
                .or_else(|| text(&request, "supplier").map(str::to_string))
                .or_else(|| rule.as_ref().and_then(|r| text(r, "supplier").map(str::to_string)))
                .ok_or_else(|| Error::msg("name a supplier for this purchase"))?;
            let po: Record = plugins::call(
                "erp_procurement",
                "create_po",
                &json!({ "company": company, "supplier": supplier, "warehouse": warehouse, "order_date": today()? }),
            )?;
            let po_id = id_of(&po)?.to_string();
            let line: Record = plugins::call(
                "erp_procurement",
                "add_po_line",
                &json!({ "po": po_id, "item": item, "qty": qty, "unit_price": input.unit_price, "warehouse": warehouse }),
            )?;
            let done = finish(&input.id, "po", &po_id)?;
            Ok(json!({ "request": done, "po": po, "line": line }))
        }
    }
}

fn do_cancel_request(input: Id) -> Result<Record> {
    require_admin()?;
    let request = require("replenish_request", &input.id, "request")?;
    if text(&request, "state") != Some("open") {
        return Err(Error::msg("only an open request can be cancelled"));
    }
    db::update("replenish_request", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the request is gone"))
}

fn do_list_rules(_: ListRequests) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("reorder_rule").limit(1000).all()?;
    rows.sort_by(|a, b| text(a, "item").cmp(&text(b, "item")));
    Ok(rows)
}

handler! {
    fn list_rules(input: ListRequests) -> Vec<Record> { do_list_rules(input) }
    fn create_rule(input: RuleInput) -> Record { do_create_rule(input) }
    fn set_rule(input: SetRule) -> Record { do_set_rule(input) }
    fn evaluate(input: Evaluate) -> Value { do_evaluate(input) }
    fn request_for_so(input: ForSo) -> Value { do_request_for_so(input) }
    fn list_requests(input: ListRequests) -> Vec<Record> { do_list_requests(input) }
    fn convert_request(input: Convert) -> Value { do_convert(input) }
    fn cancel_request(input: Id) -> Record { do_cancel_request(input) }
}
