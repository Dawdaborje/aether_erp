# Items (`erp_item`)

## Purpose

Lean item master: category tree, SKU, UOM, stocked flag, tracking (none, lot, serial), shelf life and per-company valuation. Standard cost is an append-only dated rate table. App route `/erp/items`.

## Dependencies

`company`, `uom`

## Models

| Model | Role |
|---|---|
| `item_category` | Code/name tree; optional default `valuation_method` (`fifo` / `average` / `standard`) |
| `item` | SKU: `code`, `name`, `uom`, optional `category`, `tracking` (`none` / `lot` / `serial`), optional `expiry_days`, `is_stocked`, `active` |
| `item_company` | Per `(item, company)`: `valuation_method`, denormalized `standard_cost` for display |
| `item_standard_cost` | Append-only `(item, company, effective_date)` → `rate` |
| `item_supplier` | Per `(item, supplier)`: `supplier_sku`, `min_qty`, `lead_days`, `is_preferred` (one preferred per item) |
| `item_customer` | Per `(item, customer)`: `customer_sku` |
| `item_uom` | Per `(item, uom)`: `factor` in the item's own unit, `use_for` (`both` / `purchase` / `sales`) |

## Design

- Valuation method on `item_company` wins; else active category default; else `fifo`.
- Standard cost: one row per effective date (no overwrite). `valuation_for` picks the latest rate with `effective_date ≤ as_of` (default today); falls back to `item_company.standard_cost` for older rows.
- Tracking is `none`, `lot` or `serial` (a serial is a lot of exactly one unit). `expiry_days` gives lots a default expiry from the receipt date. Non-stocked items are still master data for buying/selling.
- **Unit conversion:** the item's own unit converts at 1. Another unit uses the item's `item_uom` row (a case of 12 is `12`), else the ratio of the two units' reference factors when they share a unit group. `convert_qty` returns the quantity in the target unit (default the item's own) and `base_qty`. Rows marked `purchase` or `sales` apply only in that context.
- **Supplier and customer data** is master data only: procurement and replenishment read `min_qty`, `lead_days` and the preferred supplier; documents print the partner's code.
- Cross-plugin readers: inventory, procurement, manufacturing, and selling roles may call read handlers (`get_item`, `search_items`, `valuation_for`).

## Functions, commands, events

**Handlers:** `create_category`, `set_category`, `create_item`, `set_item`, `set_item_company`, `set_standard_cost`, `get_item`, `search_items`, `valuation_for`

**Partner data and units:** `set_item_supplier`, `list_item_suppliers`, `set_item_customer`, `list_item_customers`, `set_item_uom`, `list_item_uoms`, `convert_qty`

**Commands / events:** none

## Roles

| Role | Scope |
|---|---|
| `item_admin` | Categories, items, company settings, standard costs |
| `item_reader` | Read item master and valuation |

## Boundaries

- No warehouses, stock qty, or GL. Warehouses and the ledger live in `erp_inventory`.
- Does not post accounting entries; valuation method/cost are inputs to inventory layers.
