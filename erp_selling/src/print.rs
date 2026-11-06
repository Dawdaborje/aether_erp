//! Printable and mailable copies of the sales documents.
//!
//! The PDF is a table made by the kernel's `report::export` (title, columns, rows), so it has no
//! letterhead: it lists the lines and the totals. The email carries the same figures in its body.
//! Email attachments are not offered by the platform, hence the figures are in the message itself.
//! Nothing is sent unless an administrator has set up a mail provider; without one the send fails
//! with a clear message and nothing changes.

use aether_sdk::communication;
use aether_sdk::prelude::*;
use aether_sdk::report;
use serde_json::{json, Map, Value};

use crate::common::{id_of, require, require_reader, require_user, text, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

/// Which document a copy is made of.
#[derive(Clone, Copy)]
enum Kind {
    Quotation,
    Order,
    Invoice,
}

impl Kind {
    fn header(self) -> &'static str {
        match self {
            Kind::Quotation => "quotation",
            Kind::Order => "so",
            Kind::Invoice => "sales_invoice",
        }
    }
    fn lines(self) -> (&'static str, &'static str) {
        match self {
            Kind::Quotation => ("quotation_line", "quotation"),
            Kind::Order => ("so_line", "so"),
            Kind::Invoice => ("sales_invoice_line", "invoice"),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Quotation => "Quotation",
            Kind::Order => "Sales order",
            Kind::Invoice => "Invoice",
        }
    }
    fn date_field(self) -> &'static str {
        match self {
            Kind::Quotation => "quote_date",
            Kind::Order => "order_date",
            Kind::Invoice => "invoice_date",
        }
    }
}

struct Doc {
    number: String,
    date: String,
    customer: String,
    email: Option<String>,
    currency: String,
    notes: String,
    /// `[#, item, qty, price, discount, net, tax]` per line.
    lines: Vec<[String; 7]>,
    subtotal: String,
    tax: String,
    total: String,
}

/// A stored value as it should read on paper: strings as they are, numbers without noise.
fn shown(record: &Record, field: &str) -> String {
    match record.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// `3.000000` -> `3`, `120.500000` -> `120.5`; whole money (`360.00`) is left alone.
fn plain(value: String) -> String {
    if value.contains('.') && value.trim_end_matches('0').len() != value.len() && value.split('.').nth(1).map_or(false, |f| f.len() > 2) {
        let t = value.trim_end_matches('0').trim_end_matches('.');
        t.to_string()
    } else {
        value
    }
}

fn build(kind: Kind, id: &str) -> Result<Doc> {
    let head = require(kind.header(), id, "document")?;
    let number = shown(&head, "number");
    if number.is_empty() {
        return Err(Error::msg("this document has no number yet: send or confirm it first"));
    }
    let party = require("party.party", text(&head, "customer").unwrap_or_default(), "customer")?;
    let name = text(&party, "display_name").or_else(|| text(&party, "name")).unwrap_or("customer").to_string();
    let email = text(&party, "email").map(str::trim).filter(|e| !e.is_empty()).map(str::to_string);

    let (line_model, parent) = kind.lines();
    let mut rows: Vec<Record> = db::find(line_model).filter(parent, id).limit(500).all()?;
    rows.sort_by_key(|r| r.get("line_no").and_then(Value::as_i64).unwrap_or(0));
    let mut names: Vec<(String, String)> = Vec::new();
    let mut lines = Vec::new();
    for row in &rows {
        let item_id = text(row, "item").unwrap_or_default().to_string();
        let item_name = match names.iter().find(|(i, _)| *i == item_id) {
            Some((_, n)) => n.clone(),
            None => {
                let item = require("erp_item.item", &item_id, "item")?;
                let n = format!("{} {}", shown(&item, "code"), shown(&item, "name")).trim().to_string();
                names.push((item_id.clone(), n.clone()));
                n
            }
        };
        let optional = row.get("optional") == Some(&json!(true));
        lines.push([
            shown(row, "line_no"),
            if optional { format!("{item_name} (optional)") } else { item_name },
            plain(shown(row, "qty")),
            plain(shown(row, "unit_price")),
            plain(shown(row, "discount_pct")),
            shown(row, "amount"),
            shown(row, "tax_amount"),
        ]);
    }
    Ok(Doc {
        number,
        date: shown(&head, kind.date_field()),
        customer: name,
        email,
        currency: shown(&head, "currency"),
        notes: shown(&head, "notes"),
        lines,
        subtotal: shown(&head, "subtotal"),
        tax: shown(&head, "tax_total"),
        total: shown(&head, "total"),
    })
}

fn row(pairs: &[(&str, String)]) -> Value {
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), json!(v));
    }
    Value::Object(map)
}

