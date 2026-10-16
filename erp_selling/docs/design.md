# Selling (`erp_selling`)

## Purpose

Outbound commercial spine: sales order → delivery (stock issue) → customer invoice. Thin state machine over `erp_inventory`; mirrors procurement on the sell side. Optional create-from CRM deal. App route `/erp/selling`.

## Dependencies

`erp_item`, `erp_inventory`, `erp_pricing`, `company`, `party`, `uom`, `currency`, `crm_deal`, `crm_customer`

## Models

| Model | Role |
|---|---|
| `so` / `so_line` | Sales order; optional `deal` string; line `qty_delivered` / `qty_invoiced` |
| `sales_delivery` / `sales_delivery_line` | Delivery linked to posted `stock_move` |
| `sales_invoice` / `sales_invoice_line` | Customer invoice; optional `delivery_line` |
| `sales_return` / `sales_return_line` | Goods taken back from the customer, linked to the inbound `stock_move`, with the cost they left at |
| `quotation` / `quotation_line` | Offer to a customer: valid-until date, optional add-on lines, states `draft` / `sent` / `accepted` / `rejected` / `expired` / `converted` |
| `customer_credit` | Per `(company, customer)` credit limit and hold flag |
| `sales_setting` | Per company: enforce credit limit, default payment term and price list, quotation validity |
| `sales_counter` | Numbers (`SO`, `SD`, `SINV`, `QT`, `SRET`, `CN`) |

SO states: `draft`, `confirmed`, `partially_delivered`, `delivered`, `closed`, `cancelled`.  
Delivery: `draft` / `posted` / `void`. Invoice: `draft` / `approved` / `cancelled`.

## Design

- **Confirm** assigns SO number; lines required. No automatic stock reservation on confirm — callers may reserve against demand `erp_selling.so` / so id; delivery issues use `source_kind` `erp_selling.so` so posting can consume those holds.
- **Deliver:** draft inventory issue, add lines, post, then update `qty_delivered` and SO state. Partial deliveries supported.
- **Invoice:** bill open delivered (or ordered) qty; approve emits `sales_invoice_approved` for AR (`gl_erp`). COGS/inventory come from the delivery’s `stock_posted`, not from the invoice event.
- **`create_so_from_deal`:** loads deal via `crm_deal.get_deal`, customer via `crm_customer`; multiple SOs per deal allowed; win stays on CRM.
- **Pricing & tax:** `add_so_line` may omit `unit_price` (taken from the order's or default sell price list, party-specific prices first). Lines carry `discount_pct` and one `tax`; header `subtotal` + `tax_total` = `total`. The invoice repeats the SO line's discount and tax; the event carries `revenue` (net), `tax_total`, `ar_total` and `due_date` from the order's `payment_term`. **Order unit:** a line may give `uom` (a case): `erp_item.convert_qty` turns the quantity into the item's own unit, and `unit_price` is divided by the factor. `qty`, `unit_price` and every downstream quantity (receipt, delivery, bill, stock) stay per item unit; the entered unit, quantity and price are kept in `order_uom`, `order_qty` and `order_unit_price` for display and printing.
- Customer is an active `party` (v0.1 does not require a strict `is_customer` flag).

## Functions, commands, events

**Handlers:** `create_so`, `create_so_from_deal`, `add_so_line`, `confirm_so`, `cancel_so`, `deliver_so`, `create_invoice`, `approve_invoice`, `get_so`

**Quotations:** `create_quotation`, `add_quotation_line`, `send_quotation`, `accept_quotation`, `reject_quotation`, `convert_quotation_to_so`

**Credit / settings:** `set_credit_limit`, `check_credit`, `set_sales_setting`

**Screens:** `list_sos`, `list_quotations`, `list_invoices`, `list_deliveries`, `list_returns`, `list_so_lines`, `list_quotation_lines`, `list_invoice_lines`, `sales_dashboard`

**Returns:** `return_from_customer`, `create_credit_note`

**Emit:** `sales_invoice_approved`, `sales_credit_note_approved`

## Quotations and credit control

- **Quotation → order:** a sent or accepted quotation converts to a draft SO that keeps the quoted price, discount and tax of each line; optional lines are only taken when named. An expired quotation is refused (and marked expired).
- **Credit limit:** with `enforce_credit_limit`, `confirm_so` refuses an order that takes the customer's exposure over `credit_limit`, or a customer on hold, unless a `sales_admin` confirms. Exposure is the uninvoiced value of open orders plus open receivable documents from `gl_payment` when installed (those are whole documents, not net of part payments). `check_credit` reports the figures.

## Returns and credit notes

- `return_from_customer` receives stock back at the **cost the goods were delivered at** (read from the delivery's posted move), with `source_kind` `erp_selling.return` so the stock event credits COGS. It lowers `qty_delivered`, raises `qty_returned`.
- `create_credit_note` reverses part of an **approved** invoice (`sales_invoice.kind = credit_note`, `reverses`), capped by what is not yet credited; approval emits `sales_credit_note_approved`. `gl` nets it against the invoice by allocation on the customer's control account.

## Quality gate

`sales_setting.require_inspection` makes `deliver_so` require an `accepted` inspection of the order (`source_kind` `erp_selling.so`) when the optional `erp_quality` plugin is installed; without it the setting has no effect.

## Roles

| Role | Scope |
|---|---|
| `sales_admin` | Full control |
| `sales_user` | Orders, deliveries, invoices |
| `sales_reader` | Read documents |

## Boundaries

- Does not post GL — `gl_erp` listens to invoice (and stock) events.
- No payment collection here; pricing and tax rules live in `erp_pricing`.
- CRM owns pipeline win/lose; this plugin only optionally seeds an SO from a deal.
