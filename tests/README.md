# ERP end-to-end checks

`erp_e2e.py` drives the ERP plugins over HTTP as two people (the organization owner and a buyer) and checks
that stock, value and documents agree. Plain Python 3, no packages. It is the safety net for changes to money
paths (taxes, numbering, valuation).

## One-time setup (the `project/test` project)

```sh
cd project/test
A="../../aether/target/debug/aether -c aether.toml"
$A --load-plugins ../../plugins/base ../../plugins/crm ../../plugins/erp
$A --create-org "ERP Test" --org-db-name erp_test --username erp_owner --email erp_owner@test.local --password Test-pass-123
for p in erp_item erp_pricing erp_inventory erp_procurement erp_manufacturing erp_selling erp_replenishment erp_quality erp_warehouse; do
  $A --install-plugin $p --org erp_test
done
$A --create-user --username erp_buyer --email erp_buyer@test.local --password Test-pass-123 --org-db-name erp_test
$A --grant-role erp_procurement.purchase_user --username erp_buyer --org erp_test
```

After changing a plugin: `make` in its folder (or `make fast`), `--load-plugins` it again, then
`--upgrade-plugin <name> --org erp_test`.

## Running

```sh
python3 plugins/erp/tests/erp_e2e.py          # -v prints every call
```

It uses fresh items, parties and warehouses every run, so it can be run again and again. It exits 1 when a
check fails. Sections 8 and 9 are skipped when `erp_quality` or `erp_warehouse` is not installed.

| Section | What it checks |
|---|---|
| 1 | PO → receive → bill; stock and value on hand; over-receipt refused; bill approved |
| 2 | average cost across receipts; issue, void restores exactly; no negative stock |
| 3 | sales order → deliver → invoice; stock value falls at cost; over-invoicing refused |
| 4 | an order unit (a case): conversion, line kept per item unit |
| 5 | scrap needs a reason and reduces stock |
| 6 | MO numbered on confirm, build consumes parts and values finished goods, unbuild returns parts |
| 7 | material request workflow: a buyer cannot approve; ordering needs approval; one PO per supplier |
| 8 | quality gate: receipt refused without an accepted inspection, allowed after |
| 9 | bins, putaway rule, transfer limits, pick list numbered on release, pick takes stock out of the bin |
| 11 | tax groups: two taxes on one price, compound, withholding, included taxes split in proportion, refused combinations, and the totals on a real PO and SO |
| 10 | with `gl_erp` installed: stock value, COGS, revenue, receivable and GRNI in the ledger move by exactly what the documents say, and the trial balance balances |

Section 10 creates the accounts, journals and posting rules it needs from the `gl_erp` templates (install with
`--load-plugins ../../plugins/accounting/{gl,gl_payment,gl_erp}` and `--install-plugin gl_erp --org erp_test`).

Not covered yet: the per-tax breakdown in the ledger, landed cost, counts, replenishment.
