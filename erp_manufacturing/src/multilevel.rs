//! Multi-level BOMs: explosion, cost roll-up and child manufacturing orders.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{dec, id_of, qty_of, require, require_reader, require_user, text, Record, PCT_SCALE, QTY_SCALE, VALUE_SCALE};
use crate::orders::bom_lines;
use crate::rules::{explode_tree, operation_cost, raw_materials, BomNode, Need, NodeLine};

#[derive(Deserialize)]
struct Explode {
    bom: String,
    qty: Decimal,
}

#[derive(Deserialize)]
struct Rollup {
    bom: String,
    company: String,
    #[serde(default)]
    qty: Option<Decimal>,
    #[serde(default)]
    as_of: Option<String>,
    /// Cost per item for materials with no standard cost (item id → unit cost).
    #[serde(default)]
    costs: Option<std::collections::HashMap<String, Decimal>>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn node_of(bom_id: &str) -> Result<BomNode> {
    let bom = require("bom", bom_id, "BOM")?;
    let lines = bom_lines(bom_id)?
        .into_iter()
        .map(|r| {
            Ok(NodeLine {
                item_id: text(&r, "item").unwrap_or_default().to_string(),
                qty_per: qty_of(&r, "qty")?,
                scrap_pct: dec(&r, "scrap_pct", PCT_SCALE)?,
                is_byproduct: r.get("is_byproduct") == Some(&json!(true)),
                sub_bom: text(&r, "sub_bom").map(str::to_string),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(BomNode { output_qty: qty_of(&bom, "qty")?, lines })
}

/// Whether `target` is `from` or appears anywhere beneath it.
pub(crate) fn reaches(from: &str, target: &str) -> Result<bool> {
    let mut stack = vec![from.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = stack.pop() {
        if current == target {
            return Ok(true);
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        for line in bom_lines(&current)? {
            if let Some(sub) = text(&line, "sub_bom") {
                stack.push(sub.to_string());
            }
        }
    }
    Ok(false)
}

fn needs_for(bom: &str, qty: Decimal) -> Result<Vec<Need>> {
    let mut fetch = |id: &str| node_of(id);
    explode_tree(bom, qty, &mut fetch)
}

fn do_explode_bom(input: Explode) -> Result<Value> {
    require_reader()?;
    let _ = require("bom", &input.bom, "BOM")?;
    let needs = needs_for(&input.bom, input.qty)?;
    let raw = raw_materials(&needs)?;
    Ok(json!({
        "components": needs,
        "raw_materials": raw.iter().map(|(item, qty)| json!({ "item": item, "qty": qty })).collect::<Vec<_>>(),
    }))
}

/// Material cost per raw item plus routing cost of the BOM and every sub-assembly BOM in the tree.
fn do_rollup_cost(input: Rollup) -> Result<Value> {
    require_reader()?;
    let bom = require("bom", &input.bom, "BOM")?;
    let company = require("company.company", &input.company, "company")?;
    let qty = input.qty.unwrap_or_else(|| Decimal::parse("1").unwrap()).with_scale(QTY_SCALE)?;
    let needs = needs_for(&input.bom, qty)?;
    let raw = raw_materials(&needs)?;
    let overrides = input.costs.unwrap_or_default();
    let mut material = Decimal::zero(VALUE_SCALE);
    let mut missing = Vec::new();
    let mut lines = Vec::new();
    for (item, need_qty) in &raw {
        let unit = match overrides.get(item) {
            Some(c) => Some(c.with_scale(VALUE_SCALE)?),
            None => {
                let mut q = json!({ "item": item, "company": id_of(&company)? });
                if let Some(d) = &input.as_of {
                    q["as_of"] = json!(d);
                }
                match plugins::call::<Value>("erp_item", "valuation_for", &q) {
                    Ok(v) if v.get("valuation_method").and_then(Value::as_str) == Some("standard") => {
                        Some(dec(&v, "standard_cost", VALUE_SCALE)?)
                    }
                    _ => None,
                }
            }
        };
        match unit {
            Some(u) => {
                let cost = need_qty.times(u, VALUE_SCALE)?;
                material = material.checked_add(cost)?;
                lines.push(json!({ "item": item, "qty": need_qty, "unit_cost": u, "cost": cost }));
            }
            None => missing.push(item.clone()),
        }
    }
    // Operations: the top BOM once per output batch, each sub-assembly BOM per batch needed.
    let mut batches: Vec<(String, Decimal)> = vec![(input.bom.clone(), qty.div(qty_of(&bom, "qty")?, QTY_SCALE)?)];
    for n in &needs {
        if let Some(sub) = &n.sub_bom {
            let out = qty_of(&require("bom", sub, "BOM")?, "qty")?;
            batches.push((sub.clone(), n.qty.div(out, QTY_SCALE)?));
        }
    }
    let mut labour = Decimal::zero(VALUE_SCALE);
    for (bom_id, count) in batches {
        for op in crate::routing::bom_operations(&bom_id)? {
            let wc = require("work_center", text(&op, "work_center").unwrap_or_default(), "work center")?;
            let minutes = dec(&op, "setup_minutes", 2)?.checked_add(dec(&op, "cycle_minutes", 2)?.times(count, 2)?)?;
            labour = labour.checked_add(operation_cost(minutes, dec(&wc, "cost_per_hour", 4)?, VALUE_SCALE)?)?;
        }
    }
    let total = material.checked_add(labour)?;
    let unit = total.div(qty, VALUE_SCALE)?;
    Ok(json!({
        "bom": input.bom,
        "qty": qty,
        "material_cost": material,
        "labour_cost": labour,
        "total_cost": total,
        "unit_cost": unit,
        "materials": lines,
        "missing_cost": missing,
    }))
}

/// One child order per sub-assembly component of a draft or confirmed order, for the quantity it needs.
fn do_create_child_mos(input: Id) -> Result<Value> {
    require_user()?;
    let mo = require("mo", &input.id, "manufacturing order")?;
    if !matches!(text(&mo, "state"), Some("draft" | "confirmed")) {
        return Err(Error::msg("create child orders before the order is reserved"));
    }
    let existing: Vec<Record> = db::find("mo").filter("parent_mo", input.id.as_str()).limit(200).all()?;
    if !existing.is_empty() {
        return Err(Error::msg("child orders already exist for this order"));
    }
    let bom_id = text(&mo, "bom").unwrap_or_default();
    let bom = require("bom", bom_id, "BOM")?;
    let needs = needs_for(bom_id, qty_of(&mo, "qty")?)?;
    let _ = bom;
    let mut created = Vec::new();
    for n in needs.iter().filter(|n| n.level == 0 && n.sub_bom.is_some()) {
        let child: Record = plugins::call(
            "erp_manufacturing",
            "create_mo",
            &json!({
                "company": text(&mo, "company"),
                "bom": n.sub_bom,
                "qty": n.qty,
                "warehouse": text(&mo, "warehouse"),
                "planned_date": text(&mo, "planned_date"),
            }),
        )?;
        let child: Record = db::update::<Record>("mo", id_of(&child)?, &json!({ "parent_mo": input.id }))?
            .ok_or_else(|| Error::msg("the child order is gone"))?;
        created.push(child);
    }
    if created.is_empty() {
        return Err(Error::msg("this BOM has no sub-assembly components"));
    }
    Ok(json!({ "mo": input.id, "children": created }))
}

/// A parent order cannot complete while a child order is still open.
pub(crate) fn assert_children_done(mo_id: &str) -> Result<()> {
    let children: Vec<Record> = db::find("mo").filter("parent_mo", mo_id).limit(200).all()?;
    for child in &children {
        if !matches!(text(child, "state"), Some("done" | "cancelled")) {
            return Err(Error::msg(format!(
                "sub-assembly order {} is not finished",
                text(child, "number").unwrap_or("(draft)")
            )));
        }
    }
    Ok(())
}

handler! {
    fn explode_bom(input: Explode) -> Value { do_explode_bom(input) }
    fn rollup_cost(input: Rollup) -> Value { do_rollup_cost(input) }
    fn create_child_mos(input: Id) -> Value { do_create_child_mos(input) }
}
