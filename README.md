# Aether ERP

Items, an append-only stock ledger, then buying, making, and selling — **Rust → WASM** plugins
prefixed `erp_`, with deliberately stricter immutability on stock (no editable on-hand rows, no
stock-repost).

## Features

- Item master with per-company valuation (`fifo` / `average` / `standard`), dated standard cost, lot and serial tracking, shelf life, supplier / customer item codes, per-item unit conversion
- Pricing: taxes (percent / fixed, inclusive / exclusive), payment terms, buy and sell price lists with quantity breaks
- Stock ledger: warehouses, moves, layers, allocations, reservations, lots with expiry (FEFO), physical counts, landed cost
- Procurement: material requests (approval workflow) → RFQ compare / award → PO (approval limit) → receive → bill → three-way match, supplier returns, debit notes
- Selling: quotation → SO (credit limit) → deliver → invoice, customer returns, credit notes (optional create from CRM deal)
- Manufacturing: multi-level BOM, routing and work centers, MO with labour absorption, child orders for sub-assemblies
- Replenishment: reorder rules and sales shortfall → draft purchase or manufacturing orders
- Scrap write-offs and unbuild of completed manufacturing orders; order lines in a purchase / sales unit (a case)
- Quality: templates with numeric and pass / fail parameters, inspections with recorded readings, an `inspection_status` gate
- Warehouse: bins in a tree, putaway rules, an append-only bin ledger, pick lists from sales orders, shipments with packages
- Reports: net sales and purchases by month or party, open orders by state, late manufacturing orders, export to Excel / PDF
- Bulk import / export of categories, items and price list rows (dry run first, undoable); opening stock from a CSV posted through the ledger
- Point of sale: sessions, carts, cash / card payment that issues stock in the same step, drawer count at close
- Alerts: a scheduled morning digest per subscriber, built with that person's own access
- Screens: dashboards, lists, create forms and line pages in every plugin

## Plugins

| Plugin | Role | Design |
|---|---|---|
| [`erp_item`](erp_item/) | Categories, SKUs, dated standard cost, per-company valuation | [docs/design.md](erp_item/docs/design.md) |
| [`erp_pricing`](erp_pricing/) | Taxes, payment terms, price lists, line calculator | [docs/design.md](erp_pricing/docs/design.md) |
| [`erp_inventory`](erp_inventory/) | Warehouses + stock ledger (moves, layers, allocs, reservations), lots, counts, landed cost, reports | [docs/design.md](erp_inventory/docs/design.md) |
| [`erp_procurement`](erp_procurement/) | RFQ → PO → receive → bill → match / release-to-pay, returns, debit notes | [docs/design.md](erp_procurement/docs/design.md) |
| [`erp_manufacturing`](erp_manufacturing/) | BOM, routing, work centers, MO (reserve / consume / produce) | [docs/design.md](erp_manufacturing/docs/design.md) |
| [`erp_selling`](erp_selling/) | Quotation → SO → deliver → invoice, returns, credit notes, credit limit | [docs/design.md](erp_selling/docs/design.md) |
| [`erp_replenishment`](erp_replenishment/) | Reorder rules and shortfall → draft PO / MO | [docs/design.md](erp_replenishment/docs/design.md) |
| [`erp_warehouse`](erp_warehouse/) | Bins, putaway rules, pick lists and shipments | [docs/design.md](erp_warehouse/docs/design.md) |
| [`erp_quality`](erp_quality/) | Inspection templates, inspections and readings that accept or reject goods | [docs/design.md](erp_quality/docs/design.md) |
| [`erp_reports`](erp_reports/) | Read-only sales, purchase, open-order and manufacturing reports with Excel / PDF export | [docs/design.md](erp_reports/docs/design.md) |
| [`erp_alerts`](erp_alerts/) | Daily notification digest: expiring lots, late orders, overdue bills, open requests | [docs/design.md](erp_alerts/docs/design.md) |
| [`erp_pos`](erp_pos/) | Cashier sessions, carts priced from the price list, payment that issues stock, drawer count | [docs/design.md](erp_pos/docs/design.md) |

## Relation to other workspaces

| Workspace | How it fits |
|---|---|
| **base** | `company`, `uom`, `currency`, `party` — legal entity, units, money, suppliers/customers |
| **accounting** | General ledger stays in `gl`. Stock / bill / invoice events are turned into entries by [`gl_erp`](../accounting/gl_erp/) (not by ERP plugins) |
| **crm** | Pipeline and customers live in CRM; `erp_selling` can open an SO from a deal (`crm_deal` / `crm_customer`) |

ERP does not post the books. Inventory emits `stock_posted` / `stock_voided` / `stock_revalued`;
procurement emits `purchase_bill_approved` / `purchase_debit_note_approved`; selling emits
`sales_invoice_approved` / `sales_credit_note_approved`; manufacturing emits `mo_completed`. Install
`gl_erp` when both accounting and ERP are in use.

## Not covered

Barcode scanning, sampling plans and corrective actions, carrier rates and labels, wave picking, subcontracting, blanket orders, product variants and kits, multi-tax and withholding,
finite-capacity scheduling, partial completion of one manufacturing order, forecast-driven planning,
inter-company.

## Load order

```text
base: currency → country → party → company → uom
erp_item → erp_pricing
erp_item → erp_inventory
erp_inventory + erp_pricing → erp_procurement | erp_manufacturing
erp_inventory + erp_pricing + crm_customer + crm_deal → erp_selling
erp_procurement + erp_manufacturing + erp_selling → erp_replenishment
erp_item → erp_quality
erp_inventory + erp_item → erp_warehouse   # erp_selling optional

# optional GL bridge (accounting workspace)
gl → gl_erp   # after erp_inventory, erp_procurement, erp_selling, erp_manufacturing
```

`--sync-models` each plugin after its dependencies are in the catalog, then `--load-plugin` (or
`--install-plugin` / `--watch` as for other workspaces).
