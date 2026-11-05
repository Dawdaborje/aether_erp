# Replenishment (`erp_replenishment`)

## Purpose

Close the loop from low stock or unmet demand to a purchase or manufacturing order. Min/max reorder rules, demand shortfall from sales orders, and one-step conversion of a request into a draft PO or MO. App route `/erp/replenishment`.

## Dependencies

`erp_item`, `erp_inventory`, `erp_procurement`, `erp_manufacturing`, `erp_selling`, `company`, `party`

## Models

| Model | Role |
|---|---|
| `reorder_rule` | One per `(company, item, warehouse)`: `min_qty`, `max_qty`, order `multiple`, `route` (`buy` / `make`), default `supplier` / `bom`, `lead_days` |
| `replenish_request` | Shortfall to cover: `qty`, `route`, `state` (`open` / `converted` / `cancelled`), `reason` (`reorder` / `demand` / `manual`), demand reference, result (`po` / `mo` + id) |

## Design

- **Projection:** `available` (on hand − reserved, from `erp_inventory.on_hand`) + **incoming** (open PO lines not yet received into that warehouse, plus open MOs). Below `min` the order brings the projection to `max`, rounded up to `multiple` (`rules.rs`, pure).
- **No double ordering:** a rule with an open request is skipped; ordered-but-unreceived quantity counts as incoming.
- **Demand:** `request_for_so` compares each open SO line with free stock plus incoming and opens a request for the shortfall, keyed to the SO line.
- **Convert:** buy → `create_po` + `add_po_line` (priced from the buy price list unless `unit_price` is given); make → `create_mo` from the rule's BOM or the item's active BOM. Orders are left as drafts for a buyer / planner to confirm.
- **Roles:** the plugin runs as the caller on other plugins. Evaluating needs stock, purchase, manufacturing and sales read access; converting needs `purchase_user` / `mfg_user`.

## Functions

`create_rule`, `set_rule`, `evaluate`, `request_for_so`, `list_requests`, `convert_request`, `cancel_request`

## Boundaries

- Evaluation is on demand (no schedule yet); call `evaluate` from a scheduled job or the UI.
- No forecast, safety stock by season, supplier lead-time scheduling, or multi-level (sub-assembly) planning.
