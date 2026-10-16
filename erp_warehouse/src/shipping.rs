//! Shipments: packages leaving on a carrier. The delivery itself (stock out, invoice) is posted
//! by selling; a shipment is the physical hand-over and its tracking.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_reader, require_user, text, today, Record};

#[derive(Deserialize)]
struct PackageIn {
    code: String,
    #[serde(default)]
    weight_kg: Option<Decimal>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct CreateShipment {
    company: String,
    #[serde(default)]
    pick: Option<String>,
    #[serde(default)]
    carrier: Option<String>,
    #[serde(default)]
    tracking: Option<String>,
    #[serde(default)]
    packages: Vec<PackageIn>,
}

#[derive(Deserialize)]
struct Id {
    #[serde(alias = "id")]
    shipment: String,
}

#[derive(Deserialize)]
struct AddPackage {
    #[serde(alias = "id")]
    shipment: String,
    code: String,
    #[serde(default)]
    weight_kg: Option<Decimal>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize, Default)]
struct ListFilter {
    #[serde(default)]
    state: Option<String>,
}

fn package_row(p: &PackageIn) -> Result<Value> {
    let mut row = json!({ "code": p.code.trim() });
    if let Some(w) = &p.weight_kg {
        if w.is_negative() {
            return Err(Error::msg("a weight is not negative"));
        }
        row["weight_kg"] = json!(w.clone().with_scale(3)?);
    }
    if let Some(n) = p.note.as_ref().filter(|n| !n.is_empty()) {
        row["note"] = json!(n);
    }
    Ok(row)
}

fn do_create(input: CreateShipment) -> Result<Record> {
    require_user()?;
    let mut data = json!({ "company": input.company, "state": "draft" });
    if let Some(pick) = input.pick.filter(|p| !p.is_empty()) {
        let pick = require("pick_list", &pick, "pick list")?;
        if text(&pick, "state") != Some("picked") {
            return Err(Error::msg("ship a pick list that has been picked"));
        }
        data["pick"] = json!(id_of(&pick)?);
    }
    for (key, value) in [("carrier", &input.carrier), ("tracking", &input.tracking)] {
        if let Some(v) = value.as_ref().filter(|v| !v.is_empty()) {
            data[key] = json!(v.trim());
        }
    }
    let mut packages = Vec::new();
    for p in &input.packages {
        packages.push(package_row(p)?);
    }
    if !packages.is_empty() {
        data["packages"] = json!(packages);
    }
    db::create("shipment", &data).map_err(|e| e.or("could not create the shipment"))
}

fn do_add_package(input: AddPackage) -> Result<Record> {
    require_user()?;
    let shipment = require("shipment", &input.shipment, "shipment")?;
    if text(&shipment, "state") != Some("draft") {
        return Err(Error::msg("packages are added while the shipment is a draft"));
    }
    let mut row = package_row(&PackageIn { code: input.code, weight_kg: input.weight_kg, note: input.note })?;
    row["shipment"] = json!(id_of(&shipment)?);
    db::create("shipment_package", &row).map_err(|e| e.or("could not add the package"))
}

fn do_ship(input: Id) -> Result<Record> {
    require_user()?;
    let shipment: Record = db::get_expanded("shipment", &input.shipment, &["packages"])?
        .ok_or_else(|| Error::msg("no such shipment"))?;
    let has_packages = shipment.get("packages").and_then(Value::as_array).map_or(false, |p| !p.is_empty());
    if !has_packages {
        return Err(Error::msg("add at least one package before shipping"));
    }
    db::update("shipment", &input.shipment, &json!({ "state": "shipped", "shipped_on": today()? }))?
        .ok_or_else(|| Error::msg("the shipment is gone"))
}

fn move_to(shipment: &str, state: &str) -> Result<Record> {
    require_user()?;
    db::update("shipment", shipment, &json!({ "state": state }))?.ok_or_else(|| Error::msg("the shipment is gone"))
}

fn do_list(input: ListFilter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("shipment").limit(500);
    if let Some(s) = input.state.filter(|s| !s.is_empty()) {
        q = q.filter("state", s.as_str());
    }
    let mut rows = q.all()?;
    rows.sort_by(|a, b| text(b, "number").cmp(&text(a, "number")));
    Ok(rows)
}

fn do_list_packages(input: Id) -> Result<Vec<Record>> {
    require_reader()?;
    db::find("shipment_package").filter("shipment", input.shipment.as_str()).limit(500).all()
}

handler! {
    fn create_shipment(input: CreateShipment) -> Record { do_create(input) }
    fn add_package(input: AddPackage) -> Record { do_add_package(input) }
    fn ship_shipment(input: Id) -> Record { do_ship(input) }
    fn deliver_shipment(input: Id) -> Record { move_to(&input.shipment, "delivered") }
    fn cancel_shipment(input: Id) -> Record { move_to(&input.shipment, "cancelled") }
    fn list_shipments(input: ListFilter) -> Vec<Record> { do_list(input) }
    fn list_packages(input: Id) -> Vec<Record> { do_list_packages(input) }
}
