//! BOM explosion (pure).

use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

use crate::common::{PCT_SCALE, QTY_SCALE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BomComponent {
    pub item_id: String,
    pub qty_per: Decimal,
    pub scrap_pct: Decimal,
    pub is_byproduct: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExplodedLine {
    pub item_id: String,
    pub qty: Decimal,
    pub is_byproduct: bool,
}

/// Quantity to consume (or byproduct to produce) for `mo_qty` of finished goods.
/// Material: mo_qty / bom_output_qty * qty_per * (1 + scrap_pct/100).
pub fn explode(bom_output_qty: Decimal, mo_qty: Decimal, lines: &[BomComponent]) -> Result<Vec<ExplodedLine>> {
    let bom_output_qty = bom_output_qty.with_scale(QTY_SCALE)?;
    let mo_qty = mo_qty.with_scale(QTY_SCALE)?;
    if bom_output_qty.is_zero() || bom_output_qty.is_negative() {
        return Err(Error::msg("BOM output quantity must be greater than zero"));
    }
    if mo_qty.is_zero() || mo_qty.is_negative() {
        return Err(Error::msg("manufacturing quantity must be greater than zero"));
    }
    let mut out = Vec::new();
    for line in lines {
        let qty_per = line.qty_per.with_scale(QTY_SCALE)?;
        if qty_per.is_zero() || qty_per.is_negative() {
            return Err(Error::msg("BOM line quantity must be greater than zero"));
        }
        let scrap = line.scrap_pct.with_scale(PCT_SCALE)?;
        if scrap.is_negative() {
            return Err(Error::msg("scrap percent is not negative"));
        }
        // base = mo_qty * qty_per / bom_output
        let base = mo_qty.times(qty_per, QTY_SCALE)?.div(bom_output_qty, QTY_SCALE)?;
        let qty = if scrap.is_zero() || line.is_byproduct {
            base
        } else {
            // base * (100 + scrap) / 100
            let hundred = Decimal::parse("100")?.with_scale(PCT_SCALE)?;
            let factor = hundred.checked_add(scrap)?;
            base.times(factor, QTY_SCALE)?.div(hundred.with_scale(QTY_SCALE)?, QTY_SCALE)?
        };
        out.push(ExplodedLine {
            item_id: line.item_id.clone(),
            qty,
            is_byproduct: line.is_byproduct,
        });
    }
    Ok(out)
}

/// Minutes an operation is planned for: `setup + cycle × (mo_qty / bom_output_qty)`.
pub fn planned_minutes(setup: Decimal, cycle: Decimal, mo_qty: Decimal, bom_output_qty: Decimal) -> Result<Decimal> {
    if bom_output_qty.is_zero() || bom_output_qty.is_negative() {
        return Err(Error::msg("BOM output quantity must be greater than zero"));
    }
    let batches = mo_qty.with_scale(QTY_SCALE)?.div(bom_output_qty.with_scale(QTY_SCALE)?, QTY_SCALE)?;
    let run = cycle.with_scale(2)?.times(batches, 2)?;
    setup.with_scale(2)?.checked_add(run)
}

/// Cost of `minutes` at `rate_per_hour`.
pub fn operation_cost(minutes: Decimal, rate_per_hour: Decimal, scale: u32) -> Result<Decimal> {
    let sixty = Decimal::parse("60")?;
    minutes.with_scale(2)?.times(rate_per_hour.with_scale(4)?, scale + 4)?.div(sixty, scale)
}

#[derive(Debug, Clone)]
pub struct NodeLine {
    pub item_id: String,
    pub qty_per: Decimal,
    pub scrap_pct: Decimal,
    pub is_byproduct: bool,
    pub sub_bom: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BomNode {
    pub output_qty: Decimal,
    pub lines: Vec<NodeLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Need {
    pub item_id: String,
    pub qty: Decimal,
    /// `0` for a direct component of the top BOM.
    pub level: u32,
    /// The BOM this component is made from, when it is a sub-assembly.
    pub sub_bom: Option<String>,
}

pub const MAX_LEVELS: u32 = 8;

/// Walk a multi-level BOM. Every component is reported at its level (sub-assemblies included, with the
/// quantity needed of them); components of a sub-assembly follow, scaled to that quantity. Byproducts are
/// ignored below the top level. A BOM that contains itself is refused.
pub fn explode_tree(
    root: &str,
    qty: Decimal,
    fetch: &mut dyn FnMut(&str) -> Result<BomNode>,
) -> Result<Vec<Need>> {
    fn walk(
        bom: &str,
        qty: Decimal,
        level: u32,
        path: &mut Vec<String>,
        fetch: &mut dyn FnMut(&str) -> Result<BomNode>,
        out: &mut Vec<Need>,
    ) -> Result<()> {
        if level >= MAX_LEVELS {
            return Err(Error::msg("the BOM is nested too deeply"));
        }
        if path.iter().any(|p| p == bom) {
            return Err(Error::msg("a BOM cannot contain itself"));
        }
        path.push(bom.to_string());
        let node = fetch(bom)?;
        let components: Vec<BomComponent> = node
            .lines
            .iter()
            .map(|l| BomComponent {
                item_id: l.item_id.clone(),
                qty_per: l.qty_per,
                scrap_pct: l.scrap_pct,
                is_byproduct: l.is_byproduct,
            })
            .collect();
        let exploded = explode(node.output_qty, qty, &components)?;
        for (line, need) in node.lines.iter().zip(exploded) {
            if need.is_byproduct && level > 0 {
                continue;
            }
            out.push(Need {
                item_id: need.item_id.clone(),
                qty: need.qty,
                level,
                sub_bom: line.sub_bom.clone(),
            });
            if let (Some(sub), false) = (&line.sub_bom, need.is_byproduct) {
                walk(sub, need.qty, level + 1, path, fetch, out)?;
            }
        }
        path.pop();
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, qty, 0, &mut Vec::new(), fetch, &mut out)?;
    Ok(out)
}

/// Raw materials only (components with no sub-BOM), summed by item.
pub fn raw_materials(needs: &[Need]) -> Result<Vec<(String, Decimal)>> {
    let mut order: Vec<String> = Vec::new();
    let mut sums: std::collections::HashMap<String, Decimal> = std::collections::HashMap::new();
    for n in needs.iter().filter(|n| n.sub_bom.is_none()) {
        match sums.get_mut(&n.item_id) {
            Some(total) => *total = total.checked_add(n.qty)?,
            None => {
                order.push(n.item_id.clone());
                sums.insert(n.item_id.clone(), n.qty);
            }
        }
    }
    Ok(order.into_iter().map(|i| { let q = sums[&i]; (i, q) }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: &str) -> Decimal {
        Decimal::parse(t).unwrap().with_scale(QTY_SCALE).unwrap()
    }
    fn p(t: &str) -> Decimal {
        Decimal::parse(t).unwrap().with_scale(PCT_SCALE).unwrap()
    }

    #[test]
    fn doubles_materials_when_making_two() {
        let lines = vec![BomComponent {
            item_id: "wood".into(),
            qty_per: q("2"),
            scrap_pct: p("0"),
            is_byproduct: false,
        }];
        let out = explode(q("1"), q("2"), &lines).unwrap();
        assert_eq!(out[0].qty, q("4"));
    }

    #[test]
    fn scrap_adds_percent() {
        let lines = vec![BomComponent {
            item_id: "steel".into(),
            qty_per: q("10"),
            scrap_pct: p("10"),
            is_byproduct: false,
        }];
        let out = explode(q("1"), q("1"), &lines).unwrap();
        assert_eq!(out[0].qty, q("11"));
    }

    #[test]
    fn byproduct_ignores_scrap() {
        let lines = vec![BomComponent {
            item_id: "scrapmetal".into(),
            qty_per: q("1"),
            scrap_pct: p("50"),
            is_byproduct: true,
        }];
        let out = explode(q("1"), q("2"), &lines).unwrap();
        assert_eq!(out[0].qty, q("2"));
        assert!(out[0].is_byproduct);
    }

    fn d(t: &str) -> Decimal {
        Decimal::parse(t).unwrap()
    }

    #[test]
    fn planned_minutes_scale_with_batches() {
        // setup 10, 5 min per batch of 2, order of 6 => 3 batches => 25
        let m = planned_minutes(d("10"), d("5"), d("6"), d("2")).unwrap();
        assert_eq!(m, d("25"));
    }

    #[test]
    fn operation_cost_per_hour() {
        let c = operation_cost(d("90"), d("20"), 6).unwrap();
        assert_eq!(c, d("30"));
    }

    fn line(item: &str, qty: &str, sub: Option<&str>) -> NodeLine {
        NodeLine { item_id: item.into(), qty_per: d(qty), scrap_pct: d("0"), is_byproduct: false, sub_bom: sub.map(str::to_string) }
    }

    #[test]
    fn multi_level_scales_sub_assemblies() {
        // bike (1) = 2 wheel + 1 frame ; wheel (1) = 36 spoke + 1 rim
        let mut fetch = |id: &str| -> Result<BomNode> {
            Ok(match id {
                "bike" => BomNode { output_qty: d("1"), lines: vec![line("wheel", "2", Some("wheel_bom")), line("frame", "1", None)] },
                "wheel_bom" => BomNode { output_qty: d("1"), lines: vec![line("spoke", "36", None), line("rim", "1", None)] },
                _ => return Err(Error::msg("unknown bom")),
            })
        };
        let needs = explode_tree("bike", d("3"), &mut fetch).unwrap();
        let raw = raw_materials(&needs).unwrap();
        let get = |i: &str| raw.iter().find(|(n, _)| n == i).map(|(_, q)| *q).unwrap();
        assert_eq!(get("spoke"), d("216"));
        assert_eq!(get("rim"), d("6"));
        assert_eq!(get("frame"), d("3"));
        assert!(needs.iter().any(|n| n.item_id == "wheel" && n.qty == d("6") && n.level == 0));
    }

    #[test]
    fn self_containing_bom_refused() {
        let mut fetch = |_: &str| -> Result<BomNode> {
            Ok(BomNode { output_qty: d("1"), lines: vec![line("x", "1", Some("loop"))] })
        };
        assert!(explode_tree("loop", d("1"), &mut fetch).is_err());
    }
}
