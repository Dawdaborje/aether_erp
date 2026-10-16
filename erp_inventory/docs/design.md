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
| `stock_lot` | Lot / serial master per `(company, item, code)`: `expiry_date`, `mfg_date` |
| `stock_count` / `stock_count_line` | Physical count: counted vs system qty, gains and losses posted as adjustments |
| `stock_landed_cost` / `stock_landed_charge` / `stock_landed_alloc` | Freight/duty added to a posted receipt; append-only allocation per receipt line |

Move kinds: `receipt`, `issue`, `transfer`, `adjust`, `manufacture_consume`, `manufacture_produce`, `void`.

## Design

- **Immutable posts:** a posted move never changes; mistakes are a void reversing move (`stock_voided`).
- **Layers:** receipt opens a layer (FIFO/standard) or tops up one pooled average layer. Issue walks open layers; insufficient stock refused (no negatives). Allocations are append-only; `remaining_*` on the layer updates in place with optimistic concurrency (re-read before commit, up to 5 attempts).
- **Valuation:** pure functions in `rules.rs` (`receive`, `receive_average`, `issue`, `standard_variance`). Method/standard cost from `erp_item.valuation_for`. Standard receipts store variance on the event payload for GL.
- **Reservations:** available = on-hand − reserved. Outbound posting may consume reservations when `source_kind` / `source_id` match a demand.
- **Idempotency:** draft by `idem_key` returns the existing move. Manufacture consume/produce post as a pair (`post_manufacture_pair`).
- **Returns:** a move whose `source_kind` is `erp_procurement.return` (an issue) posts to GRNI clearing instead of COGS; `erp_selling.return` (a receipt) credits COGS instead of clearing. Same layers and valuation; only the event amounts are reclassified.
- Designed so stock history is never rewritten: no repost and no mutable on-hand rows.

## Tracking, counts and landed cost

- **Serials:** `tracking = serial` items take one line per unit, with the serial as the lot; a serial that still has quantity on hand cannot be received again. Lots and serials get a `stock_lot` master row on first receipt; the expiry is the line's `expiry_date`, else the item's `expiry_days` from the move date.
- **Expiry:** an expired lot cannot be issued or consumed. `suggest_lots` returns on-hand lots and a first-expired-first-out picking plan; `expiring_lots` lists what expires within a window.
- **Counts:** `post_count` compares counted with on-hand per `(item, lot)` and posts gains and losses as two adjustments (gains cost from the line, else the newest layer, else standard). Uncounted items are untouched.
- **Landed cost:** `post_landed_cost` shares the charges over the receipt's lines (by value or qty), adds the part still on hand to the open layers' value and unit cost, and takes the part for goods already issued to cost of sales (standard-cost layers take it as variance). It emits `stock_revalued`. A receipt with posted landed cost cannot be voided. History rows (`value_in`, line values) are not rewritten.

## Scrap

`scrap_stock` writes damaged or spoiled stock off: an `adjust` move that removes stock, with `source_kind` `erp_inventory.scrap` and a required reason. It values the removal from the layers like any issue, may remove an expired lot (the main use), and the event reaches `gl_erp` as an adjustment loss. Retry with the same `idem_key` to avoid a second write-off.

## Functions, commands, events

**Warehouses:** `create_warehouse`, `set_warehouse`, `list_warehouses`

**Moves:** `draft_receipt`, `draft_issue`, `draft_transfer`, `draft_adjust`, `draft_manufacture_consume`, `draft_manufacture_produce`, `add_line`, `post_move`, `post_manufacture_pair`, `outbound_value`, `void_move`, `on_hand`, `get_move`

**Reservations:** `reserve`, `release`, `reserved`

**Scrap:** `scrap_stock`

**Lots:** `set_lot`, `get_lot`, `suggest_lots`, `expiring_lots`

**Counts:** `create_count`, `set_count_line`, `post_count`, `cancel_count`

**Landed cost:** `create_landed_cost`, `add_landed_charge`, `post_landed_cost`, `get_landed_cost`

**Emit:** `stock_posted`, `stock_voided`, `stock_revalued` (payload includes inventory/COGS/variance amounts for `gl_erp`)

## Roles

| Role | Scope |
|---|---|
| `stock_admin` | Warehouses, void, full control |
| `stock_user` | Draft and post moves, reserve |
| `stock_reader` | On-hand and stock card |

## Boundaries

- Does not own the general ledger. Financial posting is `accounting/gl_erp` listening to stock events → `gl.on_posting_event`.
- Item master and valuation settings stay in `erp_item`. Buying/making/selling only call these handlers; they do not write layers themselves.
