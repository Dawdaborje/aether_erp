//! Valuation rules with nothing read from a database.
//!
//! Layers are the only place quantity and value live. A receipt opens a layer; an issue walks
//! open layers (FIFO), a pooled average layer, or standard cost. Insufficient stock is refused —
//! no negative stock, no repost job.

use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

pub const QTY_SCALE: u32 = 6;
pub const VALUE_SCALE: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Fifo,
    Average,
    Standard,
}

impl Method {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "fifo" => Some(Self::Fifo),
            "average" => Some(Self::Average),
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Average => "average",
            Self::Standard => "standard",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub id: String,
    pub seq: i64,
    pub remaining_qty: Decimal,
    pub remaining_value: Decimal,
    pub unit_cost: Decimal,
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    pub layer_id: String,
    pub qty: Decimal,
    pub value: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueResult {
    pub allocations: Vec<Allocation>,
    pub layers: Vec<Layer>,
    pub total_value: Decimal,
    pub unit_cost: Decimal,
}

fn q0() -> Decimal {
    Decimal::zero(QTY_SCALE)
}

fn v0() -> Decimal {
    Decimal::zero(VALUE_SCALE)
}

/// Open a layer from a receipt (or average pool top-up).
pub fn receive(qty: Decimal, unit_cost: Decimal, method: Method, id: impl Into<String>, seq: i64) -> Result<Layer> {
    let qty = qty.with_scale(QTY_SCALE)?;
    let unit_cost = unit_cost.with_scale(VALUE_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("receipt quantity must be greater than zero"));
    }
    if unit_cost.is_negative() {
        return Err(Error::msg("unit cost is not negative"));
    }
    let value = qty.times(unit_cost, VALUE_SCALE)?;
    Ok(Layer {
        id: id.into(),
        seq,
        remaining_qty: qty,
        remaining_value: value,
        unit_cost,
        method,
    })
}

/// Merge a receipt into a single pooled average layer (creating one if needed).
pub fn receive_average(pool: Option<Layer>, qty: Decimal, unit_cost: Decimal, new_id: impl Into<String>) -> Result<Layer> {
    let qty = qty.with_scale(QTY_SCALE)?;
    let unit_cost = unit_cost.with_scale(VALUE_SCALE)?;
    let add_value = qty.times(unit_cost, VALUE_SCALE)?;
    match pool {
        None => receive(qty, unit_cost, Method::Average, new_id, 1),
        Some(mut layer) => {
            if layer.method != Method::Average {
                return Err(Error::msg("average receipt needs an average layer"));
            }
            layer.remaining_qty = layer.remaining_qty.checked_add(qty)?;
            layer.remaining_value = layer.remaining_value.checked_add(add_value)?;
            if layer.remaining_qty.is_zero() {
                layer.unit_cost = v0();
            } else {
                layer.unit_cost = layer.remaining_value.div(layer.remaining_qty, VALUE_SCALE)?;
            }
            Ok(layer)
        }
    }
}

/// Issue `qty` against open layers. Refuses negative stock.
pub fn issue(qty: Decimal, method: Method, mut layers: Vec<Layer>) -> Result<IssueResult> {
    let qty = qty.with_scale(QTY_SCALE)?;
    if qty.is_zero() || qty.is_negative() {
        return Err(Error::msg("issue quantity must be greater than zero"));
    }
    layers.sort_by_key(|l| l.seq);
    let on_hand = layers.iter().try_fold(q0(), |acc, l| acc.checked_add(l.remaining_qty))?;
    if qty > on_hand {
        return Err(Error::msg(format!("not enough stock: need {qty}, have {on_hand}")));
    }

    match method {
        Method::Fifo | Method::Standard => issue_fifo(qty, layers),
        Method::Average => issue_average(qty, layers),
    }
}

fn issue_fifo(mut need: Decimal, mut layers: Vec<Layer>) -> Result<IssueResult> {
    let mut allocations = Vec::new();
    let mut total_value = v0();
    for layer in layers.iter_mut() {
        if need.is_zero() {
            break;
        }
        if layer.remaining_qty.is_zero() {
            continue;
        }
        let take = if need < layer.remaining_qty { need } else { layer.remaining_qty };
        let value = if take == layer.remaining_qty {
            layer.remaining_value
        } else {
            take.times(layer.unit_cost, VALUE_SCALE)?
        };
        layer.remaining_qty = layer.remaining_qty.checked_sub(take)?;
        layer.remaining_value = layer.remaining_value.checked_sub(value)?;
        if layer.remaining_qty.is_zero() {
            layer.remaining_value = v0();
        }
        allocations.push(Allocation {
            layer_id: layer.id.clone(),
            qty: take,
            value,
        });
        total_value = total_value.checked_add(value)?;
        need = need.checked_sub(take)?;
    }
    if !need.is_zero() {
        return Err(Error::msg("not enough stock"));
    }
    let unit_cost = if qty_sum(&allocations)?.is_zero() {
        v0()
    } else {
        total_value.div(qty_sum(&allocations)?, VALUE_SCALE)?
    };
    Ok(IssueResult {
        allocations,
        layers,
        total_value,
        unit_cost,
    })
}

