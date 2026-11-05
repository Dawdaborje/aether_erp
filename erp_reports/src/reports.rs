//! Sales, purchase, order-status and manufacturing reports, and their export.

use aether_sdk::prelude::*;
use aether_sdk::report;
use serde_json::json;

use crate::common::{require_reader, today};

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    company: Option<String>,
    /// A kernel period name (`this_month`, `last_quarter`, `this_year`, ...).
    #[serde(default)]
    period: Option<String>,
    /// `month` (default) or `party`.
    #[serde(default)]
    group_by: Option<String>,
}

#[derive(Deserialize)]
struct ExportInput {
    report: String,
    #[serde(flatten)]
    filter: Filter,
    #[serde(default)]
    format: Option<String>,
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

fn scope(f: &Filter, date_field: &str, mut filter: Value) -> Value {
    if let Some(company) = non_empty(&f.company) {
        filter["company"] = json!(company);
    }
    if let Some(period) = non_empty(&f.period) {
        filter[date_field] = json!({ "in_period": period });
    }
    filter
}

fn dimension(f: &Filter, party_field: &str, date_field: &str) -> Value {
    match non_empty(&f.group_by) {
        Some("party") => json!(party_field),
        _ => json!({ "field": date_field, "grain": "month" }),
    }
}

/// Approved invoices (or bills) and the credit (or debit) notes against them, per month or party.
fn money_report(
    f: &Filter,
    model: &str,
    party_field: &str,
    date_field: &str,
    plain: &str,
    note: &str,
) -> Result<Value> {
    let by_party = non_empty(&f.group_by) == Some("party");
    report::query(&json!({
        "model": model,
        "filter": scope(f, date_field, json!({ "state": "approved" })),
        "dimensions": [dimension(f, party_field, date_field)],
        "measures": {
            "documents": { "count_if": { "kind": plain } },
            "amount": { "sum_if": { "field": "total", "where": { "kind": plain } } },
            "notes": { "sum_if": { "field": "total", "where": { "kind": note } } },
            "tax": { "sum_if": { "field": "tax_total", "where": { "kind": plain } } }
        },
        "formulas": { "net": { "delta": ["amount", "notes"] } },
        "order": if by_party { "-amount" } else { date_field },
        "limit": 1000
    }))
}

fn do_sales_report(f: Filter) -> Result<Value> {
    require_reader()?;
    money_report(&f, "erp_selling.sales_invoice", "customer", "invoice_date", "invoice", "credit_note")
}

fn do_purchase_report(f: Filter) -> Result<Value> {
    require_reader()?;
    money_report(&f, "erp_procurement.purchase_bill", "supplier", "bill_date", "bill", "debit_note")
}

fn by_state(model: &str, date_field: &str, f: &Filter) -> Result<Value> {
    report::query(&json!({
        "model": model,
        "filter": scope(f, date_field, json!({})),
        "dimensions": ["state"],
        "measures": { "orders": "count", "value": { "sum": "total" } },
        "order": "-orders"
    }))
}

/// Sales and purchase orders grouped by state.
fn do_order_status(f: Filter) -> Result<Value> {
    require_reader()?;
    Ok(json!({
        "sales": by_state("erp_selling.so", "order_date", &f)?,
        "purchases": by_state("erp_procurement.po", "order_date", &f)?,
    }))
}

/// Manufacturing orders by state, with the ones past their planned date that are not finished.
fn do_mfg_report(f: Filter) -> Result<Value> {
    require_reader()?;
    let late = json!({
        "planned_date": { "lt": today()? },
        "state": { "nin": ["done", "cancelled"] }
    });
    let mut filter = json!({});
    if let Some(company) = non_empty(&f.company) {
        filter["company"] = json!(company);
    }
    report::query(&json!({
        "model": "erp_manufacturing.mo",
        "filter": filter,
        "dimensions": ["state"],
        "measures": {
            "orders": "count",
            "qty": { "sum": "qty" },
            "late": { "count_if": late },
            "labour": { "sum": "labour_cost" }
        },
        "order": "-orders"
    }))
}

fn rows_of(report: Value) -> Vec<Value> {
    report["rows"].as_array().cloned().unwrap_or_default()
}

fn total_of(report: &Value, measure: &str) -> Value {
    report["total"].get(measure).cloned().unwrap_or(json!(0))
}

fn series(report: &Value, measure: &str) -> Vec<Value> {
    report["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let label = row
                        .as_object()
                        .and_then(|o| o.iter().find(|(_, v)| v.is_string()).map(|(_, v)| v.clone()))
                        .unwrap_or(Value::Null);
                    json!({ "label": label, "value": row.get(measure).cloned().unwrap_or(json!(0)) })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn open_orders(model: &str, open: &[&str]) -> Result<Value> {
    let r = report::query(&json!({
        "model": model,
        "filter": { "state": { "in": open } },
        "measures": { "orders": "count", "value": { "sum": "total" } }
    }))?;
    Ok(r["total"].clone())
}

fn do_dashboard(f: Filter) -> Result<Value> {
    require_reader()?;
    let month = Filter { company: f.company.clone(), period: Some("this_month".into()), group_by: None };
    let sales = money_report(&month, "erp_selling.sales_invoice", "customer", "invoice_date", "invoice", "credit_note")?;
    let purchases = money_report(&month, "erp_procurement.purchase_bill", "supplier", "bill_date", "bill", "debit_note")?;
    let year = Filter { company: f.company.clone(), period: Some("this_year".into()), group_by: None };
    let trend = money_report(&year, "erp_selling.sales_invoice", "customer", "invoice_date", "invoice", "credit_note")?;
    let sales_open = open_orders("erp_selling.so", &["confirmed", "partially_delivered"])?;
    let po_open = open_orders("erp_procurement.po", &["confirmed", "partially_received", "to_approve"])?;
    let mfg = do_mfg_report(Filter { company: f.company, ..Filter::default() })?;
    Ok(json!({
        "sales_this_month": total_of(&sales, "net"),
        "purchases_this_month": total_of(&purchases, "net"),
        "open_sales_orders": sales_open["orders"],
        "open_sales_value": sales_open["value"],
        "open_purchase_orders": po_open["orders"],
        "open_purchase_value": po_open["value"],
        "late_mos": total_of(&mfg, "late"),
        "sales_by_month": series(&trend, "net"),
        "mos_by_state": series(&mfg, "orders"),
    }))
}

/// The sales or purchase report as an Excel or PDF table.
fn do_export(input: ExportInput) -> Result<Value> {
    require_reader()?;
    let (title, data) = match input.report.as_str() {
        "sales" => ("Sales", do_sales_report(input.filter)?),
        "purchases" => ("Purchases", do_purchase_report(input.filter)?),
        other => return Err(Error::msg(format!("unknown report `{other}`: use `sales` or `purchases`"))),
    };
    let format = input.format.as_deref().filter(|f| !f.is_empty()).unwrap_or("xlsx");
    let first = data["rows"].get(0).and_then(Value::as_object);
    let columns: Vec<Value> = first
        .map(|row| row.keys().map(|k| json!({ "field": k, "label": k })).collect())
        .unwrap_or_default();
    report::export(&json!({ "format": format, "title": title, "columns": columns, "rows": data["rows"] }))
}

handler! {
    fn sales_orders_by_state(f: Filter) -> Vec<Value> { require_reader()?; Ok(rows_of(by_state("erp_selling.so", "order_date", &f)?)) }
    fn purchase_orders_by_state(f: Filter) -> Vec<Value> { require_reader()?; Ok(rows_of(by_state("erp_procurement.po", "order_date", &f)?)) }
    fn mfg_by_state(f: Filter) -> Vec<Value> { Ok(rows_of(do_mfg_report(f)?)) }
    fn sales_report(f: Filter) -> Value { do_sales_report(f) }
    fn purchase_report(f: Filter) -> Value { do_purchase_report(f) }
    fn order_status(f: Filter) -> Value { do_order_status(f) }
    fn mfg_report(f: Filter) -> Value { do_mfg_report(f) }
    fn reports_dashboard(f: Filter) -> Value { do_dashboard(f) }
    fn export_report(input: ExportInput) -> Value { do_export(input) }
}
