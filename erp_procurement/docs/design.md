# Procurement (`erp_procurement`)

## Purpose

Buy path: RFQ → purchase order → receive into stock → vendor bill → three-way match / release-to-pay. Receipts post through `erp_inventory`; bills emit a ledger event. App route `/erp/procurement`.

## Dependencies

`erp_item`, `erp_inventory`, `erp_pricing`, `company`, `party`, `uom`, `currency`

## Models

| Model | Role |
|---|---|
| `material_request` / `material_request_line` | Internal request for items: number from a naming series, lines as a child field, states `draft` → `submitted` → `approved` → `ordered` (or `rejected` / `cancelled`) |
| `rfq` / `rfq_line` | Request for quotation; states `draft` → `sent` → `awarded` / `cancelled` |
| `po` / `po_line` | Purchase order; line `qty_received` / `qty_billed` denormalized |
| `purchase_receipt` / `purchase_receipt_line` | Receipt header linked to `stock_move` |
| `purchase_bill` / `purchase_bill_line` | Vendor bill; `release_to_pay` / line `can_be_paid` |
| `purchase_match` | Append-only match rows (`kind` `receive` / `bill` / `return`) — source of truth for matched qty |
| `purchase_return` / `purchase_return_line` | Goods sent back to the supplier, linked to the outbound `stock_move` |
| `purchase_setting` | Per company: PO approval limit, over-receipt tolerance, bill match basis |
| `purchase_counter` | Document numbers (`RFQ`, `PO`, `PR`, `BILL`, `RET`, `DN`) |

PO states: `draft`, `to_approve`, `confirmed`, `partially_received`, `received`, `closed`, `cancelled`.

## Design

- **Spine:** confirm PO → `receive_po` drafts/posts an inventory receipt (`source_kind` `erp_procurement.po`) and writes `purchase_match` receive rows → `create_bill` / `approve_bill` with bill match rows.
- **Match rows, not flags:** release status recomputed from ordered / received / billed qty and unit prices (`match_status.rs`). Default policy is on received qty; price mismatch → `exception`.
- **Approve:** refuses `release_to_pay == no` (nothing received). `exception` still allows approve (visible for finance review). Emits goods (`grni_clear` = subtotal), `tax_total` and the payable total.
- Stock posts first; `qty_received` updates only after a successful inventory post.
- Supplier must be a `party` with `is_supplier`.
- **Pricing & tax:** `add_po_line` may omit `unit_price` and takes it from the order's (else default) buy price list. Each line carries `discount_pct` and one `tax`; `amount` is net, `tax_amount` separate, header `subtotal` + `tax_total` = `total`. Receipts value stock at the tax-exclusive `net_unit_price`. **Order unit:** a line may give `uom` (a case): `erp_item.convert_qty` turns the quantity into the item's own unit, and `unit_price` is divided by the factor. `qty`, `unit_price` and every downstream quantity (receipt, delivery, bill, stock) stay per item unit; the entered unit, quantity and price are kept in `order_uom`, `order_qty` and `order_unit_price` for display and printing.
- **Terms:** the PO's `payment_term` gives the bill a `due_date`, forwarded on `purchase_bill_approved`.

## Functions, commands, events

**RFQ:** `create_rfq`, `add_rfq_line`, `set_rfq_price`, `send_rfq`, `compare_rfqs`, `award_rfq`, `convert_rfq_to_po`, `cancel_rfq`, `get_rfq`

**Approval / settings:** `approve_po`, `reject_po`, `set_purchase_setting`, `get_purchase_setting`

**Screens:** `list_pos`, `list_rfqs`, `list_bills`, `list_receipts`, `list_returns`, `list_po_lines`, `list_rfq_lines`, `list_bill_lines`, `purchasing_dashboard`

**PO / receive / bill:** `create_po`, `add_po_line`, `confirm_po`, `cancel_po`, `receive_po`, `create_bill`, `approve_bill`, `get_po`

**Returns:** `return_to_supplier`, `create_debit_note`

**Emit:** `purchase_bill_approved`, `purchase_debit_note_approved`

## Policy, approval and RFQ comparison

- **Approval:** with a `po_approval_limit` above zero, `confirm_po` on a total above it parks the order in `to_approve` (no number). `purchase_admin` approves (number allocated, order confirmed) or sends it back to draft; an administrator confirming directly is not held.
- **Over-receipt:** `receive_po` accepts up to the ordered quantity plus `over_receipt_pct`.
- **Match basis:** `bill_match_on` `received` (default) or `ordered` selects the quantity bills are released against.
- **Comparing suppliers:** RFQs for the same need share a `request_group`; `set_rfq_price` records each supplier's answer, `compare_rfqs` shows the cheapest per item and per total, and `award_rfq` converts one to a PO and cancels the group's other open RFQs.

## Returns and debit notes

- `return_to_supplier` issues stock through `erp_inventory` with `source_kind` `erp_procurement.return` (the stock event clears GRNI rather than posting COGS), then lowers `qty_received`, raises `qty_returned` and writes `return` match rows. The order reopens for receiving.
- `create_debit_note` reverses part of an **approved** bill (`purchase_bill.kind = debit_note`, `reverses`). Lines repeat the bill line's price, discount and tax, are capped by what is not yet credited, and give `qty_billed` back. Approval emits `purchase_debit_note_approved`; no three-way match applies.

## Material requests

The first model here to lean on kernel features instead of hand-written bookkeeping: `number` is a naming series (`MR-{YYYY}-{#####}`), `lines` is a `child` field written with the request, `line_count` is a computed `count(lines)`, a `check` refuses a needed-by date before the request date, and chatter tracks every state change. **Who may make which move is the `workflow` in `rules/material_request.json`**: buyers submit and cancel, only `purchase_admin` approves or rejects, and ordering is a move from `approved`. The handlers (`submit_material_request`, `approve_material_request`, …) are plain state writes; a refused move changes nothing. `order_material_request` groups the unordered lines by supplier (the line's own, else the item's preferred supplier, else the `supplier` argument), creates one draft PO per supplier through the normal `create_po` / `add_po_line` (so pricing, tax and units apply), records the PO on each line and moves the request to `ordered`.

## Quality gate

`purchase_setting.require_inspection` (per company) makes `receive_po` require an `accepted` inspection of the order (`source_kind` `erp_procurement.po`, `source_id` the PO id). `erp_quality` is an **optional dependency**: the check asks `plugins::available("erp_quality")` and does nothing when it is not installed, so procurement never needs the quality plugin. A missing, pending or rejected inspection refuses the receipt with a message saying which.

## Roles

| Role | Scope |
|---|---|
| `purchase_admin` | Full control |
| `purchase_user` | Orders, receipts, bills |
| `purchase_reader` | Read documents |

## Boundaries

- Does not post GL itself — `gl_erp` listens to `purchase_bill_approved` (and stock events from the receipt move).
- No payments / bank reconciliation here.
- Inventory layers and valuation stay in `erp_inventory` / `erp_item`.