fn issue_average(need: Decimal, mut layers: Vec<Layer>) -> Result<IssueResult> {
    // Average uses one pooled layer per (item, warehouse). receive_average maintains it.
    let open: Vec<_> = layers.iter().filter(|l| !l.remaining_qty.is_zero()).cloned().collect();
    if open.len() != 1 {
        return Err(Error::msg("average valuation needs one pooled layer per item and warehouse"));
    }
    let rate = open[0].remaining_value.div(open[0].remaining_qty, VALUE_SCALE)?;
    let total_value = need.times(rate, VALUE_SCALE)?;
    let layer = layers.iter_mut().find(|l| l.id == open[0].id).unwrap();
    layer.remaining_qty = layer.remaining_qty.checked_sub(need)?;
    layer.remaining_value = layer.remaining_value.checked_sub(total_value)?;
    if layer.remaining_qty.is_zero() {
        layer.remaining_value = v0();
        layer.unit_cost = v0();
    } else {
        layer.unit_cost = rate;
    }
    Ok(IssueResult {
        allocations: vec![Allocation {
            layer_id: layer.id.clone(),
            qty: need,
            value: total_value,
        }],
        layers,
        total_value,
        unit_cost: rate,
    })
}

fn qty_sum(allocs: &[Allocation]) -> Result<Decimal> {
    allocs.iter().try_fold(q0(), |acc, a| acc.checked_add(a.qty))
}

/// On-hand quantity and value from open layers.
pub fn on_hand(layers: &[Layer]) -> Result<(Decimal, Decimal)> {
    let qty = layers.iter().try_fold(q0(), |acc, l| acc.checked_add(l.remaining_qty))?;
    let value = layers.iter().try_fold(v0(), |acc, l| acc.checked_add(l.remaining_value))?;
    Ok((qty, value))
}

/// Price variance on a standard-cost receipt: (actual − standard) × qty.
pub fn standard_variance(qty: Decimal, actual_unit: Decimal, standard_unit: Decimal) -> Result<Decimal> {
    let qty = qty.with_scale(QTY_SCALE)?;
    let diff = actual_unit.with_scale(VALUE_SCALE)?.checked_sub(standard_unit.with_scale(VALUE_SCALE)?)?;
    qty.times(diff, VALUE_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::parse(text).unwrap().with_scale(QTY_SCALE).unwrap()
    }

    fn money(text: &str) -> Decimal {
        Decimal::parse(text).unwrap().with_scale(VALUE_SCALE).unwrap()
    }

    fn layer(id: &str, seq: i64, qty: &str, cost: &str, method: Method) -> Layer {
        receive(d(qty), money(cost), method, id, seq).unwrap()
    }

    #[test]
    fn fifo_walks_oldest_first() {
        let layers = vec![
            layer("a", 1, "10", "2", Method::Fifo),
            layer("b", 2, "10", "4", Method::Fifo),
        ];
        let out = issue(d("12"), Method::Fifo, layers).unwrap();
        assert_eq!(out.allocations.len(), 2);
        assert_eq!(out.allocations[0].layer_id, "a");
        assert_eq!(out.allocations[0].qty, d("10"));
        assert_eq!(out.allocations[0].value, money("20"));
        assert_eq!(out.allocations[1].qty, d("2"));
        assert_eq!(out.allocations[1].value, money("8"));
        assert_eq!(out.total_value, money("28"));
        let (q, v) = on_hand(&out.layers).unwrap();
        assert_eq!(q, d("8"));
        assert_eq!(v, money("32"));
    }

    #[test]
    fn fifo_refuses_negative_stock() {
        let layers = vec![layer("a", 1, "5", "1", Method::Fifo)];
        let err = issue(d("6"), Method::Fifo, layers).unwrap_err();
        assert!(format!("{err}").contains("not enough stock"));
    }

    #[test]
    fn average_pool_receipt_and_issue() {
        let pool = receive_average(None, d("10"), money("2"), "p").unwrap();
        let pool = receive_average(Some(pool), d("10"), money("4"), "p").unwrap();
        assert_eq!(pool.remaining_qty, d("20"));
        assert_eq!(pool.unit_cost, money("3"));
        let out = issue(d("5"), Method::Average, vec![pool]).unwrap();
        assert_eq!(out.unit_cost, money("3"));
        assert_eq!(out.total_value, money("15"));
        let (q, v) = on_hand(&out.layers).unwrap();
        assert_eq!(q, d("15"));
        assert_eq!(v, money("45"));
    }

    #[test]
    fn standard_issues_at_layer_cost() {
        let layers = vec![layer("a", 1, "10", "5", Method::Standard)];
        let out = issue(d("4"), Method::Standard, layers).unwrap();
        assert_eq!(out.total_value, money("20"));
        assert_eq!(standard_variance(d("10"), money("6"), money("5")).unwrap(), money("10"));
    }

    #[test]
    fn receipt_rejects_zero_qty() {
        assert!(receive(d("0"), money("1"), Method::Fifo, "x", 1).is_err());
    }
}
