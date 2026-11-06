# ERP: gaps against enterprise suites, and the improvement roadmap

Written 2026-11-01 from a read of the README, each plugin's `docs/design.md` and a sample of its pages, plus a
keyword search of the sources. It was **not** checked against a running instance. "README" marks items the
workspace README already lists under *Not covered*; the rest are inferred from searches that found nothing, so
confirm before relying on them. Compared against SAP S/4HANA and Business One, Oracle NetSuite, Dynamics 365
Business Central, Odoo and ERPNext.

Some gaps may be covered by sibling workspaces (`accounting`, `commerce`, `projects`, `fieldservice`,
`subscriptions`, `contracts`, `helpdesk`, `crm`, `prism`). Check there before building here.

## What is already strong

Append-only stock ledger with FIFO / average / standard valuation; lot and serial tracking with
first-expired-first-out picking; landed cost; RFQ → PO → receive → bill with three-way match; approval and
credit limits; bin ledger and pick lists; quality inspections that gate goods; undoable bulk import.

## User interface and experience

| # | Gap | Today | Target |
|---|---|---|---|
| U1 | Documents split across pages | A sales order is a list page, a separate "Order lines" page and a separate "Add line" page (`erp_selling/pages/sos.xml`, `so_lines.xml`, `so_lines_new.xml`); same for POs, invoices, quotations, bills, MOs | One document screen with an editable line grid, running totals, status tracker and a history of changes |
| U2 | Actions on list rows | Confirm / Reserve / Cancel are row actions | Header buttons showing only what the state allows; "Create invoice / delivery" carrying lines across |
| U3 | No visual views | No kanban, calendar or board views in the suite | Kanban for quotation → order and MOs by state; calendar for deliveries, due bills and production |
| U4 | Thin dashboards | Six landing pages, 4 to 7 numbers and at most one chart; no filters, comparison or drill-down | `board` dashboards with period and warehouse filters, period comparison, click-through, one per role |
| U5 | Reports are lists | `erp_reports` is lists plus Excel / PDF | Pivots, saved filters, scheduled emailed reports, drill-through |
| U6 | No translations | No `i18n` folder in any plugin | Translations for every plugin |
| U7 | Navigation | A row of buttons under the stats | Searchable menu with counts of what waits ("Sales orders (14 open)") |
| U8 | Point of sale | 642 lines; confirm touch layout, barcode field, receipt print, hold / recall, offline | All of these |

## Functional gaps

### Supply chain planning
- MRP: forecasts and orders become planned purchase and production orders across BOM levels (README: forecast-driven planning).
- Demand forecasting by item with seasonality; sales and operations planning.
- Finite-capacity scheduling and work-center load (README).
- Safety stock and reorder points calculated from lead time and demand variability.
- Available-to-promise: a delivery date quoted from stock, incoming orders and production.

### Procurement
- Blanket and scheduled orders; contracts with agreed price and volume (README).
- Subcontracting (README).
- Supplier scorecards (on-time, rejects, price variance) and approved-supplier lists.
- Drop-shipping and consignment stock.
- Supplier portal.
- Budget checks on requisitions.

### Sales
- Promotions, coupons, customer-specific agreements, rebates, tiered volume discounts.
- Product variants and configurator (README); kits and bundles (README).
- Back-order and partial-shipment management with allocation priority.
- Customer portal and e-commerce (check `commerce`).
- Subscription and rental billing (check `subscriptions`).
- Multi-tax and withholding (README); e-invoicing (Peppol, SDI, GST, CFDI).

### Inventory and warehouse
- Barcode and handheld scanning (README); RFID.
- Wave, batch and zone picking, cross-docking (README).
- Carrier rate shopping, labels and tracking (README).
- Cycle counting by ABC class; stock aging, turnover and slow-moving reports.
- Recall / traceability report: which customers received lot X.
- Returnable packaging; inter-warehouse replenishment.

