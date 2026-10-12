//! Three-way match release status (pure).

use aether_sdk::decimal::Decimal;
use aether_sdk::Result;

use crate::common::{MONEY_SCALE, PRICE_SCALE, QTY_SCALE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    Yes,
    No,
    Exception,
}

impl Release {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Exception => "exception",
        }
    }
}

/// Line release from ordered / received / billed quantities and unit prices.
/// Paid-when-received policy without float compare:
/// * received policy (default): billed ≤ received → yes; received 0 and billed ≤ ordered → no; else exception
/// * ordered policy: billed ≤ ordered → yes (partial over-bill → exception)
pub fn line_release(
    ordered: Decimal,
    received: Decimal,
    billed: Decimal,
    po_price: Decimal,
    bill_price: Decimal,
    on_ordered: bool,
) -> Result<Release> {
    let ordered = ordered.with_scale(QTY_SCALE)?;
    let received = received.with_scale(QTY_SCALE)?;
    let billed = billed.with_scale(QTY_SCALE)?;
    let po_price = po_price.with_scale(PRICE_SCALE)?;
    let bill_price = bill_price.with_scale(PRICE_SCALE)?;
    if po_price != bill_price {
        return Ok(Release::Exception);
    }
    if on_ordered {
        if billed > ordered {
            return Ok(Release::Exception);
        }
        if billed.is_zero() {
            return Ok(Release::No);
        }
        return Ok(Release::Yes);
    }
    // on received quantities
    if billed <= received {
        return Ok(Release::Yes);
    }
    if received.is_zero() && billed <= ordered {
        return Ok(Release::No);
    }
    Ok(Release::Exception)
}

pub fn header_release(lines: &[Release]) -> Release {
    if lines.is_empty() {
        return Release::No;
    }
    if lines.iter().any(|r| *r == Release::Exception) {
        return Release::Exception;
    }
    if lines.iter().all(|r| *r == Release::Yes) {
        return Release::Yes;
    }
    if lines.iter().all(|r| *r == Release::No) {
        return Release::No;
    }
    Release::Exception
}

pub fn line_amount(qty: Decimal, unit_price: Decimal) -> Result<Decimal> {
    qty.with_scale(QTY_SCALE)?.times(unit_price.with_scale(PRICE_SCALE)?, MONEY_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: &str) -> Decimal {
        Decimal::parse(t).unwrap().with_scale(QTY_SCALE).unwrap()
    }
    fn p(t: &str) -> Decimal {
        Decimal::parse(t).unwrap().with_scale(PRICE_SCALE).unwrap()
    }

    #[test]
    fn received_policy_yes_when_billed_covered() {
        let r = line_release(q("10"), q("10"), q("8"), p("5"), p("5"), false).unwrap();
        assert_eq!(r, Release::Yes);
    }

    #[test]
    fn received_policy_no_when_nothing_received() {
        let r = line_release(q("10"), q("0"), q("5"), p("5"), p("5"), false).unwrap();
        assert_eq!(r, Release::No);
    }

    #[test]
    fn price_mismatch_is_exception() {
        let r = line_release(q("10"), q("10"), q("5"), p("5"), p("6"), false).unwrap();
        assert_eq!(r, Release::Exception);
    }

    #[test]
    fn header_exception_if_any_line_exception() {
        assert_eq!(header_release(&[Release::Yes, Release::Exception]), Release::Exception);
        assert_eq!(header_release(&[Release::Yes, Release::Yes]), Release::Yes);
    }

    #[test]
    fn amount_is_exact() {
        assert_eq!(line_amount(q("3"), p("2.5")).unwrap().to_string(), "7.50");
    }
}
