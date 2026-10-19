//! The morning sweep, and the digest each subscriber gets.

use aether_sdk::dates::{format_date, parse_date, Duration};
use aether_sdk::notify::{Audience, Level, Notification};
use aether_sdk::prelude::*;
use aether_sdk::report;
use aether_sdk::scheduler;
use serde_json::json;

use crate::common::{caller, id_of, require_admin, text, today, Record};

#[derive(Deserialize)]
struct Digest {
    subscription: String,
}

#[derive(Deserialize, Default)]
struct Nothing {}

fn is_on(sub: &Record, field: &str) -> bool {
    sub.get(field) != Some(&json!(false))
}

/// One count and a headline total from a model the subscriber may read; `None` when there is nothing
/// to say, or when their roles do not let them read it.
fn count(model: &str, filter: Value, company: Option<&str>, sum: Option<&str>) -> Option<(i64, Value)> {
    let mut filter = filter;
    if let Some(company) = company {
        filter["company"] = json!(company);
    }
    let measures = match sum {
        Some(field) => json!({ "n": "count", "sum": { "sum": field } }),
        None => json!({ "n": "count" }),
    };
    let answer = report::query(&json!({ "model": model, "filter": filter, "measures": measures })).ok()?;
    let n = answer["total"]["n"].as_i64().unwrap_or(0);
    (n > 0).then(|| (n, answer["total"]["sum"].clone()))
}

fn lines_for(sub: &Record, today: &str) -> Result<Vec<String>> {
    let company = text(sub, "company").filter(|c| !c.is_empty());
    let mut lines = Vec::new();
    if is_on(sub, "lots") {
        let days = sub.get("expiry_days").and_then(Value::as_i64).unwrap_or(30);
        let until = format_date(parse_date(today)? + Duration::days(days));
        let filter = json!({ "expiry_date": { "gte": today, "lte": until } });
        if let Some((n, _)) = count("erp_inventory.stock_lot", filter, company, None) {
            lines.push(format!("{n} lot(s) expire within {days} days"));
        }
    }
    if is_on(sub, "late_mos") {
        let filter = json!({ "planned_date": { "lt": today }, "state": { "nin": ["done", "cancelled"] } });
        if let Some((n, _)) = count("erp_manufacturing.mo", filter, company, None) {
            lines.push(format!("{n} manufacturing order(s) past their planned date"));
        }
    }
    if is_on(sub, "overdue_bills") {
        let filter = json!({ "state": "approved", "kind": "bill", "due_date": { "lt": today } });
        if let Some((n, total)) = count("erp_procurement.purchase_bill", filter, company, Some("total")) {
            lines.push(format!("{n} supplier bill(s) past due, {total} in all"));
        }
    }
    if is_on(sub, "open_requests") {
        if let Some((n, _)) = count("erp_replenishment.replenish_request", json!({ "state": "open" }), company, None) {
            lines.push(format!("{n} replenishment request(s) waiting to be ordered"));
        }
    }
    if is_on(sub, "rejected_inspections") {
        let since = format_date(parse_date(today)? - Duration::days(7));
        let filter = json!({ "state": "rejected", "inspected_on": { "gte": since } });
        if let Some((n, _)) = count("erp_quality.quality_inspection", filter, company, None) {
            lines.push(format!("{n} inspection(s) rejected in the last 7 days"));
        }
    }
    Ok(lines)
}

/// Runs as the subscriber, so the figures are the ones their roles allow.
fn do_digest(input: Digest) -> Result<Value> {
    let sub: Record = db::get("alert_subscription", &input.subscription)?
        .ok_or_else(|| Error::msg("the subscription is gone"))?;
    if text(&sub, "user") != Some(caller()?.as_str()) {
        return Err(Error::msg("a digest is only built for its own subscriber"));
    }
    let today = today()?;
    let lines = lines_for(&sub, &today)?;
    if lines.is_empty() {
        return Ok(json!({ "sent": false, "lines": 0 }));
    }
    Notification::new("ERP: needs attention")
        .body(lines.join("\n"))
        .level(Level::Warning)
        .to(Audience::Actors(vec![caller()?]))
        .send()?;
    db::update::<Record>("alert_subscription", id_of(&sub)?, &json!({ "last_sent": today }))?;
    Ok(json!({ "sent": true, "lines": lines.len() }))
}

/// The schedule's entry: queue a digest for every active subscriber, as that person.
fn do_sweep(_: Nothing) -> Result<Value> {
    let today = today()?;
    let subs = db::find::<Record>("alert_subscription").filter("active", true).limit(1000).all()?;
    let mut queued = 0;
    for sub in subs {
        let (Some(user), Ok(id)) = (text(&sub, "user"), id_of(&sub)) else { continue };
        scheduler::enqueue("digest")
            .payload(&json!({ "subscription": id }))
            .unique_key(&format!("digest-{id}-{today}"))
            .run_as(user)
            .send()?;
        queued += 1;
    }
    Ok(json!({ "queued": queued }))
}

/// Queue the digests now instead of waiting for the morning.
fn do_send_now(_: Nothing) -> Result<Value> {
    require_admin()?;
    do_sweep(Nothing {})
}

handler! {
    fn digest(input: Digest) -> Value { do_digest(input) }
    fn sweep(input: Nothing) -> Value { do_sweep(input) }
    fn send_now(input: Nothing) -> Value { do_send_now(input) }
}
