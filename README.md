# Aether ERP

Items, an append-only stock ledger, then buying, making, and selling — **Rust → WASM** plugins
prefixed `erp_`, with deliberately stricter immutability on stock (no editable on-hand rows, no
stock-repost).

## Features

- Item master with per-company valuation (`fifo` / `average` / `standard`) and dated standard cost
- Stock ledger: warehouses, moves, layers, allocations, reservations
- Procurement: RFQ → PO → receive → bill → three-way match / release-to-pay
- Manufacturing: BOM + MO (reserve / consume / produce on the same ledger)
- Selling: SO → deliver → invoice (optional create from CRM deal)

## Plugins

| Plugin | Role | Design |
|---|---|---|
| [`erp_item`](erp_item/) | Categories, SKUs, dated standard cost, per-company valuation | [docs/design.md](erp_item/docs/design.md) |
| [`erp_inventory`](erp_inventory/) | Warehouses + stock ledger (moves, layers, allocs, reservations) | [docs/design.md](erp_inventory/docs/design.md) |
| [`erp_procurement`](erp_procurement/) | RFQ → PO → receive → bill → match / release-to-pay | [docs/design.md](erp_procurement/docs/design.md) |
| [`erp_manufacturing`](erp_manufacturing/) | BOM + MO (reserve / consume / produce) | [docs/design.md](erp_manufacturing/docs/design.md) |
| [`erp_selling`](erp_selling/) | SO → deliver → invoice | [docs/design.md](erp_selling/docs/design.md) |

## Relation to other workspaces

| Workspace | How it fits |
|---|---|
| **base** | `company`, `uom`, `currency`, `party` — legal entity, units, money, suppliers/customers |
| **accounting** | General ledger stays in `gl`. Stock / bill / invoice events are turned into entries by [`gl_erp`](../accounting/gl_erp/) (not by ERP plugins) |
| **crm** | Pipeline and customers live in CRM; `erp_selling` can open an SO from a deal (`crm_deal` / `crm_customer`) |

ERP does not post the books. Inventory emits `stock_posted` / `stock_voided`; procurement emits
`purchase_bill_approved`; selling emits `sales_invoice_approved`. Install `gl_erp` when both
accounting and ERP are in use.

## Load order

```text
base: currency → country → party → company → uom
erp_item → erp_inventory
erp_inventory → erp_procurement | erp_manufacturing
erp_inventory + crm_customer + crm_deal → erp_selling

# optional GL bridge (accounting workspace)
gl → gl_erp   # after erp_inventory, erp_procurement, erp_selling
```

`--sync-models` each plugin after its dependencies are in the catalog, then `--load-plugin` (or
`--install-plugin` / `--watch` as for other workspaces).
