# Inventory (`erp_inventory`)

## Purpose

Append-only stock ledger: warehouses, draft/post moves, layers, allocations, and reservations. Quantity and value live in one chain — no mutable quants, no stock-repost job. App route `/erp/inventory`.

## Dependencies

`erp_item`, `company`, `uom`, `currency`

Soft call to `gl.list_periods` when accounting is installed (closed period refuses post). Not a hard dependency.

## Models

| Model | Role |
|---|---|
| `stock_warehouse` | Company warehouse tree; `kind` `stock` / `view` / `transit` / `scrap` (holding: stock, transit) |
| `stock_move` | Header: `kind`, `state` (`draft` / `posted` / `void`), posting date, currency, source/idempotency, `reverses` |
| `stock_move_line` | Item, qty, warehouse(s), lot, unit cost / value after post |
| `stock_layer` | Open-layer cache: `qty_in` / `value_in`, `remaining_*`, `unit_cost`, method, `seq` |
| `stock_layer_alloc` | Append-only qty/value taken from a layer by a move line |
| `stock_reservation` | Soft hold: demand `(kind, id)`, qty (release = negative-sense rows via API) |
| `stock_counter` | Per company/prefix/year document numbers |

Move kinds: `receipt`, `issue`, `transfer`, `adjust`, `manufacture_consume`, `manufacture_produce`, `void`.

## Design

- **Immutable posts:** a posted move never changes; mistakes are a void reversing move (`stock_voided`).
- **Layers:** receipt opens a layer (FIFO/standard) or tops up one pooled average layer. Issue walks open layers; insufficient stock refused (no negatives). Allocations are append-only; `remaining_*` on the layer updates in place with optimistic concurrency (re-read before commit, up to 5 attempts).
- **Valuation:** pure functions in `rules.rs` (`receive`, `receive_average`, `issue`, `standard_variance`). Method/standard cost from `erp_item.valuation_for`. Standard receipts store variance on the event payload for GL.
- **Reservations:** available = on-hand − reserved. Outbound posting may consume reservations when `source_kind` / `source_id` match a demand.
- **Idempotency:** draft by `idem_key` returns the existing move. Manufacture consume/produce post as a pair (`post_manufacture_pair`).
-Designed so stock history is never rewritten: no repost and no mutable on-hand rows.

## Functions, commands, events

**Warehouses:** `create_warehouse`, `set_warehouse`, `list_warehouses`

**Moves:** `draft_receipt`, `draft_issue`, `draft_transfer`, `draft_adjust`, `draft_manufacture_consume`, `draft_manufacture_produce`, `add_line`, `post_move`, `post_manufacture_pair`, `outbound_value`, `void_move`, `on_hand`, `get_move`

**Reservations:** `reserve`, `release`, `reserved`

**Emit:** `stock_posted`, `stock_voided` (payload includes inventory/COGS/variance amounts for `gl_erp`)

## Roles

| Role | Scope |
|---|---|
| `stock_admin` | Warehouses, void, full control |
| `stock_user` | Draft and post moves, reserve |
| `stock_reader` | On-hand and stock card |

## Boundaries

- Does not own the general ledger. Financial posting is `accounting/gl_erp` listening to stock events → `gl.on_posting_event`.
- Item master and valuation settings stay in `erp_item`. Buying/making/selling only call these handlers; they do not write layers themselves.
