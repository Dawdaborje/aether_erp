# Selling (`erp_selling`)

## Purpose

Outbound commercial spine: sales order → delivery (stock issue) → customer invoice. Thin state machine over `erp_inventory`; mirrors procurement on the sell side. Optional create-from CRM deal. App route `/erp/selling`.

## Dependencies

`erp_item`, `erp_inventory`, `company`, `party`, `uom`, `currency`, `crm_deal`, `crm_customer`

## Models

| Model | Role |
|---|---|
| `so` / `so_line` | Sales order; optional `deal` string; line `qty_delivered` / `qty_invoiced` |
| `sales_delivery` / `sales_delivery_line` | Delivery linked to posted `stock_move` |
| `sales_invoice` / `sales_invoice_line` | Customer invoice; optional `delivery_line` |
| `sales_counter` | Numbers (`SO`, `SD`, invoice prefix) |

SO states: `draft`, `confirmed`, `partially_delivered`, `delivered`, `closed`, `cancelled`.  
Delivery: `draft` / `posted` / `void`. Invoice: `draft` / `approved` / `cancelled`.

## Design

- **Confirm** assigns SO number; lines required. No automatic stock reservation on confirm — callers may reserve against demand `erp_selling.so` / so id; delivery issues use `source_kind` `erp_selling.so` so posting can consume those holds.
- **Deliver:** draft inventory issue, add lines, post, then update `qty_delivered` and SO state. Partial deliveries supported.
- **Invoice:** bill open delivered (or ordered) qty; approve emits `sales_invoice_approved` for AR (`gl_erp`). COGS/inventory come from the delivery’s `stock_posted`, not from the invoice event.
- **`create_so_from_deal`:** loads deal via `crm_deal.get_deal`, customer via `crm_customer`; multiple SOs per deal allowed; win stays on CRM.
- Customer is an active `party` (v0.1 does not require a strict `is_customer` flag).

## Functions, commands, events

**Handlers:** `create_so`, `create_so_from_deal`, `add_so_line`, `confirm_so`, `cancel_so`, `deliver_so`, `create_invoice`, `approve_invoice`, `get_so`

**Emit:** `sales_invoice_approved`

## Roles

| Role | Scope |
|---|---|
| `sales_admin` | Full control |
| `sales_user` | Orders, deliveries, invoices |
| `sales_reader` | Read documents |

## Boundaries

- Does not post GL — `gl_erp` listens to invoice (and stock) events.
- No quotations, pricing engines, or payment collection here.
- CRM owns pipeline win/lose; this plugin only optionally seeds an SO from a deal.