### Manufacturing
- Partial completion of one manufacturing order (README).
- Shop-floor tablet screens: start / stop operations, report scrap, record quality readings.
- Co-products and by-products; BOM versions with effectivity dates and change orders.
- Maintenance management for machines.
- Costing: standard cost roll-up, price / usage / efficiency variances, overhead allocation.

### Quality
- Sampling plans, non-conformance reports, corrective and preventive actions (README).
- Certificates of analysis on lots; supplier quality feeding scorecards.

### Platform
- Configurable approval workflows (multi-step, conditional, delegated). Today: fixed purchase approval limit and credit limit.
- Audit trail of who changed what: **done for documents and master data** (see Progress, 2026-11-01); segregation-of-duties checks still open.
- Multi-currency on documents with exchange-rate handling; inter-company transactions (README).
- Attachments, email and print templates; PDF quotes, POs, invoices and delivery notes.
- EDI, webhooks and carrier / bank / marketplace connectors.
- Localization packs: country tax rules, statutory reports, legal numbering.

### Adjacent modules enterprise suites bundle
Project accounting (`projects`), field service and repair (`fieldservice`), helpdesk and warranty claims
(`helpdesk`), fixed assets (`accounting`), business intelligence (`prism`). Decide per module whether it
belongs here or stays elsewhere.

## Roadmap

Ranked by how often a typical buyer calls the gap a deal-breaker, then by cost.

| Order | Work | Status |
|---|---|---|
| 1 | Dashboards on the `board` view: sales, procurement, inventory (U4) | Done for sales, procurement, inventory, manufacturing, point of sale and replenishment. Role-specific boards (buyer, planner, warehouse lead, cashier) still to do |
| 2 | Single-page documents with editable lines (U1, U2) | Done for the sales order, purchase order, quotation, sales invoice, purchase bill and manufacturing order. Lines are edited in a dialog, not inline: a true inline grid needs a new widget in `aether/web` |
| 3 | Audit trail; print and email templates | Audit trail done for documents and master data; print and email done in a basic form (see log); letterhead layouts and attachments not possible yet |
| 4 | Inventory analytics: aging, ABC, turnover; cycle counts | Analytics board done (ABC, turnover, slow stock, counts due); aging was already on the overview; creating a count from the due list not done |
| 5 | Barcode scanning (also lifts POS and warehouse) | Not started |
| 6 | Translations (U6) | Not started |
| 7 | Configurable approval workflows; multi-currency | Not started |
| 8 | MRP and forecasting; shop-floor screens | Not started |
| 9 | Pricing and promotions; variants and kits; subcontracting; blanket orders | Not started |

Which of these matter most depends on the customer: distributors and retailers lean on 1 to 5 plus variants and
kits; manufacturers lean on 8.

## Progress

Update this section as work lands.

- 2026-11-01: this document written.
- 2026-11-01: **Overview boards for sales, purchasing and inventory** (roadmap 1, part of U4).
  - Pages: `/erp/selling/overview`, `/erp/procurement/overview`, `/erp/inventory/overview`. Each is a `board` view with a period filter and click-through from every chart to the records behind it.
  - Sales and purchasing: ordered and invoiced / billed with comparison against the period just before; open value today; month by month; top customers / suppliers; orders by state; quotation-to-cash funnel; orders waiting longest; quotations about to lapse; orders and bills needing a decision; received-not-billed. One currency at a time (a currency filter), never added together.
  - Inventory: stock value now, by warehouse and category; **stock aging** by band (also a roadmap item under *Inventory and warehouse*); lots expired or expiring; stock in and out by month. Company filter.
  - Code: `src/board.rs` in each plugin, on a shared-by-copy `src/board_kit.rs` (plugins cannot share code, so keep the three copies identical).
  - Checked against a scratch instance with seeded orders, bills, receipts, lots and quotations: the figures on each board reconcile with each other and with the source documents; filters, click-through and role access behave. **Not checked in a browser**, so layout is unseen.
  - Known limits: boards read at most 5000 orders / bills and 20,000 stock layers; money is shown as a float for charts (the books stay exact); amounts show in the company currency without conversion.
  - Not yet done at the time: boards per role (buyer, planner, warehouse lead, cashier). The below-reorder list and the manufacturing and POS boards were added later the same day (see below).
