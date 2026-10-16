//! Pure replenishment arithmetic.

use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

use crate::common::QTY_SCALE;

/// Quantity to order for a min/max rule, or `None` when no order is needed.
///
/// `projected = available + incoming`. Below `min` the order brings it up to `max` (at least `min` when `max`
/// is lower), rounded **up** to a whole `multiple`.
pub fn reorder_qty(available: Decimal, incoming: Decimal, min: Decimal, max: Decimal, multiple: Decimal) -> Result<Option<Decimal>> {
    let zero = Decimal::zero(QTY_SCALE);
    let min = min.with_scale(QTY_SCALE)?;
    let top = if max < min { min } else { max.with_scale(QTY_SCALE)? };
    let projected = available.with_scale(QTY_SCALE)?.checked_add(incoming.with_scale(QTY_SCALE)?)?;
    if projected >= min {
        return Ok(None);
    }
    let mut need = top.checked_sub(projected)?;
    if need <= zero {
        return Ok(None);
    }
    let multiple = multiple.with_scale(QTY_SCALE)?;
    if multiple.is_negative() {
        return Err(Error::msg("the order multiple must not be negative"));
    }
    if !multiple.is_zero() {
        // ceil(need / multiple) * multiple
        let steps = need.div(multiple, 0)?;
        let mut covered = steps.with_scale(QTY_SCALE)?.times(multiple, QTY_SCALE)?;
        if covered < need {
            covered = covered.checked_add(multiple)?;
        }
        need = covered;
    }
    Ok(Some(need))
}

/// Part of `ordered` that free stock cannot cover.
pub fn shortfall(ordered: Decimal, available: Decimal) -> Result<Decimal> {
    let zero = Decimal::zero(QTY_SCALE);
    let missing = ordered.with_scale(QTY_SCALE)?.checked_sub(available.with_scale(QTY_SCALE)?)?;
    Ok(if missing < zero { zero } else { missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(t: &str) -> Decimal {
        Decimal::parse(t).unwrap()
    }

    #[test]
    fn no_order_at_or_above_min() {
        assert_eq!(reorder_qty(d("10"), d("0"), d("10"), d("50"), d("1")).unwrap(), None);
    }

    #[test]
    fn orders_up_to_max() {
        let q = reorder_qty(d("4"), d("0"), d("10"), d("50"), d("1")).unwrap().unwrap();
        assert_eq!(q, d("46").with_scale(6).unwrap());
    }

    #[test]
    fn incoming_counts_toward_projection() {
        assert_eq!(reorder_qty(d("4"), d("6"), d("10"), d("50"), d("1")).unwrap(), None);
    }

    #[test]
    fn rounds_up_to_multiple() {
        let q = reorder_qty(d("4"), d("0"), d("10"), d("50"), d("12")).unwrap().unwrap();
        assert_eq!(q, d("48").with_scale(6).unwrap());
    }

    #[test]
    fn max_below_min_orders_to_min() {
        let q = reorder_qty(d("1"), d("0"), d("10"), d("5"), d("1")).unwrap().unwrap();
        assert_eq!(q, d("9").with_scale(6).unwrap());
    }

    #[test]
    fn shortfall_never_negative() {
        assert_eq!(shortfall(d("5"), d("8")).unwrap(), d("0").with_scale(6).unwrap());
        assert_eq!(shortfall(d("5"), d("2")).unwrap(), d("3").with_scale(6).unwrap());
    }
}
