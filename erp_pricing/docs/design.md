# Pricing & taxes (`erp_pricing`)

## Purpose

One place for how a document line is priced and taxed: tax rates, payment terms and buy/sell price lists. Procurement and selling call it instead of each re-implementing the arithmetic. App route `/erp/pricing`.

## Dependencies

`erp_item`, `party`

## Models

| Model | Role |
|---|---|
| `tax` | `code`, `kind` (`percent` / `fixed`), `rate`, `applies_to` (`both` / `sales` / `purchase`), `price_includes_tax`, optional ledger `account` hint |
| `payment_term` | `net_days`, early-payment `discount_pct` within `discount_days` |
| `price_list` | `kind` (`sell` / `buy`), optional `currency` / `company` restriction, one `is_default` per kind |
| `price_list_item` | `(price_list, item[, party], min_qty)` → `unit_price`, optional `discount_pct`, validity window |

## Design

- **Line amounts** (pure, `rules.rs`): `gross = qty × unit_price`; `discount = gross × pct`; `net` is the taxable base; `total = net + tax`. Inclusive percent taxes are split out of the price (`net = after_discount × 100 / (100 + rate)`), so `total` stays what the party pays. Fixed taxes are per unit. Rounding is half away from zero at money scale.
- **`net_unit`** is the tax-exclusive, post-discount unit price at price scale. Receipts value stock at it, so recoverable tax and discounts never reach inventory cost.
- **Price pick:** among rows valid on the date with `min_qty ≤ qty`, take the highest `min_qty`; a party-specific row beats a general one at the same break; then the latest `valid_from`. No match is `found: false`, not an error, so callers fall back to a typed price.
- **Payment terms** overlap `gl_payment`'s `pay_term` (net days); these add the early-payment discount window and are what ERP documents use to set `due_date`.
- **Tax groups:** a `tax` of kind `group` has no rate; its `components` (a child field of `tax_component` rows, in order) are applied one after another. A line still carries *one* `tax` (a plain tax or a group), so every document prices through `compute_line` unchanged; the answer adds `breakdown` (each component's tax, code, ledger `account`, signed amount, `withholding`).
- **Compound:** a component marked `compound` is worked out on the price plus the taxes added before it (`10%` then compound `5%` on 100 is 10.00 then 5.50).
- **Withholding:** a tax with `is_withholding` is negative on the document (owed to the tax authority, not to the supplier), so `total = net + tax` still holds with `tax` the signed sum.
- **Included taxes in a group** must all be included, none compound or withheld; the price holds all of them, split in proportion to their rates (the last takes the rounding). A group cannot mix included with added-on-top percent taxes, and cannot contain another group (refused when the group is created).
- A single plain tax prices exactly as before.

## Functions

`create_tax`, `create_tax_group`, `set_tax`, `list_taxes`, `get_tax`, `create_payment_term`, `due_date_for`, `create_price_list`, `set_item_price`, `price_for`, `compute_line`; screens: `search_taxes`, `list_payment_terms`, `list_price_lists`, `list_price_items`, `list_price_list_items`

## Roles

| Role | Scope |
|---|---|
| `pricing_admin` | Maintain taxes, terms, lists |
| `pricing_reader` | Read; purchase and sales roles may also read |

## Boundaries

- Does not post accounting entries. Bills and invoices emit `tax_total` and `due_date`; `gl_erp` maps them to input/output tax accounts and `gl_payment`.
- No reverse charge or per-jurisdiction tax determination. Documents keep the summed tax; the per-tax `breakdown` is returned by `compute_line` but not yet stored on documents or forwarded to the ledger, so `gl_erp` posts the total (a negative withholding total needs its own posting rule).