- 2026-11-01: **Sales order as one document** (roadmap 2, U1 and U2, pilot).
  - `/erp/selling/sos/{id}` is now a single page: editable header (date, warehouse, payment term, price list, notes), totals, status tracker, and a Lines tab with add / edit (dialog) / remove. It replaces the old "Order lines" and "Add line" pages.
  - Header buttons appear only for the states that allow them: Confirm (draft), Reserve stock / Deliver all (confirmed, partly delivered), Invoice delivered, Cancel order.
  - New functions in `erp_selling/src/orders.rs`: `update_so`, `get_so_doc`, `get_so_line`, `update_so_line`, `remove_so_line`, `deliver_all_so`, `invoice_all_so`. Header and lines can be changed only while the order is a draft; totals are recomputed on every change.
  - Checked on a scratch instance through the API: add, edit, remove, header edit, confirm, edits refused after confirming, deliver all, invoice all, a second invoice refused, a read-only user refused. **Not checked in a browser**: the page layout, the dialog and the button visibility rules are unseen.
  - Known limits: lines are not edited inline in a grid; "Deliver all" and "Invoice delivered" take defaults (no lot, date or price choice; use the existing functions for partial or lot-specific deliveries); header buttons show to read-only users, and the function refuses them.
- 2026-11-01: **Purchase order as one document** (roadmap 2, second document).
  - `/erp/procurement/pos/{id}` mirrors the sales order page: editable header, totals, status tracker (including *to approve*), Lines tab with add / edit / remove in a dialog. It replaces the old "Order lines" and "Add line" pages.
  - Buttons by state: Confirm (draft), Approve / Send back (to approve), Receive all, Bill received, Cancel order.
  - New functions in `erp_procurement/src/orders.rs`: `update_po`, `get_po_doc`, `get_po_line`, `update_po_line`, `remove_po_line`, `receive_all_po`, `bill_all_po`. "Bill received" bills what was received, up to what was ordered (corrected 2026-11-01: an earlier version of this note and of the function also allowed billing the ordered quantity under the match-on-ordered policy, which `create_bill` refuses).
  - Checked through the API on a scratch instance (same cases as the sales order, plus a read-only buyer refused). Not checked in a browser. Not tested: the *to approve* path from this page, lot-tracked items, and an approval limit.
  - Same limits as the sales order: dialog rather than inline grid; "Receive all" and "Bill received" use defaults; buttons show to read-only users and the functions refuse them.
- 2026-11-01: **Quotation as one document** (roadmap 2, third document).
  - `/erp/selling/quotations/{id}` has an editable header (dates, valid until, warehouse, terms, price list, notes), totals, tracker and a Lines tab with add / edit / remove in a dialog, including the *optional add-on* flag. It replaces the old "Quotation lines" and "Add quotation line" pages.
  - Buttons by state: Send (draft), Accepted, Make order, **Make order with optional lines** (new), Rejected.
  - New functions in `erp_selling/src/quotes.rs`: `update_quotation`, `get_quotation_doc`, `get_quotation_line`, `update_quotation_line`, `remove_quotation_line`, `convert_quotation_with_optional`.
  - Checked through the API on a scratch instance: edits and removal with totals that leave optional lines out, edits refused after sending, conversion with optional lines carrying all three lines and the quoted prices to the order, read-only user refused. Not checked in a browser.
  - Note: editing a line gives it a new id (the new line is built first, so a failed edit changes nothing). Anything holding a line id across an edit would lose it; nothing in the suite does today.
  - Same limits as the other documents.
