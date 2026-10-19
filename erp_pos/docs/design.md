# Point of sale (`erp_pos`)

## Purpose

Counter sales: a cashier opens a session, rings up a cart priced from the sell price list, takes payment, and the stock leaves the warehouse in the same step. App route `/erp/pos`.

## Dependencies

`erp_item`, `erp_pricing`, `erp_inventory`, `company`, `party`

## Models

| Model | Role |
|---|---|
| `pos_profile` | A till: company, warehouse to sell from, sell price list, currency |
| `pos_session` | One cashier's shift (`SES-{YYYY}-#####`): float, state `open` / `closed`, cash and other takings, expected vs counted cash, difference |
| `pos_sale` | A cart (`draft`) that becomes `paid` or `void`. Numbered `POS-{YYYY}-#####` when it is paid, so a deleted cart leaves no gap. Totals, paid, change, the stock move |
| `pos_sale_line` | Item, qty, unit price, discount, tax, net and tax amounts, optional lot |
| `pos_payment` | One tender (`cash`, `card`, `other`) per sale, with its session |

## Design

- **Pricing:** a line's price comes from `erp_pricing.price_for` (the profile's price list, quantity breaks, dates) unless a price is typed; `compute_line` gives net and tax. Totals are rebuilt from the lines on every add and remove.
- **Paying issues the stock:** `pay_sale` drafts an `erp_inventory` issue from the profile's warehouse, adds a line per stocked item, and posts it before the sale is marked paid. A sale the stock cannot supply fails with the ledger's message and stays a cart. Non-stocked items are sold without a stock line. Tracked items need the lot named on the line.
- **Tender:** paid must cover the total. Change is given only out of cash.
- **Closing:** refused while carts are open. `expected cash = float + cash taken − change given`; the difference against the counted cash is stored on the session.
- **Void:** a manager voids a paid sale while its session is open; the issue is voided in the ledger. After the session closes a void needs a return instead (not built).
- **Events:** `pos_sale_paid` and `pos_sale_voided`, with totals, tax, cash and change, for a ledger plugin to turn into entries. This plugin posts nothing itself.
- **Roles:** `pos_cashier` sells and closes their own session; `pos_manager` also sets up profiles, voids, and acts on any session; `pos_reader` reads. The calls into inventory run as the caller, so a cashier also needs `stock_user`, and a void needs `stock_admin`.

## Functions

`create_profile`, `list_profiles`, `open_session`, `close_session`, `list_sessions`, `my_session`, `new_sale`, `add_sale_line`, `remove_sale_line`, `list_sale_lines`, `pay_sale`, `void_sale`, `list_sales`, `pos_dashboard`

## Boundaries

- One open session per cashier.
- No barcode scanning, receipt printing, offline mode, held tills or split by customer account.
- No returns after a session closes; no loyalty or promotions beyond price list discounts.
- Lots are not picked automatically (no FEFO); name the lot on the line.
- Stock is checked when payment is taken, not when an item is added to the cart.
