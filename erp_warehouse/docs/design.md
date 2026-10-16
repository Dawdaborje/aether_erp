# Warehouse (`erp_warehouse`)

## Purpose

Say *where* stock sits inside a warehouse and drive the physical flow: put received goods in a bin,
move between bins, pick for a sales order, and hand packages to a carrier. Valuation, layers and the
stock ledger stay in `erp_inventory`.

## Dependencies

`erp_item`, `erp_inventory`, `company`. **Optional:** `erp_selling` (pick lists from a sales order).

## Models

| Model | Role |
|---|---|
| `wh_bin` | Bin in a warehouse: `code` (unique per warehouse), `kind` (`storage` / `receiving` / `shipping` / `quarantine`), `parent` (a tree: shelf → rack) |
| `putaway_rule` | Where an item (or an item category) goes in a warehouse, by `priority` (lower first) |
| `bin_move` | Append-only movement: `from_bin` and / or `to_bin`, item, lot, qty, reason (`putaway` / `transfer` / `pick` / `adjust`) |
| `pick_list` / `pick_line` | Numbered `PK-{YYYY}-{#####}` when released, per company; lines say item, bin, qty, qty picked |
| `shipment` / `shipment_package` | Numbered `SH-{YYYY}-{#####}` when shipped; carrier, tracking, packages with weight |

## Design

- **Ledger, not balances:** on hand in a bin is what entered minus what left, summed by the database. A move is never edited or deleted (the rules give nobody `write` or `delete` on `bin_move`); a mistake is a move the other way. A bin cannot go negative through `move_between_bins`.
- **Putaway:** `suggest_bin` and `put_away` pick the matching active rule with the lowest rank: a rule for the item itself beats one for its category, then lowest `priority`.
- **Picking:** `create_pick_list` with `so` asks selling (optional dependency, `plugins::available`) for the open quantity of each line and takes it from the fullest bins first; what no bin covers becomes a line without a bin. `confirm_pick` records quantities and writes a `pick` move out of each line's bin, then moves the list to `picked`.
- **Kernel does the bookkeeping:** both numbers are naming series with `scope: company` and `assign_on` (drafts have no number); lines and packages are `child` fields written with their parent; `line_count` and `package_count` are computed; every state change is a plain write checked by the `workflow` in the rule file and recorded in chatter.
- **Bins are not the stock ledger:** nothing here posts to `erp_inventory`. Putting away into a bin does not receive stock; the receipt does that. Keep the two in step by putting away what the receipt received.

## Functions

**Bins and rules:** `create_bin`, `set_bin`, `list_bins`, `create_putaway_rule`, `list_putaway_rules`, `suggest_bin`
**Movements:** `put_away`, `move_between_bins`, `bin_stock`
**Picking:** `create_pick_list`, `release_pick_list`, `confirm_pick`, `cancel_pick_list`, `list_pick_lists`, `list_pick_lines`
**Shipping:** `create_shipment`, `add_package`, `ship_shipment`, `deliver_shipment`, `cancel_shipment`, `list_shipments`, `list_packages`

## Roles

`wh_reader`, `wh_user` (put away, move, pick, ship), `wh_admin` (bins and putaway rules).

## Boundaries

- No barcode scanning, wave or batch picking, or put-to-light.
- No carrier rate shopping or label printing; `tracking` is a typed value.
- The bin ledger is not reconciled against `erp_inventory` on-hand automatically.
