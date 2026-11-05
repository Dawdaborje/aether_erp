//! List sources for the screens.

use aether_sdk::prelude::*;

use crate::common::{require_reader, text, Record};

#[derive(Deserialize, Default)]
struct Filter {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

fn list(model: &str, f: &Filter, search: &[&str], order: &str) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>(model).limit(1000).all()?;
    if let Some(k) = f.kind.as_deref().filter(|k| !k.is_empty()) {
        rows.retain(|r| text(r, "kind") == Some(k));
    }
    if let Some(needle) = f.q.as_deref().map(str::to_lowercase).filter(|n| !n.is_empty()) {
        rows.retain(|r| search.iter().any(|f| text(r, f).map_or(false, |v| v.to_lowercase().contains(&needle))));
    }
    rows.sort_by(|a, b| text(a, order).cmp(&text(b, order)));
    Ok(rows)
}

#[derive(Deserialize)]
struct ParentId {
    id: String,
}

fn do_price_items_of(input: ParentId) -> Result<Vec<Record>> {
    require_reader()?;
    let mut rows: Vec<Record> = db::find::<Record>("price_list_item").filter("price_list", input.id.as_str()).limit(1000).all()?;
    rows.sort_by(|a, b| text(a, "item").cmp(&text(b, "item")));
    Ok(rows)
}

handler! {
    fn list_price_list_items(input: ParentId) -> Vec<Record> { do_price_items_of(input) }
    fn search_taxes(f: Filter) -> Vec<Record> { list("tax", &f, &["code", "name"], "code") }
    fn list_payment_terms(f: Filter) -> Vec<Record> { list("payment_term", &f, &["code", "name"], "code") }
    fn list_price_lists(f: Filter) -> Vec<Record> { list("price_list", &f, &["code", "name"], "code") }
    fn list_price_items(f: Filter) -> Vec<Record> { list("price_list_item", &f, &["item"], "item") }
}
