//! A person's subscription to the digest.

use aether_sdk::prelude::*;
use serde_json::json;

use crate::common::{caller, id_of, require_user, Record};

#[derive(Deserialize, Default)]
struct Preferences {
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    expiry_days: Option<i64>,
    #[serde(default)]
    lots: Option<bool>,
    #[serde(default)]
    late_mos: Option<bool>,
    #[serde(default)]
    overdue_bills: Option<bool>,
    #[serde(default)]
    open_requests: Option<bool>,
    #[serde(default)]
    rejected_inspections: Option<bool>,
}

pub fn mine() -> Result<Option<Record>> {
    db::find::<Record>("alert_subscription").filter("user", caller()?.as_str()).first()
}

/// Create the caller's subscription, or change the fields given.
fn do_subscribe(p: Preferences) -> Result<Record> {
    require_user()?;
    let mut data = serde_json::Map::new();
    if let Some(company) = p.company.as_deref().filter(|c| !c.is_empty()) {
        data.insert("company".into(), json!(company));
    }
    if let Some(days) = p.expiry_days {
        if !(1..=365).contains(&days) {
            return Err(Error::msg("expiry_days is between 1 and 365"));
        }
        data.insert("expiry_days".into(), json!(days));
    }
    for (name, value) in [
        ("active", p.active),
        ("lots", p.lots),
        ("late_mos", p.late_mos),
        ("overdue_bills", p.overdue_bills),
        ("open_requests", p.open_requests),
        ("rejected_inspections", p.rejected_inspections),
    ] {
        if let Some(value) = value {
            data.insert(name.into(), json!(value));
        }
    }
    match mine()? {
        Some(existing) => {
            db::update(
                "alert_subscription",
                id_of(&existing)?,
                &Value::Object(data),
            )?
            .ok_or_else(|| Error::msg("the subscription is gone"))
        }
        None => {
            data.insert("user".into(), json!(caller()?));
            db::create("alert_subscription", &Value::Object(data))
        }
    }
}

fn do_my_subscription(_: Value) -> Result<Vec<Record>> {
    require_user()?;
    Ok(mine()?.into_iter().collect())
}

fn do_list_subscriptions(_: Value) -> Result<Vec<Record>> {
    require_user()?;
    db::find::<Record>("alert_subscription").limit(500).all()
}

handler! {
    fn subscribe(p: Preferences) -> Record { do_subscribe(p) }
    fn my_subscription(input: Value) -> Vec<Record> { do_my_subscription(input) }
    fn list_subscriptions(input: Value) -> Vec<Record> { do_list_subscriptions(input) }
}
