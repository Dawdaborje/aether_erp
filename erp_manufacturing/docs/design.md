# Manufacturing (`erp_manufacturing`)

## Purpose

Bills of materials and manufacturing orders on the stock ledger. One MO: reserve materials, consume with a stock issue, produce with a stock receipt — no separate stock-entry document zoo. App route `/erp/manufacturing`.

## Dependencies

`erp_item`, `erp_inventory`, `company`, `uom`, `currency`

## Models

| Model | Role |
|---|---|
| `bom` / `bom_line` | Finished item + output qty; lines with `qty`, optional `scrap_pct`, `is_byproduct` |
| `mo` | Order: BOM, qty, warehouse, state, links to `consume_move` / `produce_move` |
| `mo_component` | Snapshot of BOM lines at confirm |
| `mfg_counter` | Document numbers (`MO`) |

MO states: `draft` → `confirmed` → `reserved` → `in_progress` → `done` (or `cancelled`).

## Design

- **Confirm** copies BOM lines into `mo_component` and allocates an MO number.
- **Explosion** (`rules.rs`): material qty = `mo_qty / bom_output_qty * qty_per * (1 + scrap%/100)`; byproducts ignore scrap.
- **Reserve** calls `erp_inventory.reserve` per material with demand `erp_manufacturing.mo` / mo id. Cancel from reserved releases holds.
- **Complete:** draft `manufacture_consume` + `manufacture_produce` (idempotent keys), `outbound_value` on consume → FG unit cost = consume value / mo qty; byproducts at zero cost; `post_manufacture_pair`; then release reservations if any. Emits `mo_completed`.
- Cannot complete twice while move links exist; void stock moves before retrying. Cancel refuses `in_progress` until moves are cleared.

## Functions, commands, events

**Handlers:** `create_bom`, `add_bom_line`, `create_mo`, `confirm_mo`, `reserve_mo`, `complete_mo`, `cancel_mo`, `get_mo`, `get_bom`

**Emit:** `mo_completed` (`mo`, qty, consume/produce move ids, `consume_value`)

## Roles

| Role | Scope |
|---|---|
| `mfg_admin` | BOMs and orders |
| `mfg_user` | Confirm, reserve, complete |
| `mfg_reader` | Read BOMs and orders |

## Boundaries

- All stock quantity/value changes go through `erp_inventory` moves only.
- No work centers, routings, or capacity planning in v0.1.
- GL for consume/produce comes from inventory `stock_posted` via `gl_erp`, not from this plugin.
