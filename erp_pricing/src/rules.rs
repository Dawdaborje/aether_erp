//! Pure pricing arithmetic: line amounts, taxes, quantity breaks and due dates.

use aether_sdk::dates::{Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::Result;

use crate::common::{MONEY_SCALE, PCT_SCALE, PRICE_SCALE, QTY_SCALE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxKind {
    Percent,
    Fixed,
}

#[derive(Debug, Clone, Copy)]
pub struct TaxSpec {
    pub kind: TaxKind,
    pub rate: Decimal,
    pub included: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineAmounts {
    pub gross: Decimal,
    pub discount: Decimal,
    /// Taxable base: the amount before tax.
    pub net: Decimal,
    pub tax: Decimal,
    /// What the party pays: `net + tax`.
    pub total: Decimal,
    /// Tax-exclusive price per unit after discount (the cost a receipt should carry).
    pub net_unit: Decimal,
}

fn hundred() -> Decimal {
    Decimal::whole(100, 0).expect("100 fits")
}

/// Amount of a line: `qty * unit_price`, less the discount percent, plus (or split out of) the tax.
pub fn compute_line(qty: Decimal, unit_price: Decimal, discount_pct: Decimal, tax: Option<TaxSpec>) -> Result<LineAmounts> {
    let qty = qty.with_scale(QTY_SCALE)?;
    let unit_price = unit_price.with_scale(PRICE_SCALE)?;
    let discount_pct = discount_pct.with_scale(PCT_SCALE)?;
    if discount_pct.is_negative() || discount_pct > hundred().with_scale(PCT_SCALE)? {
        return Err(aether_sdk::Error::msg("discount must be between 0 and 100 percent"));
    }
    let gross = qty.times(unit_price, MONEY_SCALE)?;
    let discount = gross.times(discount_pct, PRICE_SCALE)?.div(hundred(), MONEY_SCALE)?;
    let after_discount = gross.checked_sub(discount)?;
    let unit_after_discount = unit_price
        .times(hundred().checked_sub(discount_pct)?, PRICE_SCALE + 2)?
        .div(hundred(), PRICE_SCALE)?;
    let zero = Decimal::zero(MONEY_SCALE);
    let Some(tax) = tax else {
        return Ok(LineAmounts { gross, discount, net: after_discount, tax: zero, total: after_discount, net_unit: unit_after_discount });
    };
    match (tax.kind, tax.included) {
        (TaxKind::Percent, false) => {
            let amount = after_discount.times(tax.rate.with_scale(PCT_SCALE)?, PRICE_SCALE)?.div(hundred(), MONEY_SCALE)?;
            Ok(LineAmounts {
                gross,
                discount,
                net: after_discount,
                tax: amount,
                total: after_discount.checked_add(amount)?,
                net_unit: unit_after_discount,
            })
        }
        (TaxKind::Percent, true) => {
            let divisor = hundred().checked_add(tax.rate.with_scale(PCT_SCALE)?)?;
            let net = after_discount.times(hundred(), PRICE_SCALE)?.div(divisor, MONEY_SCALE)?;
            let amount = after_discount.checked_sub(net)?;
            let net_unit = unit_after_discount.times(hundred(), PRICE_SCALE + 2)?.div(divisor, PRICE_SCALE)?;
            Ok(LineAmounts { gross, discount, net, tax: amount, total: after_discount, net_unit })
        }
        (TaxKind::Fixed, _) => {
            let amount = qty.times(tax.rate.with_scale(PRICE_SCALE)?, MONEY_SCALE)?;
            Ok(LineAmounts {
                gross,
                discount,
                net: after_discount,
                tax: amount,
                total: after_discount.checked_add(amount)?,
                net_unit: unit_after_discount,
            })
        }
    }
}

/// One tax of a group, in the order the group applies them.
#[derive(Debug, Clone, Copy)]
pub struct Component {
    pub spec: TaxSpec,
    /// Applies to the price plus the taxes before it.
    pub compound: bool,
    /// Taken off what is paid (negative on the document) instead of added.
    pub withholding: bool,
}

/// A line priced with several taxes: the usual amounts, where `tax` is the signed sum, plus each
/// component's signed amount in the group's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiAmounts {
    pub line: LineAmounts,
    pub amounts: Vec<Decimal>,
}

/// Price a line with an ordered list of taxes.
///
/// * **Exclusive** (the default): each percent tax is worked out on the price after discount; a
///   `compound` one on that price plus the (non-withheld) taxes before it. A fixed tax is `qty × rate`.
/// * **Inclusive**: every percent tax must be inclusive and none compound; the price holds all of
///   them, split out in proportion to their rates. Fixed taxes are added on top. A withholding
///   cannot be inclusive.
/// * A withholding amount is negative, so `total = net + tax` still holds.
///
/// One ordinary tax gives exactly [`compute_line`]'s answer.
pub fn compute_line_multi(qty: Decimal, unit_price: Decimal, discount_pct: Decimal, taxes: &[Component]) -> Result<MultiAmounts> {
    if let [only] = taxes {
        if !only.withholding {
            let line = compute_line(qty, unit_price, discount_pct, Some(only.spec))?;
            let amounts = vec![line.tax];
            return Ok(MultiAmounts { line, amounts });
        }
    }
    let zero_tax = compute_line(qty, unit_price, discount_pct, None)?;
    let qty = qty.with_scale(QTY_SCALE)?;
    let after_discount = zero_tax.net;
    let zero = Decimal::zero(MONEY_SCALE);
    let any_included = taxes.iter().any(|t| t.spec.included && t.spec.kind == TaxKind::Percent);
    let any_excluded_percent = taxes.iter().any(|t| !t.spec.included && t.spec.kind == TaxKind::Percent);
    if any_included && any_excluded_percent {
        return Err(aether_sdk::Error::msg("a group cannot mix taxes included in the price with taxes added on top"));
    }
    let mut amounts = vec![zero; taxes.len()];
    if any_included {
        if taxes.iter().any(|t| t.compound || t.withholding) {
            return Err(aether_sdk::Error::msg("included taxes cannot be compound or withheld"));
        }
        let rate_sum = taxes
            .iter()
            .filter(|t| t.spec.kind == TaxKind::Percent)
            .try_fold(Decimal::zero(PCT_SCALE), |acc, t| acc.checked_add(t.spec.rate.with_scale(PCT_SCALE)?))?;
        let divisor = hundred().checked_add(rate_sum)?;
        let net = after_discount.times(hundred(), PRICE_SCALE)?.div(divisor, MONEY_SCALE)?;
        let included_total = after_discount.checked_sub(net)?;
        // Each tax takes its share of the part of the price that is tax; the last takes the rounding.
        let mut given = zero;
        let last = taxes.iter().rposition(|t| t.spec.kind == TaxKind::Percent);
        for (i, t) in taxes.iter().enumerate() {
            if t.spec.kind != TaxKind::Percent {
                continue;
            }
            amounts[i] = if Some(i) == last {
                included_total.checked_sub(given)?
            } else {
                included_total.times(t.spec.rate.with_scale(PCT_SCALE)?, PRICE_SCALE)?.div(rate_sum, MONEY_SCALE)?
            };
            given = given.checked_add(amounts[i])?;
        }
        let mut total = after_discount;
        for (i, t) in taxes.iter().enumerate() {
            if t.spec.kind == TaxKind::Fixed {
                amounts[i] = qty.times(t.spec.rate.with_scale(PRICE_SCALE)?, MONEY_SCALE)?;
                total = total.checked_add(amounts[i])?;
            }
        }
        let tax = amounts.iter().try_fold(zero, |a, x| a.checked_add(*x))?;
        let net_unit = zero_tax.net_unit.times(hundred(), PRICE_SCALE + 2)?.div(divisor, PRICE_SCALE)?;
        return Ok(MultiAmounts {
            line: LineAmounts { gross: zero_tax.gross, discount: zero_tax.discount, net, tax, total, net_unit },
            amounts,
        });
    }
    // Exclusive: in order, each on the price (or the price plus the earlier added taxes if compound).
    let mut added = zero;
    for (i, t) in taxes.iter().enumerate() {
        let amount = match t.spec.kind {
            TaxKind::Fixed => qty.times(t.spec.rate.with_scale(PRICE_SCALE)?, MONEY_SCALE)?,
            TaxKind::Percent => {
                let base = if t.compound { after_discount.checked_add(added)? } else { after_discount };
                base.times(t.spec.rate.with_scale(PCT_SCALE)?, PRICE_SCALE)?.div(hundred(), MONEY_SCALE)?
            }
        };
        amounts[i] = if t.withholding { zero.checked_sub(amount)? } else { amount };
        if !t.withholding {
            added = added.checked_add(amount)?;
        }
    }
    let tax = amounts.iter().try_fold(zero, |a, x| a.checked_add(*x))?;
    Ok(MultiAmounts {
        line: LineAmounts {
            gross: zero_tax.gross,
            discount: zero_tax.discount,
            net: after_discount,
            tax,
            total: after_discount.checked_add(tax)?,
            net_unit: zero_tax.net_unit,
        },
        amounts,
    })
}

#[derive(Debug, Clone)]
pub struct PriceRow {
    pub id: String,
    pub min_qty: Decimal,
    pub unit_price: Decimal,
    pub discount_pct: Decimal,
    pub valid_from: Option<NaiveDate>,
    pub valid_to: Option<NaiveDate>,
    /// A price tied to one party beats a general one at the same break.
    pub party_specific: bool,
}

/// The best quantity break for `qty` on `on`: the highest `min_qty` not above `qty`, party price first,
/// then the most recent `valid_from`.
pub fn pick_price(rows: &[PriceRow], qty: Decimal, on: NaiveDate) -> Option<&PriceRow> {
    rows.iter()
        .filter(|r| r.min_qty <= qty)
        .filter(|r| r.valid_from.map_or(true, |d| d <= on) && r.valid_to.map_or(true, |d| d >= on))
        .max_by(|a, b| {
            a.min_qty
                .cmp(&b.min_qty)
                .then(a.party_specific.cmp(&b.party_specific))
                .then(a.valid_from.cmp(&b.valid_from))
        })
}

pub fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
    date + Duration::days(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(t: &str) -> Decimal {
        Decimal::parse(t).unwrap()
    }
    fn date(t: &str) -> NaiveDate {
        aether_sdk::dates::parse_date(t).unwrap()
    }
    fn pct(rate: &str, included: bool) -> TaxSpec {
        TaxSpec { kind: TaxKind::Percent, rate: d(rate), included }
    }

    #[test]
    fn exclusive_percent_adds_tax() {
        let a = compute_line(d("3"), d("10"), d("0"), Some(pct("18", false))).unwrap();
        assert_eq!((a.net.to_string(), a.tax.to_string(), a.total.to_string()), ("30.00".into(), "5.40".into(), "35.40".into()));
    }

    #[test]
    fn inclusive_percent_splits_tax_out() {
        let a = compute_line(d("1"), d("118"), d("0"), Some(pct("18", true))).unwrap();
        assert_eq!((a.net.to_string(), a.tax.to_string(), a.total.to_string()), ("100.00".into(), "18.00".into(), "118.00".into()));
    }

    #[test]
    fn discount_applies_before_tax() {
        let a = compute_line(d("2"), d("50"), d("10"), Some(pct("10", false))).unwrap();
        assert_eq!(a.gross.to_string(), "100.00");
        assert_eq!(a.discount.to_string(), "10.00");
        assert_eq!(a.net.to_string(), "90.00");
        assert_eq!(a.tax.to_string(), "9.00");
        assert_eq!(a.total.to_string(), "99.00");
    }

    #[test]
    fn fixed_tax_is_per_unit() {
        let t = TaxSpec { kind: TaxKind::Fixed, rate: d("0.5"), included: false };
        let a = compute_line(d("4"), d("10"), d("0"), Some(t)).unwrap();
        assert_eq!(a.tax.to_string(), "2.00");
        assert_eq!(a.total.to_string(), "42.00");
    }

    #[test]
    fn net_unit_is_tax_exclusive_after_discount() {
        let a = compute_line(d("2"), d("118"), d("10"), Some(pct("18", true))).unwrap();
        assert_eq!(a.net_unit.to_string(), "90.000000");
        let b = compute_line(d("2"), d("50"), d("10"), Some(pct("10", false))).unwrap();
        assert_eq!(b.net_unit.to_string(), "45.000000");
    }

    #[test]
    fn no_tax_is_net() {
        let a = compute_line(d("2"), d("2.5"), d("0"), None).unwrap();
        assert_eq!((a.net.to_string(), a.tax.to_string()), ("5.00".into(), "0.00".into()));
    }

    #[test]
    fn discount_over_100_refused() {
        assert!(compute_line(d("1"), d("1"), d("101"), None).is_err());
    }

    fn row(id: &str, min: &str, price: &str, from: Option<&str>, to: Option<&str>, party: bool) -> PriceRow {
        PriceRow {
            id: id.into(),
            min_qty: d(min),
            unit_price: d(price),
            discount_pct: d("0"),
            valid_from: from.map(date),
            valid_to: to.map(date),
            party_specific: party,
        }
    }

    fn comp(rate: &str, included: bool, compound: bool, withholding: bool) -> Component {
        Component { spec: pct(rate, included), compound, withholding }
    }

    #[test]
    fn one_tax_is_what_the_single_tax_path_gives() {
        let single = compute_line(d("3"), d("10"), d("0"), Some(pct("18", false))).unwrap();
        let multi = compute_line_multi(d("3"), d("10"), d("0"), &[comp("18", false, false, false)]).unwrap();
        assert_eq!(multi.line, single);
        assert_eq!(multi.amounts, vec![single.tax]);
    }

    #[test]
    fn two_taxes_on_the_same_price_add_up() {
        // 7% and 3% of 100.
        let m = compute_line_multi(d("1"), d("100"), d("0"), &[comp("7", false, false, false), comp("3", false, false, false)]).unwrap();
        assert_eq!(m.amounts.iter().map(|a| a.to_string()).collect::<Vec<_>>(), vec!["7.00", "3.00"]);
        assert_eq!((m.line.net.to_string(), m.line.tax.to_string(), m.line.total.to_string()), ("100.00".into(), "10.00".into(), "110.00".into()));
    }

    #[test]
    fn a_compound_tax_applies_to_price_plus_earlier_taxes() {
        // 10% then 5% compound: 10 + 5% of 110 = 5.50.
        let m = compute_line_multi(d("1"), d("100"), d("0"), &[comp("10", false, false, false), comp("5", false, true, false)]).unwrap();
        assert_eq!(m.amounts.iter().map(|a| a.to_string()).collect::<Vec<_>>(), vec!["10.00", "5.50"]);
        assert_eq!(m.line.total.to_string(), "115.50");
    }

    #[test]
    fn withholding_is_negative_and_total_still_adds_up() {
        // 18% VAT added, 5% withheld: pay 100 + 18 - 5 = 113.
        let m = compute_line_multi(d("1"), d("100"), d("0"), &[comp("18", false, false, false), comp("5", false, false, true)]).unwrap();
        assert_eq!(m.amounts.iter().map(|a| a.to_string()).collect::<Vec<_>>(), vec!["18.00", "-5.00"]);
        assert_eq!((m.line.tax.to_string(), m.line.total.to_string()), ("13.00".into(), "113.00".into()));
    }

    #[test]
    fn a_withholding_alone_reduces_the_total() {
        let m = compute_line_multi(d("1"), d("100"), d("0"), &[comp("10", false, false, true)]).unwrap();
        assert_eq!((m.line.tax.to_string(), m.line.total.to_string()), ("-10.00".into(), "90.00".into()));
    }

    #[test]
    fn included_taxes_are_split_out_in_proportion() {
        // 118 holds 100 + 12% + 6%: the 18 of tax is split 12:6.
        let m = compute_line_multi(d("1"), d("118"), d("0"), &[comp("12", true, false, false), comp("6", true, false, false)]).unwrap();
        assert_eq!(m.line.net.to_string(), "100.00");
        assert_eq!(m.amounts.iter().map(|a| a.to_string()).collect::<Vec<_>>(), vec!["12.00", "6.00"]);
        assert_eq!(m.line.total.to_string(), "118.00");
        assert_eq!(m.amounts.iter().fold(Decimal::zero(2), |a, x| a + *x).to_string(), "18.00");
    }

    #[test]
    fn mixing_included_with_added_or_compounding_included_is_refused() {
        assert!(compute_line_multi(d("1"), d("100"), d("0"), &[comp("10", true, false, false), comp("5", false, false, false)]).is_err());
        assert!(compute_line_multi(d("1"), d("100"), d("0"), &[comp("10", true, false, false), comp("5", true, true, false)]).is_err());
        assert!(compute_line_multi(d("1"), d("100"), d("0"), &[comp("10", true, false, true)]).is_err());
    }

    #[test]
    fn a_fixed_tax_is_per_unit_and_discount_applies_first() {
        let fixed = Component { spec: TaxSpec { kind: TaxKind::Fixed, rate: d("2"), included: false }, compound: false, withholding: false };
        let m = compute_line_multi(d("3"), d("50"), d("10"), &[comp("10", false, false, false), fixed]).unwrap();
        // 150 - 15 = 135; 10% = 13.50; fixed 3 x 2 = 6.
        assert_eq!(m.amounts.iter().map(|a| a.to_string()).collect::<Vec<_>>(), vec!["13.50", "6.00"]);
        assert_eq!(m.line.total.to_string(), "154.50");
    }

    #[test]
    fn quantity_break_picks_highest_not_above() {
        let rows = vec![row("a", "0", "10", None, None, false), row("b", "10", "9", None, None, false), row("c", "100", "8", None, None, false)];
        assert_eq!(pick_price(&rows, d("12"), date("2026-01-01")).unwrap().id, "b");
        assert_eq!(pick_price(&rows, d("5"), date("2026-01-01")).unwrap().id, "a");
    }

    #[test]
    fn validity_window_and_party_priority() {
        let rows = vec![
            row("old", "0", "10", None, Some("2025-12-31"), false),
            row("gen", "0", "9", Some("2026-01-01"), None, false),
            row("vip", "0", "8", Some("2026-01-01"), None, true),
        ];
        assert_eq!(pick_price(&rows, d("1"), date("2026-06-01")).unwrap().id, "vip");
        assert_eq!(pick_price(&rows[..1], d("1"), date("2026-06-01")).map(|r| r.id.clone()), None);
    }

    #[test]
    fn due_date_adds_days() {
        assert_eq!(add_days(date("2026-01-30"), 30).to_string(), "2026-03-01");
    }
}
