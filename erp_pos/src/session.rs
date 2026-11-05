//! Profiles and cashier sessions.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;
use serde_json::json;

use crate::common::{
    caller, id_of, is_manager, money_of, open_session_of, require, require_cashier, require_manager, require_reader, text, today, Record,
    MONEY_SCALE,
};

#[derive(Deserialize)]
struct ProfileInput {
    company: String,
    name: String,
    warehouse: String,
    #[serde(default)]
    price_list: Option<String>,
    #[serde(default)]
    currency: Option<String>,
}

#[derive(Deserialize)]
struct OpenInput {
    profile: String,
    #[serde(default)]
    opening_cash: Option<Decimal>,
}

#[derive(Deserialize)]
struct CloseInput {
    #[serde(alias = "session")]
    id: String,
    counted_cash: Decimal,
}

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    state: Option<String>,
}

fn do_create_profile(input: ProfileInput) -> Result<Record> {
    require_manager()?;
    let company = require("company.company", &input.company, "company")?;
    let warehouse = require("erp_inventory.stock_warehouse", &input.warehouse, "warehouse")?;
    let mut data = json!({
        "company": id_of(&company)?,
        "name": input.name.trim(),
        "warehouse": id_of(&warehouse)?,
        "active": true,
    });
    if let Some(list) = input.price_list.as_deref().filter(|l| !l.is_empty()) {
        let list = require("erp_pricing.price_list", list, "price list")?;
        if text(&list, "kind") != Some("sell") {
            return Err(Error::msg("a till sells: pick a sell price list"));
        }
        data["price_list"] = json!(id_of(&list)?);
    }
    if let Some(currency) = input.currency.as_deref().filter(|c| !c.is_empty()) {
        data["currency"] = json!(currency.to_uppercase());
    }
    db::create("pos_profile", &data).map_err(|e| e.or("could not create the profile (the name may be taken)"))
}

fn do_list_profiles(_: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    db::find::<Record>("pos_profile").limit(200).all()
}

fn do_open_session(input: OpenInput) -> Result<Record> {
    require_cashier()?;
    let profile = require("pos_profile", &input.profile, "profile")?;
    if profile.get("active") == Some(&json!(false)) {
        return Err(Error::msg("the profile is not active"));
    }
    let cashier = caller()?;
    let already = db::find::<Record>("pos_session").filter("cashier", cashier.as_str()).filter("state", "open").first()?;
    if already.is_some() {
        return Err(Error::msg("you already have an open session: close it first"));
    }
    let float = input.opening_cash.unwrap_or_else(|| Decimal::zero(MONEY_SCALE)).with_scale(MONEY_SCALE)?;
    if float.is_negative() {
        return Err(Error::msg("the opening cash must not be negative"));
    }
    db::create(
        "pos_session",
        &json!({
            "profile": id_of(&profile)?,
            "company": text(&profile, "company"),
            "cashier": cashier,
            "state": "open",
            "opened_on": today()?,
            "opening_cash": float,
        }),
    )
}

/// Count the drawer against the sales: `expected = float + cash taken - change given`.
fn do_close_session(input: CloseInput) -> Result<Record> {
    require_cashier()?;
    let session = open_session_of(&input.id)?;
    let session_id = id_of(&session)?.to_string();
    let sales: Vec<Record> = db::find("pos_sale").filter("session", session_id.as_str()).limit(1000).all()?;
    let unpaid = sales.iter().filter(|s| text(s, "state") == Some("draft")).count();
    if unpaid > 0 {
        return Err(Error::msg(format!("{unpaid} cart(s) are still open: take payment or remove them first")));
    }
    let paid: Vec<&Record> = sales.iter().filter(|s| text(s, "state") == Some("paid")).collect();
    let paid_ids: Vec<String> = paid.iter().filter_map(|s| id_of(s).ok().map(str::to_string)).collect();
    let payments: Vec<Record> = db::find("pos_payment").filter("session", session_id.as_str()).limit(5000).all()?;
    let zero = Decimal::zero(MONEY_SCALE);
    let (mut cash, mut other) = (zero.clone(), zero.clone());
    for payment in payments.iter().filter(|p| text(p, "sale").map_or(false, |s| paid_ids.iter().any(|id| id == s))) {
        let amount = money_of(payment, "amount")?;
        if text(payment, "method") == Some("cash") {
            cash = cash.checked_add(amount)?;
        } else {
            other = other.checked_add(amount)?;
        }
    }
    for sale in &paid {
        cash = cash.checked_sub(money_of(sale, "change")?)?;
    }
    let expected = money_of(&session, "opening_cash")?.checked_add(cash.clone())?;
    let counted = input.counted_cash.with_scale(MONEY_SCALE)?;
    if counted.is_negative() {
        return Err(Error::msg("the counted cash must not be negative"));
    }
    let difference = counted.checked_sub(expected.clone())?;
    db::update(
        "pos_session",
        &session_id,
        &json!({
            "state": "closed",
            "closed_on": today()?,
            "cash_sales": cash,
            "card_sales": other,
            "expected_cash": expected,
            "counted_cash": counted,
            "difference": difference,
        }),
    )?
    .ok_or_else(|| Error::msg("the session is gone"))
}

fn do_list_sessions(f: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let mut q = db::find::<Record>("pos_session").limit(500);
    if let Some(state) = f.state.as_deref().filter(|s| !s.is_empty()) {
        q = q.filter("state", state);
    }
    q.all()
}

/// The caller's open session, if any, as a one-row list.
fn do_my_session(_: Filter) -> Result<Vec<Record>> {
    require_reader()?;
    let cashier = caller()?;
    let mut q = db::find::<Record>("pos_session").filter("state", "open");
    if !is_manager() {
        q = q.filter("cashier", cashier.as_str());
    }
    q.limit(50).all()
}

handler! {
    fn create_profile(input: ProfileInput) -> Record { do_create_profile(input) }
    fn list_profiles(input: Filter) -> Vec<Record> { do_list_profiles(input) }
    fn open_session(input: OpenInput) -> Record { do_open_session(input) }
    fn close_session(input: CloseInput) -> Record { do_close_session(input) }
    fn list_sessions(input: Filter) -> Vec<Record> { do_list_sessions(input) }
    fn my_session(input: Filter) -> Vec<Record> { do_my_session(input) }
}
