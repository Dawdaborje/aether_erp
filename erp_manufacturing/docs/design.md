# Manufacturing (`erp_manufacturing`)

## Purpose

Bills of materials and manufacturing orders on the stock ledger. One MO: reserve materials, consume with a stock issue, produce with a stock receipt — no separate stock-entry document zoo. App route `/erp/manufacturing`.

## Dependencies

`erp_item`, `erp_inventory`, `company`, `uom`, `currency`

## Models

| Model | Role |
|---|---|
| `bom` / `bom_line` | Finished item + output qty; lines with `qty`, optional `scrap_pct`, `is_byproduct`, optional `sub_bom` (multi-level) |
| `work_center` | Company work center with `cost_per_hour` (labour and overhead absorbed) |
| `bom_operation` | Routing step on a BOM: work center, `setup_minutes`, `cycle_minutes` per output batch |
| `mo_operation` / `mo_time_log` | Order operations (snapshot at confirm) with planned/actual minutes, state and append-only time entries |
| `mo` | Order: BOM, qty, warehouse, state, links to `consume_move` / `produce_move` |
| `mo_component` | Snapshot of BOM lines at confirm |
| `mfg_counter` | Legacy: document numbers before the kernel numbered orders (`MO`); no longer written |

MO states: `draft` → `confirmed` → `reserved` → `in_progress` → `done` (or `cancelled`).

## Design

- **Confirm** copies BOM lines into `mo_component`. The MO number is a kernel naming series on `mo.number` (`MO-{YYYY}-{#####}`, yearly, per company) that numbers the order the moment its state first becomes `confirmed`: drafts have no number, a deleted draft leaves no gap, and confirming twice never takes a second number. Orders confirmed before this change keep their old number; the new series counts from 1, so seed or accept a repeat if both generations share a year.
- **Explosion** (`rules.rs`): material qty = `mo_qty / bom_output_qty * qty_per * (1 + scrap%/100)`; byproducts ignore scrap.
- **Reserve** calls `erp_inventory.reserve` per material with demand `erp_manufacturing.mo` / mo id. Cancel from reserved releases holds.
- **Complete:** draft `manufacture_consume` + `manufacture_produce` (idempotent keys), `outbound_value` on consume → FG unit cost = consume value / mo qty; byproducts at zero cost; `post_manufacture_pair`; then release reservations if any. Emits `mo_completed`.
- Cannot complete twice while move links exist; void stock moves before retrying. Cancel refuses `in_progress` until moves are cleared.

## Functions, commands, events

**Handlers:** `create_bom`, `add_bom_line`, `create_mo`, `confirm_mo`, `reserve_mo`, `complete_mo`, `cancel_mo`, `get_mo`, `get_bom`

**Routing:** `create_work_center`, `set_work_center`, `add_bom_operation`, `start_operation`, `log_time`, `finish_operation`

**Multi-level:** `explode_bom`, `rollup_cost`, `create_child_mos`

**Unbuild:** `unbuild_mo`

**Emit:** `mo_completed` (`mo`, qty, consume/produce move ids, `consume_value`, `labour_cost`)

## Routing, labour and multi-level BOMs

- **Operations:** confirm copies the BOM's routing to `mo_operation` rows with planned minutes `setup + cycle × (mo qty / BOM qty)` and the work center's hourly rate frozen. Operations run in sequence (`start_operation` → `log_time` → `finish_operation`).
- **Labour absorption:** `complete_mo` needs every operation finished. Labour = Σ minutes × rate / 60 (planned minutes when none were logged). Finished goods are received at `(consume value + labour) / qty`; `mo_completed` carries `labour_cost` so `gl_erp` can clear the labour part of WIP.
- **Multi-level:** `bom_line.sub_bom` makes a component from another BOM. `explode_bom` flattens the tree (raw materials summed, cycles refused, depth ≤ 8); `rollup_cost` prices raw materials from standard cost (or supplied costs) plus operations of every BOM in the tree; `create_child_mos` opens one child order per sub-assembly, and the parent cannot complete until its children are done.

## Unbuild

`unbuild_mo` takes finished goods of a **done** order (all of it, or `qty` up to what is left) back out of stock and returns the components. The finished goods leave as a `manufacture_consume` at their layer cost; the components return as a `manufacture_produce` at that value shared in proportion to what the order consumed, so work in progress nets to zero. Each unbuild is an append-only `mo_unbuild` row. Byproducts and absorbed labour are not reversed.

## Roles

| Role | Scope |
|---|---|
| `mfg_admin` | BOMs and orders |
| `mfg_user` | Confirm, reserve, complete |
| `mfg_reader` | Read BOMs and orders |

## Boundaries

- All stock quantity/value changes go through `erp_inventory` moves only.
- No capacity or finite scheduling, parallel operations, or partial/batch completion of one order (an order produces once).
- GL for consume/produce comes from inventory `stock_posted` via `gl_erp`, not from this plugin.
