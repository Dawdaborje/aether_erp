//! List sources and the sales dashboard.

use std::collections::BTreeMap;

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{money_of, qty_of, require, require_reader, text, Record, MONEY_SCALE};

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

fn list(model: &str, f: &Filter, search: &[&str], newest: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>(model).limit(500);
    if let Some(s) = f.state.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("state", s);
    }
    let mut rows = q.all()?;
    if let Some(k) = f.kind.as_deref().filter(|k| !k.is_empty()) {
        // Invoices written before `kind` existed are plain invoices.
        rows.retain(|r| text(r, "kind").unwrap_or("invoice") == k);
    }
    if let Some(needle) = f.q.as_deref().map(str::to_lowercase).filter(|n| !n.is_empty()) {
        rows.retain(|r| search.iter().any(|f| text(r, f).map_or(false, |v| v.to_lowercase().contains(&needle))));
    }
    rows.sort_by(|a, b| text(b, newest).cmp(&text(a, newest)));
    Ok(rows)
}

fn name_of(id: &str) -> String {
    require("party.party", id, "party")
        .ok()
        .and_then(|r| text(&r, "display_name").or_else(|| text(&r, "name")).map(str::to_string))
        .unwrap_or_else(|| id.to_string())
}

fn do_dashboard(_: Filter) -> Result<Value> {
    require_reader()?;
    let orders: Vec<Record> = db::find("so").limit(2000).all()?;
    let mut open = 0u64;
    let mut open_value = Decimal::zero(MONEY_SCALE);
    let mut uninvoiced = Decimal::zero(MONEY_SCALE);
    let mut by_customer: BTreeMap<String, Decimal> = BTreeMap::new();
    for so in &orders {
        let state = text(so, "state");
        if matches!(state, Some("confirmed" | "partially_delivered")) {
            open += 1;
            open_value = open_value.checked_add(money_of(so, "total")?)?;
        }
        if matches!(state, Some("confirmed" | "partially_delivered" | "delivered")) {
            let e = by_customer
                .entry(text(so, "customer").unwrap_or_default().to_string())
                .or_insert_with(|| Decimal::zero(MONEY_SCALE));
            *e = e.checked_add(money_of(so, "total")?)?;
            for line in db::find::<Record>("so_line").filter("so", text(so, "id").unwrap_or_default()).limit(500).all()? {
                let qty = qty_of(&line, "qty")?;
                let left = qty.checked_sub(qty_of(&line, "qty_invoiced")?)?;
                if qty.is_zero() || left.is_zero() || left.is_negative() {
                    continue;
                }
                let value = money_of(&line, "amount")?.checked_add(money_of(&line, "tax_amount")?)?;
                uninvoiced = uninvoiced.checked_add(value.times(left, 6)?.div(qty, MONEY_SCALE)?)?;
            }
        }
    }
    let quotes: Vec<Record> = db::find("quotation").limit(2000).all()?;
    let open_quotes = quotes.iter().filter(|q| matches!(text(q, "state"), Some("sent" | "accepted"))).count();
    let converted = quotes.iter().filter(|q| text(q, "state") == Some("converted")).count();
    let decided = quotes
        .iter()
        .filter(|q| matches!(text(q, "state"), Some("converted" | "rejected" | "expired")))
        .count();
    let win_pct = if decided == 0 { 0 } else { converted * 100 / decided };
    let drafts = db::find::<Record>("sales_invoice").filter("state", "draft").limit(2000).all()?.len();
    let mut ranked: Vec<(String, Decimal)> = by_customer.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    let top: Vec<Value> = ranked.into_iter().take(8).map(|(id, v)| json!({ "name": name_of(&id), "value": v })).collect();
    Ok(json!({
        "open_orders": open,
        "open_value": open_value,
        "uninvoiced_value": uninvoiced,
        "open_quotations": open_quotes,
        "quotation_win_pct": win_pct,
        "draft_invoices": drafts,
        "sales_by_customer": top,
    }))
}

#[derive(Deserialize)]
struct ParentId {
    id: String,
}

fn lines(model: &str, parent_field: &str, id: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>(model).filter(parent_field, id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

handler! {
    fn list_so_lines(input: ParentId) -> Vec<Record> { lines("so_line", "so", &input.id) }
    fn list_quotation_lines(input: ParentId) -> Vec<Record> { lines("quotation_line", "quotation", &input.id) }
    fn list_invoice_lines(input: ParentId) -> Vec<Record> { lines("sales_invoice_line", "invoice", &input.id) }
    fn list_sos(f: Filter) -> Vec<Record> { list("so", &f, &["number", "notes"], "order_date") }
    fn list_quotations(f: Filter) -> Vec<Record> { list("quotation", &f, &["number", "notes"], "quote_date") }
    fn list_invoices(f: Filter) -> Vec<Record> { list("sales_invoice", &f, &["number"], "invoice_date") }
    fn list_deliveries(f: Filter) -> Vec<Record> { list("sales_delivery", &f, &["number"], "delivery_date") }
    fn list_returns(f: Filter) -> Vec<Record> { list("sales_return", &f, &["number", "reason"], "return_date") }
    fn sales_dashboard(f: Filter) -> Value { do_dashboard(f) }
}