fn do_pdf(kind: Kind, input: Id) -> Result<Value> {
    require_reader()?;
    let doc = build(kind, &input.id)?;
    let mut rows = vec![
        row(&[("item", format!("Customer: {}", doc.customer))]),
        row(&[("item", format!("Date: {}    Currency: {}", doc.date, doc.currency))]),
    ];
    if !doc.notes.is_empty() {
        rows.push(row(&[("item", format!("Notes: {}", doc.notes))]));
    }
    rows.push(row(&[]));
    for l in &doc.lines {
        rows.push(row(&[
            ("n", l[0].clone()),
            ("item", l[1].clone()),
            ("qty", l[2].clone()),
            ("price", l[3].clone()),
            ("disc", l[4].clone()),
            ("net", l[5].clone()),
            ("tax", l[6].clone()),
        ]));
    }
    rows.push(row(&[]));
    rows.push(row(&[("item", "Subtotal".into()), ("net", doc.subtotal.clone())]));
    rows.push(row(&[("item", "Tax".into()), ("net", doc.tax.clone())]));
    rows.push(row(&[("item", format!("Total {}", doc.currency)), ("net", doc.total.clone())]));
    let file = report::export(&json!({
        "format": "pdf",
        "title": format!("{} {}", kind.label(), doc.number),
        "columns": [
            {"field": "n", "label": "#"},
            {"field": "item", "label": "Item"},
            {"field": "qty", "label": "Qty"},
            {"field": "price", "label": "Price"},
            {"field": "disc", "label": "Disc %"},
            {"field": "net", "label": "Net"},
            {"field": "tax", "label": "Tax"},
        ],
        "rows": rows,
    }))?;
    Ok(file)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn do_email(kind: Kind, input: Id) -> Result<Value> {
    require_user()?;
    let doc = build(kind, &input.id)?;
    let to = doc.email.clone().ok_or_else(|| Error::msg(format!("{} has no email address on the customer record", doc.customer)))?;
    let label = kind.label();
    let mut text_body = format!("{label} {} for {}\nDate: {}\n\n", doc.number, doc.customer, doc.date);
    let mut html = format!(
        "<p>{} <b>{}</b> for {}<br>Date: {}</p><table border=\"1\" cellpadding=\"4\" cellspacing=\"0\"><tr><th>#</th><th>Item</th><th>Qty</th><th>Price</th><th>Disc %</th><th>Net</th><th>Tax</th></tr>",
        label,
        escape(&doc.number),
        escape(&doc.customer),
        escape(&doc.date)
    );
    for l in &doc.lines {
        text_body.push_str(&format!("{}. {}  x{}  @ {}  = {}\n", l[0], l[1], l[2], l[3], l[5]));
        html.push_str("<tr>");
        for cell in l {
            html.push_str(&format!("<td>{}</td>", escape(cell)));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    text_body.push_str(&format!("\nSubtotal {}\nTax {}\nTotal {} {}\n", doc.subtotal, doc.tax, doc.total, doc.currency));
    html.push_str(&format!(
        "<p>Subtotal {}<br>Tax {}<br><b>Total {} {}</b></p>",
        escape(&doc.subtotal),
        escape(&doc.tax),
        escape(&doc.total),
        escape(&doc.currency)
    ));
    if !doc.notes.is_empty() {
        text_body.push_str(&format!("\n{}\n", doc.notes));
        html.push_str(&format!("<p>{}</p>", escape(&doc.notes)));
    }
    communication::email([to.as_str()])
        .subject(&format!("{label} {}", doc.number))
        .text(&text_body)
        .html(&html)
        .send()
        .map_err(|e| e.or("the email was not queued: ask an administrator to set up the mail provider"))?;
    Ok(json!({ "sent_to": to, "document": doc.number, "id": id_of(&require(kind.header(), &input.id, "document")?)? }))
}

handler! {
    fn quotation_pdf(input: Id) -> Value { do_pdf(Kind::Quotation, input) }
    fn so_pdf(input: Id) -> Value { do_pdf(Kind::Order, input) }
    fn invoice_pdf(input: Id) -> Value { do_pdf(Kind::Invoice, input) }
    fn email_quotation(input: Id) -> Value { do_email(Kind::Quotation, input) }
    fn email_so(input: Id) -> Value { do_email(Kind::Order, input) }
    fn email_invoice(input: Id) -> Value { do_email(Kind::Invoice, input) }
}
