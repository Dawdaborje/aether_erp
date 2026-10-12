//! Warehouse master.

use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_admin, require_reader, text, Record, WAREHOUSE_KINDS};

#[derive(Deserialize)]
struct WarehouseInput {
    code: String,
    name: String,
    company: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct SetWarehouse {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Deserialize)]
struct ListWarehouses {
    company: String,
    #[serde(default)]
    active_only: Option<bool>,
}

fn do_create_warehouse(input: WarehouseInput) -> Result<Record> {
    require_admin()?;
    let kind = input.kind.as_deref().unwrap_or("stock");
    if !WAREHOUSE_KINDS.contains(&kind) {
        return Err(Error::msg("kind is stock, view, transit or scrap"));
    }
    let company = require("company.company", &input.company, "company")?;
    let mut data = json!({
        "code": input.code.trim(),
        "name": input.name.trim(),
        "company": id_of(&company)?,
        "kind": kind,
        "active": true,
    });
    if let Some(parent_id) = input.parent {
        let parent = require("stock_warehouse", &parent_id, "warehouse")?;
        if text(&parent, "company") != Some(id_of(&company)?) {
            return Err(Error::msg("the parent warehouse belongs to another company"));
        }
        data["parent"] = json!(id_of(&parent)?);
    }
    db::create("stock_warehouse", &data).map_err(|e| e.or("could not create the warehouse (the code may be taken)"))
}

fn do_set_warehouse(input: SetWarehouse) -> Result<Record> {
    require_admin()?;
    let _ = require("stock_warehouse", &input.id, "warehouse")?;
    let mut changes = json!({});
    if let Some(name) = input.name {
        changes["name"] = json!(name.trim());
    }
    if let Some(kind) = input.kind {
        if !WAREHOUSE_KINDS.contains(&kind.as_str()) {
            return Err(Error::msg("kind is stock, view, transit or scrap"));
        }
        changes["kind"] = json!(kind);
    }
    if let Some(active) = input.active {
        changes["active"] = json!(active);
    }
    if let Some(parent_id) = input.parent {
        if parent_id.is_empty() {
            changes["parent"] = Value::Null;
        } else {
            if parent_id == input.id {
                return Err(Error::msg("a warehouse cannot be its own parent"));
            }
            let parent = require("stock_warehouse", &parent_id, "warehouse")?;
            changes["parent"] = json!(id_of(&parent)?);
        }
    }
    db::update("stock_warehouse", &input.id, &changes)?.ok_or_else(|| Error::msg("the warehouse is gone"))
}

fn do_list_warehouses(input: ListWarehouses) -> Result<Vec<Record>> {
    require_reader()?;
    let company = require("company.company", &input.company, "company")?;
    let mut rows: Vec<Record> = db::find("stock_warehouse")
        .filter("company", id_of(&company)?)
        .limit(500)
        .all()?;
    if input.active_only.unwrap_or(true) {
        rows.retain(|r| r.get("active") != Some(&json!(false)));
    }
    rows.sort_by(|a, b| text(a, "code").cmp(&text(b, "code")));
    Ok(rows)
}

handler! {
    fn create_warehouse(input: WarehouseInput) -> Record { do_create_warehouse(input) }
    fn set_warehouse(input: SetWarehouse) -> Record { do_set_warehouse(input) }
    fn list_warehouses(input: ListWarehouses) -> Vec<Record> { do_list_warehouses(input) }
}