- 2026-11-01: **Sales invoice as one document** (roadmap 2, fourth document).
  - `/erp/selling/invoices/{id}` shows the invoice with its tracker, totals and lines. While the invoice is a draft you can change the dates, change a line's quantity or price, remove a line, add a line from the order's delivered-but-uninvoiced quantities, approve it, or **discard** the draft. Approved invoices and credit notes are read-only.
  - Different from the orders: an invoice line is tied to an order line's `qty_invoiced`, so every change moves that counter. Quantity can go up to what is delivered and not invoiced elsewhere; removing a line or discarding the draft gives the quantity back so it can be invoiced again. A discarded draft becomes `cancelled`; the last line cannot be removed (use Discard).
  - New functions in `erp_selling/src/orders.rs`: `update_invoice`, `get_invoice_doc`, `get_invoice_line`, `update_invoice_line`, `add_invoice_line`, `list_invoiceable_lines`, `remove_invoice_line`, `discard_invoice`.
  - Checked through the API on a scratch instance: the order's invoiced counters reconcile after every add, edit, remove and discard; over-invoicing refused; edits refused on approved and discarded invoices; read-only user refused. Boards count approved invoices only, so discards do not leak into them. Not checked in a browser.
  - Not done: a credit-note view with its own actions, and cancelling an *approved* invoice (needs a credit note, as the books are already told).
