# Items (`erp_item`)

## Purpose

Lean item master: category tree, SKU, UOM, stocked flag, and per-company valuation. Standard cost is an append-only dated rate table. App route `/erp/items`.

## Dependencies

`company`, `uom`

## Models

| Model | Role |
|---|---|
| `item_category` | Code/name tree; optional default `valuation_method` (`fifo` / `average` / `standard`) |
| `item` | SKU: `code`, `name`, `uom`, optional `category`, `tracking` (`none` / `lot`), `is_stocked`, `active` |
| `item_company` | Per `(item, company)`: `valuation_method`, denormalized `standard_cost` for display |
| `item_standard_cost` | Append-only `(item, company, effective_date)` → `rate` |

## Design

- Valuation method on `item_company` wins; else active category default; else `fifo`.
- Standard cost: one row per effective date (no overwrite). `valuation_for` picks the latest rate with `effective_date ≤ as_of` (default today); falls back to `item_company.standard_cost` for older rows.
- Tracking is `none` or `lot` only (no serial in v0.1). Non-stocked items are still master data for buying/selling.
- Cross-plugin readers: inventory, procurement, manufacturing, and selling roles may call read handlers (`get_item`, `search_items`, `valuation_for`).

## Functions, commands, events

**Handlers:** `create_category`, `set_category`, `create_item`, `set_item`, `set_item_company`, `set_standard_cost`, `get_item`, `search_items`, `valuation_for`

**Commands / events:** none

## Roles

| Role | Scope |
|---|---|
| `item_admin` | Categories, items, company settings, standard costs |
| `item_reader` | Read item master and valuation |

## Boundaries

- No warehouses, stock qty, or GL. Warehouses and the ledger live in `erp_inventory`.
- Does not post accounting entries; valuation method/cost are inputs to inventory layers.
