# Procurement (`erp_procurement`)

## Purpose

Buy path: RFQ → purchase order → receive into stock → vendor bill → three-way match / release-to-pay. Receipts post through `erp_inventory`; bills emit a ledger event. App route `/erp/procurement`.

## Dependencies

`erp_item`, `erp_inventory`, `company`, `party`, `uom`, `currency`

## Models

| Model | Role |
|---|---|
| `rfq` / `rfq_line` | Request for quotation; states `draft` → `sent` → `awarded` / `cancelled` |
| `po` / `po_line` | Purchase order; line `qty_received` / `qty_billed` denormalized |
| `purchase_receipt` / `purchase_receipt_line` | Receipt header linked to `stock_move` |
| `purchase_bill` / `purchase_bill_line` | Vendor bill; `release_to_pay` / line `can_be_paid` |
| `purchase_match` | Append-only match rows (`kind` `receive` / `bill`) — source of truth for matched qty |
| `purchase_counter` | Document numbers (`RFQ`, `PO`, `PR`, `BILL`) |

PO states: `draft`, `confirmed`, `partially_received`, `received`, `closed`, `cancelled`.

## Design

- **Spine:** confirm PO → `receive_po` drafts/posts an inventory receipt (`source_kind` `erp_procurement.po`) and writes `purchase_match` receive rows → `create_bill` / `approve_bill` with bill match rows.
- **Match rows, not flags:** release status recomputed from ordered / received / billed qty and unit prices (`match_status.rs`). Default policy is on received qty; price mismatch → `exception`.
- **Approve:** refuses `release_to_pay == no` (nothing received). `exception` still allows approve (visible for finance review). Emits full bill total as GRNI clear + AP (no tax/variance split in v0.1).
- Stock posts first; `qty_received` updates only after a successful inventory post.
- Supplier must be a `party` with `is_supplier`.

## Functions, commands, events

**RFQ:** `create_rfq`, `add_rfq_line`, `send_rfq`, `convert_rfq_to_po`, `cancel_rfq`, `get_rfq`

**PO / receive / bill:** `create_po`, `add_po_line`, `confirm_po`, `cancel_po`, `receive_po`, `create_bill`, `approve_bill`, `get_po`

**Emit:** `purchase_bill_approved`

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