- 2026-11-01: **Purchase bill as one document** (roadmap 2, fifth document) and a fix to the purchase order's "Bill received".
  - `/erp/procurement/bills/{id}` works like the sales invoice page, plus the three-way match: the page shows *release to pay* and each line's *release* badge, recomputed after every change (a price that differs from the order turns a line to *exception*; putting it back clears it).
  - Draft bills only: change dates, change a line's quantity or price, add a line from received-but-unbilled order lines, remove a line, **discard** the draft (state `cancelled`). Approved bills and debit notes are read-only. The last line cannot be removed (use Discard).
  - Each change keeps three things in step: the bill line, its `purchase_match` record, and the order line's `qty_billed`. Quantity is capped at what was received, up to what was ordered, the same cap `create_bill` uses.
  - New functions in `erp_procurement/src/orders.rs`: `update_bill`, `get_bill_doc`, `get_bill_line`, `update_bill_line`, `add_bill_line`, `list_billable_lines`, `remove_bill_line`, `discard_bill`.
  - **Fix:** `bill_all_po` (the purchase order's *Bill received* button) used the ordered quantity under the match-on-ordered policy, which `create_bill` rejects; it now uses the same cap as `create_bill`.
  - Checked through the API on a scratch instance: billed quantities on the order reconcile after each add, edit, remove and discard; over-billing refused; price variance flips the match status and back; edits refused on approved and discarded bills; read-only buyer refused; the purchasing board ignores cancelled bills. Not checked in a browser. Not tested: a company with the match-on-ordered policy, and returns against a bill that was edited.
  - Not done: debit-note view; cancelling an approved bill (needs a debit note).
- 2026-11-01: **Manufacturing order as one document** (roadmap 2, sixth and last of the set).
  - `/erp/manufacturing/mos/{id}` replaces the old operations-only page. A manufacturing order has no priced lines, so the page is shaped differently: header (quantity, warehouse, planned date editable while the order is a draft), tracker, and three tabs.
    - **Materials**: what the order needs and any by-product it makes, at its quantity, with **stock cover** (on hand, available, and an ok / short badge) while the order is open. Once reserved, cover is judged on on-hand stock because the order's own hold is already out of "available".
    - **Operations**: the routing with Start / Finish row actions, and a **Log time** dialog (a picker of running operations, or click a row to log against it).
    - **Sub-orders**: orders planned for sub-assemblies, each opening its own page.
  - Buttons by state: Confirm (draft), Sub-assemblies (draft / confirmed), Reserve materials (confirmed), Complete, Unbuild (done), Cancel order.
  - New in `erp_manufacturing/src/document.rs`: `get_mo_doc`, `update_mo`, `list_mo_materials`, `list_child_mos`, `list_open_operations`, `get_mo_operation`, `log_operation_time`. `routing::LogTime` and `do_log_time` became crate-visible so the dialog can reuse them (a plugin cannot call itself through `plugins::call`).
  - The quantity is editable in draft only: confirming copies the materials and operations at that quantity.
  - Checked through the API on a scratch instance: header edit and refusal after confirming; materials with cover for an order that can and one that cannot be covered; reserve, start, log time (both ways), finish, complete (labour cost 12.50), unbuild; read-only user refused. Not checked in a browser. **Not tested:** the *Sub-assemblies* path and the Sub-orders tab with real rows (no multi-level BOM was seeded), partial completion (not supported by the plugin), and a lot-tracked component.
  - With this, every document in roadmap item 2 has a single page. Still open under item 2: an inline editable grid (needs a widget in `aether/web`), and the credit-note / debit-note views.
- 2026-11-01: **Manufacturing, point-of-sale and replenishment boards** (roadmap 1, finishing the set).
  - Pages: `/erp/manufacturing/overview`, `/erp/pos/overview`, `/erp/replenishment/overview`. Same `board` view, `src/board.rs` on the shared-by-copy `src/board_kit.rs` (now five identical copies; keep them identical).
  - **Manufacturing**: orders planned / completed / units / labour cost against the period before; routing efficiency (planned / logged minutes, shown only once time is logged); open orders, units in progress, late orders, drafts and running operations today; units by planned month; orders by state; planned minutes still to do by work centre; tables of late orders and running operations. Company filter. Orders are placed in time by their *planned date* (an order has no other date), so the default last-12-months period leaves out orders planned for later; widen the period to see them.
  - **Point of sale**: takings and average basket, receipts and voids against the period before; taken today, open sessions and their cash; takings by month and by till; payments by method; best sellers (net of tax); tables of open sessions and of closed sessions whose cash did not match. Currency filter. Open-session cash is worked out from the payments (a session only stores it at close). Clicking a month, till, method or item lists the receipts behind it.
  - **Replenishment**: a dry run of the reorder check (rules below their minimum counting stock available plus what is already on order or being made, the quantity it would order, and whether a request already exists), open requests by route and reason, and the most urgent open requests. Nothing is created from the board. Company filter. Reads at most 200 rules at once.
  - Checked on a scratch instance with seeded sessions, sales, payments, a void, manufacturing orders (one late, one running, one done) and three reorder rules: the POS figures reconcile (takings 228 = by till 192 + 36 = by method 198 + 30; best sellers match the lines; session cash 162 + 30 matches the payments); manufacturing figures match the orders and operations; replenishment flips from three untouched rules to three covered ones after the check; drill-downs return the right records; read-only users reach their own board and are refused on others.
  - **Not tested**: the POS table of cash differences (a session could not be closed on the scratch instance because a cart could not be paid for lack of stock), the manufacturing efficiency reading with logged time, the Sub-assemblies path, and any layout in a browser.
  - Known limits: POS and manufacturing read at most 20,000 sales / 5000 orders; POS tills are named, cashiers are not (only an account id is stored).
- 2026-11-01: **Audit trail** (roadmap 3, first half; print and email templates still to do).
  - Built on the kernel's two features rather than a table of our own: **chatter** (a timeline on the record page: who changed which tracked field from what to what, plus notes) and **history** (a version of every write, which can show a record as it was, and put it back).
  - Switched on in the model JSON (`chatter` with `track` on the fields that matter, and `history`):
    - Chatter and history: sales order, quotation, sales invoice, customer credit limit, sales settings; purchase order, purchase bill, purchase settings (approval limit, match policy); manufacturing order; item; price list; reorder rule; POS session.
    - History only: the line models (order, quotation, invoice, PO, bill lines), manufacturing operations, bill of materials and its lines, work centres, standard costs, price list items, taxes, warehouses, POS profiles.
  - **History tab** on the sales order, quotation, sales invoice, purchase order, purchase bill and manufacturing order pages: header and lines (operations for a manufacturing order) in one list, newest first (when, by, what, change). Built by `src/audit.rs` (copied into the three plugins, with `history::read` added to their capabilities) from `history::list` and `history::diff`, so it holds for every write whichever page or function made it. Figures that are only derived (subtotals, line amounts) are left out; the quantity, price and total that caused them are shown.
  - **Line edits keep the line's identity.** Editing a line used to build a new row and delete the old one, which would have restarted the line's history on every edit. It now moves the new values onto the existing line (`fold_line`), so the line's own history is continuous.
  - **Avoided `log_to`.** The kernel's `log_to` (a child record writing into its parent's timeline) looked ideal for line changes, but on the kernel build used here it makes `db::create` return the chatter row instead of the new record, which breaks any code that uses the created record (adding an order line, for one). The History tab does the job instead. Worth re-testing once the kernel's in-progress "parent chatter lines" work settles; `log_to` on the line models would then put line changes in the parent's chatter too.
  - Checked on a clean reinstall of the scratch instance with the full seed (every create-and-link flow ran with history and chatter on) and on an upgrade of an existing instance; sales order, invoice, PO, bill and manufacturing order histories read correctly after add / edit / remove line, header edits, confirm, deliver, invoice, receive, bill, approve and complete; item and PO chatter timelines return their entries. **Not checked in a browser.**
  - Limits:
    - Records that existed before the switch have no history from before it (the first change they see starts it).
    - A removed line is not listed in the History tab (its removal shows as the total changing).
    - The *By* column shows a short account reference, because plugins cannot look up account names; the chatter timeline on the same page names people.
    - The History tab shows at most 200 rows (30 versions per record, 100 lines).
    - Restore / undo exist in the kernel for these models but nothing in the ERP pages offers them yet.
    - Not covered: POS sales and payments, stock moves (already an append-only ledger), receipts and deliveries, which are created once and not edited.

