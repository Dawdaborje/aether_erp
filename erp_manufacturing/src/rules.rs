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
}
