# Reports (`erp_reports`)

## Purpose

Cross-plugin figures that no single ERP plugin owns: sales and purchases over time, open orders, manufacturing load. Read-only. App route `/erp/reports`.

## Dependencies

`erp_item`, `erp_inventory`, `erp_procurement`, `erp_manufacturing`, `erp_selling`, `company`, `party`

## Models

None. The plugin owns no data; every figure is a `report::query` over a dependency's model (`erp_selling.sales_invoice`, `erp_procurement.purchase_bill`, `erp_selling.so`, `erp_procurement.po`, `erp_manufacturing.mo`).

## Design

- **Caller's rules apply:** the kernel filters each queried model by the caller's own record rules and hidden fields, so a report never shows more than the person could read in the owning plugin. The role `reports_reader` only opens the screens.
- **Sales / purchases:** approved documents only. `amount` is invoices (bills), `notes` credit (debit) notes, `net = amount − notes`, grouped by month of the document date or by party. Filter by `company` and a kernel `period` (`this_month`, `last_quarter`, ...).
- **Order status:** sales and purchase orders by `state`, with count and value.
- **Manufacturing:** orders by state; `late` counts orders past `planned_date` that are not `done` or `cancelled`.
- **Export:** `export_report` renders the sales or purchase report as `xlsx` (default) or `pdf` through `report::export`.
- Stock balance, ageing and ledger stay in `erp_inventory`.

## Functions

`sales_report`, `purchase_report`, `order_status`, `sales_orders_by_state`, `purchase_orders_by_state`, `mfg_report`, `mfg_by_state`, `reports_dashboard`, `export_report` (function only; no page button, pages cannot download a result yet)

## Boundaries

- Party columns show the party id; names are not resolved.
- No per-item sales analysis: invoice lines carry no date or state of their own, so it needs a join the report command does not offer.
- No saved reports or scheduled delivery (the kernel's saved-report commands are a proposal, not built).
- Amounts are summed per document currency as stored; no conversion across currencies.