- **Print and email (roadmap 3, second half).** `src/print.rs` in `erp_selling` and `erp_procurement`.
  - Download PDF: quotation, sales order, sales invoice, purchase order, purchase bill. Header button, shown once the document has a number (not draft). The PDF is a table from the kernel's `report::export`: customer/supplier, date, lines, subtotal, tax, total. There is no letterhead, logo, address block or terms text, because the platform only offers a table export (max 1000 rows).
  - Email to customer/supplier: quotation, sales order, sales invoice, purchase order. The figures go in the message body (text and HTML) because the platform's email has no attachments. Sent through `communication::send`; the sender and provider are set by an administrator.
  - Tested on the scratch instance: PDFs download for all five documents; a missing customer address and a missing mail provider each give a clear error and send nothing. No real email was sent.
  - Needs: capabilities `report::export`, `communication::send`, `email::send` (added to both plugins).
  - Not done: delivery note PDF, per-company templates, attachments, a log of what was emailed.

- **Inventory analytics (roadmap 4).** New board *Analytics* in `erp_inventory` (`src/analytics.rs`, `pages/analytics.xml`, route `/erp/inventory/analytics`, handlers `analytics_board` and `analytics_board_drill`). Company filter.
  - Turns per year and days of stock on hand; value of slow stock (no issue for 90 days) and of stock with no issue in a year.
  - ABC classes: items ranked by the cost of what left in the last 365 days (issues and production consumption); A is the top 80%, B the next 15%, C the rest, and held items with no usage are shown as *No movement*. The bar shows stock value per class; clicking it lists the items with usage, turns and last issue.
  - Turns by category, a slow-moving table, and a *Due for a cycle count* table (A every 90 days, B every 180, others every 365, from the last posted count; never-counted first).
  - Checked on the scratch instance: the class values add up to the overview's stock value (15,803.00), and so do the drill-down rows.
  - Limits: turnover uses today's stock value, not an average, because no daily stock history is kept; the board reads up to 50,000 posted moves and 100,000 move lines; the due list does not create a count sheet yet (counts need a counted quantity per line).
