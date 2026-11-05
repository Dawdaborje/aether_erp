//! Supplier and customer item data (their codes, minimum quantity, lead time) and per-item
//! unit conversion.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_admin, require_reader, text, Record};

const USE_FOR: &[&str] = &["both", "purchase", "sales"];

#[derive(Deserialize)]
struct SupplierInput {
    #[serde(alias = "id")]
    item: String,
    supplier: String,
    #[serde(default)]
    supplier_sku: Option<String>,
    #[serde(default)]
    min_qty: Option<Decimal>,
    #[serde(default)]
    lead_days: Option<i64>,
    #[serde(default)]
    is_preferred: Option<bool>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct CustomerInput {
    #[serde(alias = "id")]
    item: String,
    customer: String,
    #[serde(default)]
    customer_sku: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct UomInput {
    #[serde(alias = "id")]
    item: String,
    uom: String,
    factor: Decimal,
    #[serde(default)]
    use_for: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct ItemRef {
    #[serde(alias = "id")]
    item: String,
}

#[derive(Deserialize)]
struct Convert {
    item: String,
    qty: Decimal,
    from_uom: String,
    /// Default: the item's own unit.
    #[serde(default)]
    to_uom: Option<String>,
    /// `purchase` or `sales` limits which conversions apply (default: any).
    #[serde(default)]
    context: Option<String>,
}

/// Create the row for `(item, party)` or change the existing one (the key is unique).
fn upsert(model: &str, item: &str, party_field: &str, party: &str, data: Value) -> Result<Record> {
    let key = json!({ "item": item, party_field: party });
    let (record, _created) = db::upsert::<Record>(model, &key, &data)?;
    Ok(record)
}

fn do_set_item_supplier(input: SupplierInput) -> Result<Record> {
    require_admin()?;
    let item = require("item", &input.item, "item")?;
    let supplier = require("party.party", &input.supplier, "supplier")?;
    let (item_id, supplier_id) = (id_of(&item)?.to_string(), id_of(&supplier)?.to_string());
    let mut data = json!({ "item": item_id, "supplier": supplier_id });
    if let Some(sku) = input.supplier_sku {
        data["supplier_sku"] = json!(sku.trim());
    }
    if let Some(min) = input.min_qty {
        if min.is_negative() {
            return Err(Error::msg("the minimum quantity must not be negative"));
        }
        data["min_qty"] = json!(min.with_scale(6)?);
    }
    if let Some(days) = input.lead_days {
        if days < 0 {
            return Err(Error::msg("lead time must not be negative"));
        }
        data["lead_days"] = json!(days);
    }
    if let Some(active) = input.active {
        data["active"] = json!(active);
    }
    // The model's `unique_when` allows one preferred supplier per item, so the old one is cleared first.
    if input.is_preferred == Some(true) {
        let current: Vec<Record> = db::find("item_supplier")
            .filter("item", &item_id)
            .filter("is_preferred", true)
            .limit(10)
            .all()?;
        for other in current {
            if text(&other, "supplier") != Some(supplier_id.as_str()) {
                let _: Option<Record> = db::update("item_supplier", id_of(&other)?, &json!({ "is_preferred": false }))?;
            }
        }
        data["is_preferred"] = json!(true);
    } else if let Some(p) = input.is_preferred {
        data["is_preferred"] = json!(p);
    }
    upsert("item_supplier", &item_id, "supplier", &supplier_id, data)
}

fn do_list_item_suppliers(input: ItemRef) -> Result<Vec<Record>> {
    require_reader()?;
    db::find("item_supplier").filter("item", &input.item).filter("active", true).limit(200).all()
}

fn do_set_item_customer(input: CustomerInput) -> Result<Record> {
    require_admin()?;
    let item = require("item", &input.item, "item")?;
    let customer = require("party.party", &input.customer, "customer")?;
    let (item_id, customer_id) = (id_of(&item)?.to_string(), id_of(&customer)?.to_string());
    let mut data = json!({ "item": item_id, "customer": customer_id });
    if let Some(sku) = input.customer_sku {
        data["customer_sku"] = json!(sku.trim());
    }
    if let Some(active) = input.active {
        data["active"] = json!(active);
    }
    upsert("item_customer", &item_id, "customer", &customer_id, data)
}

fn do_list_item_customers(input: ItemRef) -> Result<Vec<Record>> {
    require_reader()?;
    db::find("item_customer").filter("item", &input.item).filter("active", true).limit(200).all()
}

fn do_set_item_uom(input: UomInput) -> Result<Record> {
    require_admin()?;
    let item = require("item", &input.item, "item")?;
    let uom = require("uom.uom", &input.uom, "unit")?;
    if id_of(&uom)? == text(&item, "uom").unwrap_or_default() {
        return Err(Error::msg("that is the item's own unit; it always converts at 1"));
    }
    if input.factor.is_zero() || input.factor.is_negative() {
        return Err(Error::msg("the factor must be greater than zero"));
    }
    let use_for = input.use_for.as_deref().unwrap_or("both");
    if !USE_FOR.contains(&use_for) {
        return Err(Error::msg("use_for is both, purchase or sales"));
    }
    let (item_id, uom_id) = (id_of(&item)?.to_string(), id_of(&uom)?.to_string());
    let mut data = json!({
        "item": item_id, "uom": uom_id,
        "factor": input.factor.with_scale(6)?, "use_for": use_for,
    });
    if let Some(active) = input.active {
        data["active"] = json!(active);
    }
    upsert("item_uom", &item_id, "uom", &uom_id, data)
}

fn do_list_item_uoms(input: ItemRef) -> Result<Vec<Record>> {
    require_reader()?;
    db::find("item_uom").filter("item", &input.item).filter("active", true).limit(100).all()
}

/// Factor of `uom` in the item's own unit: 1 for that unit, else the item's row, else the
/// ratio of the two units' reference factors when they share a group.
fn factor_of(item: &Record, uom_id: &str, context: Option<&str>) -> Result<Decimal> {
    let own = text(item, "uom").unwrap_or_default();
    if uom_id == own {
        return Decimal::whole(1, 6);
    }
    let item_id = id_of(item)?;
    let row: Option<Record> = db::find("item_uom")
        .filter("item", item_id)
        .filter("uom", uom_id)
        .filter("active", true)
        .first()?;
    if let Some(row) = row {
        let use_for = text(&row, "use_for").unwrap_or("both");
        if use_for != "both" && context.map_or(false, |c| c != use_for) {
            return Err(Error::msg(format!("that unit is only used for {use_for}")));
        }
        let factor = row.get("factor").cloned().unwrap_or(Value::Null);
        return serde_json::from_value::<Decimal>(factor).map_err(|e| Error::msg(format!("factor: {e}")));
    }
    // Fall back to the unit group's reference ratio.
    let from = require("uom.uom", uom_id, "unit")?;
    let base = require("uom.uom", own, "unit")?;
    if text(&from, "group") != text(&base, "group") {
        return Err(Error::msg("there is no conversion between those units for this item"));
    }
    let num = from.get("factor").and_then(Value::as_f64).unwrap_or(0.0);
    let den = base.get("factor").and_then(Value::as_f64).unwrap_or(0.0);
    if num <= 0.0 || den <= 0.0 {
        return Err(Error::msg("a unit has no factor"));
    }
    Decimal::parse(&format!("{:.6}", num / den))
}

fn do_convert_qty(input: Convert) -> Result<Value> {
    require_reader()?;
    let item = require("item", &input.item, "item")?;
    let ctx = input.context.as_deref();
    let from = factor_of(&item, &input.from_uom, ctx)?;
    let to_uom = input.to_uom.unwrap_or_else(|| text(&item, "uom").unwrap_or_default().to_string());
    let to = factor_of(&item, &to_uom, ctx)?;
    if to.is_zero() {
        return Err(Error::msg("the target unit has no factor"));
    }
    let base_qty = input.qty.times(from, 6)?;
    let qty = base_qty.div(to, 6)?;
    Ok(json!({ "qty": qty, "base_qty": base_qty, "uom": to_uom }))
}

handler! {
    fn set_item_supplier(input: SupplierInput) -> Record { do_set_item_supplier(input) }
    fn list_item_suppliers(input: ItemRef) -> Vec<Record> { do_list_item_suppliers(input) }
    fn set_item_customer(input: CustomerInput) -> Record { do_set_item_customer(input) }
    fn list_item_customers(input: ItemRef) -> Vec<Record> { do_list_item_customers(input) }
    fn set_item_uom(input: UomInput) -> Record { do_set_item_uom(input) }
    fn list_item_uoms(input: ItemRef) -> Vec<Record> { do_list_item_uoms(input) }
    fn convert_qty(input: Convert) -> Value { do_convert_qty(input) }
}
