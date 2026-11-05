# End-to-end checks of the ERP plugins against a running kernel.
#
# Needs: the server running, an organization with the ERP plugins installed (README.md next to this file),
# the organization owner, and one buyer who holds erp_procurement.purchase_user (no admin role).
#
# Usage: python3 erp_e2e.py [--base http://localhost:7890] [--org erp_test] [-v]
# Every run makes fresh items, parties and warehouses, so it can run again on the same organization.
import argparse, json, secrets, sys, urllib.error, urllib.request
from decimal import Decimal

ap = argparse.ArgumentParser()
ap.add_argument("--base", default="http://localhost:7890")
ap.add_argument("--org", default="erp_test")
ap.add_argument("--owner", default="erp_owner")
ap.add_argument("--buyer", default="erp_buyer")
ap.add_argument("--password", default="Test-pass-123")
ap.add_argument("--date", default="2026-10-01")
ap.add_argument("-v", action="store_true")
args = ap.parse_args()
RUN = secrets.token_hex(3)
D0 = args.date


class Failed(Exception):
    def __init__(self, status, data):
        super().__init__(f"{status}: {json.dumps(data)[:400]}")
        self.status, self.data = status, data


class Session:
    def __init__(self, username):
        self.cookies = {"aether_csrf": secrets.token_hex(16)}
        self.name = username
        self.post("/api/auth/login", {"username": username, "password": args.password})
        self.post("/api/auth/org", {"org": args.org})

    def post(self, path, body):
        request = urllib.request.Request(args.base + path, data=json.dumps(body).encode(), method="POST")
        request.add_header("content-type", "application/json")
        request.add_header("origin", args.base)
        request.add_header("x-csrf-token", self.cookies["aether_csrf"])
        request.add_header("cookie", "; ".join(f"{k}={v}" for k, v in self.cookies.items()))
        try:
            response = urllib.request.urlopen(request)
            status, raw, headers = response.status, response.read(), response.headers
        except urllib.error.HTTPError as error:
            status, raw, headers = error.code, error.read(), error.headers
        for cookie in headers.get_all("set-cookie") or []:
            name, _, rest = cookie.partition("=")
            self.cookies[name.strip()] = rest.split(";")[0]
        try:
            data = json.loads(raw) if raw else None
        except ValueError:
            data = {"_raw": raw.decode(errors="replace")}
        if status >= 400:
            raise Failed(status, data)
        return data

    def call(self, plugin, function, body=None):
        answer = self.post(f"/api/plugins/{plugin}/{function}", body or {})
        if args.v:
            print(f"    {self.name} {plugin}.{function} -> {json.dumps(answer)[:300]}")
        return answer


results = []


def check(name, condition, detail=""):
    results.append((name, bool(condition)))
    print(("  ok    " if condition else "  FAIL  ") + name + ("" if condition else f"  [{detail}]"))


def refused(name, action, words=()):
    try:
        action()
    except Failed as failure:
        message = json.dumps(failure.data)
        check(name, not words or any(w in message for w in words), message[:300])
        return
    check(name, False, "was allowed")


def d(value):
    return Decimal(str(value)) if value is not None else Decimal(0)


def installed(session, plugin, function, body=None):
    """True when the plugin answers (it is installed in the organization)."""
    try:
        session.call(plugin, function, body or {})
        return True
    except Failed as failure:
        return failure.status != 404


owner = Session(args.owner)
buyer = Session(args.buyer)

# ---------------------------------------------------------------------------------------------------------------
print("setup")
units = owner.call("uom", "list_units", {})["rows"]
if not units:
    owner.call("uom", "seed_units", {})
    units = owner.call("uom", "list_units", {})["rows"]
unit = next((u for u in units if u.get("code") in ("unit", "units", "pcs", "ea", "each")), units[0])
case = next((u for u in units if u.get("code") in ("dozen", "case", "box", "pack")), None)
companies = owner.call("company", "list_companies", {})["rows"]
company = next((c for c in companies if c.get("name") == "ERP Test Co"), None) or owner.call(
    "company", "create_company", {"name": "ERP Test Co", "currency_code": "USD"})
CO = company["id"]
supplier = owner.call("party", "create_organization", {"name": f"Supplier {RUN}", "is_supplier": True})
customer = owner.call("party", "create_organization", {"name": f"Customer {RUN}", "is_customer": True})
category = owner.call("erp_item", "create_category", {"code": f"cat{RUN}", "name": f"Goods {RUN}", "valuation_method": "average"})


def make_item(code, name):
    item = owner.call("erp_item", "create_item", {"code": f"{code}{RUN}", "name": f"{name} {RUN}", "uom": unit["id"], "category": category["id"]})
    owner.call("erp_item", "set_item_company", {"item": item["id"], "company": CO, "valuation_method": "average"})
    return item


widget = make_item("wid", "Widget")
part_a = make_item("pta", "Part A")
part_b = make_item("ptb", "Part B")
house = owner.call("erp_inventory", "create_warehouse", {"code": f"wh{RUN}", "name": f"Main {RUN}", "company": CO})
WH = house["id"]


def on_hand(item, warehouse=WH):
    r = owner.call("erp_inventory", "on_hand", {"company": CO, "item": item["id"], "warehouse": warehouse})
    return d(r["qty"]), d(r["value"])


# ---------------------------------------------------------------------------------------------------------------
print("1. purchase: order, receive, bill, three-way match")
po = owner.call("erp_procurement", "create_po", {"company": CO, "supplier": supplier["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
check("a new purchase order starts as a draft", po["state"] == "draft", po)
pl = owner.call("erp_procurement", "add_po_line", {"po": po["id"], "item": widget["id"], "qty": "10", "unit_price": "20.00"})
owner.call("erp_procurement", "confirm_po", {"id": po["id"]})
rec = owner.call("erp_procurement", "receive_po", {"po": po["id"], "receipt_date": D0, "lines": [{"po_line": pl["id"], "qty": "10"}]})
q, v = on_hand(widget)
check("receiving puts 10 on hand at value 200", q == 10 and v == Decimal("200"), (q, v))
refused("receiving more than ordered is refused", lambda: owner.call("erp_procurement", "receive_po", {"po": po["id"], "lines": [{"po_line": pl["id"], "qty": "1"}]}))
bill = owner.call("erp_procurement", "create_bill", {"po": po["id"], "bill_date": D0, "lines": [{"po_line": pl["id"], "qty": "10"}]})
approved = owner.call("erp_procurement", "approve_bill", {"id": bill["id"]})
check("the bill matches the receipt and is approved", approved.get("state") in ("approved", "released") or approved.get("release_to_pay") in ("yes", True), approved)

# ---------------------------------------------------------------------------------------------------------------
print("2. stock: issue, void, no negatives, average cost")
more = owner.call("erp_inventory", "draft_receipt", {"company": CO, "posting_date": D0, "currency": "USD"})
owner.call("erp_inventory", "add_line", {"move": more["id"], "item": widget["id"], "qty": "10", "warehouse": WH, "unit_cost": "30.00"})
owner.call("erp_inventory", "post_move", {"id": more["id"]})
q, v = on_hand(widget)
check("a second receipt at 30 averages the value: 20 on hand, 500", q == 20 and v == Decimal("500"), (q, v))
issue = owner.call("erp_inventory", "draft_issue", {"company": CO, "posting_date": D0, "currency": "USD"})
owner.call("erp_inventory", "add_line", {"move": issue["id"], "item": widget["id"], "qty": "8", "warehouse": WH})
posted = owner.call("erp_inventory", "post_move", {"id": issue["id"]})
q, v = on_hand(widget)
check("issuing 8 leaves 12 worth 300 (average 25)", q == 12 and v == Decimal("300"), (q, v))
owner.call("erp_inventory", "void_move", {"id": issue["id"], "reason": "test void"})
q, v = on_hand(widget)
check("voiding the issue restores 20 and 500: nothing is rewritten, the void reverses", q == 20 and v == Decimal("500"), (q, v))
big = owner.call("erp_inventory", "draft_issue", {"company": CO, "posting_date": D0, "currency": "USD"})
refused("an issue larger than stock is refused (no negatives)", lambda: (
    owner.call("erp_inventory", "add_line", {"move": big["id"], "item": widget["id"], "qty": "999", "warehouse": WH}),
    owner.call("erp_inventory", "post_move", {"id": big["id"]})))

# ---------------------------------------------------------------------------------------------------------------
print("3. selling: order, deliver, invoice, cost of goods")
so = owner.call("erp_selling", "create_so", {"company": CO, "customer": customer["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
sl = owner.call("erp_selling", "add_so_line", {"so": so["id"], "item": widget["id"], "qty": "4", "unit_price": "50.00"})
owner.call("erp_selling", "confirm_so", {"id": so["id"]})
owner.call("erp_selling", "deliver_so", {"so": so["id"], "delivery_date": D0, "lines": [{"so_line": sl["id"], "qty": "4"}]})
q, v = on_hand(widget)
check("delivering 4 leaves 16 worth 400", q == 16 and v == Decimal("400"), (q, v))
inv = owner.call("erp_selling", "create_invoice", {"so": so["id"], "invoice_date": D0, "lines": [{"so_line": sl["id"], "qty": "4"}]})
inv = owner.call("erp_selling", "approve_invoice", {"id": inv["id"]})
check("the invoice totals 200 (4 x 50)", d(inv.get("total")) == Decimal("200"), inv.get("total"))
refused("invoicing more than delivered is refused", lambda: owner.call("erp_selling", "create_invoice", {"so": so["id"], "lines": [{"so_line": sl["id"], "qty": "5"}]}))

# ---------------------------------------------------------------------------------------------------------------
print("4. order units: a case on a purchase line")
if case:
    owner.call("erp_item", "set_item_uom", {"item": widget["id"], "uom": case["id"], "factor": "12"})
    conv = owner.call("erp_item", "convert_qty", {"item": widget["id"], "qty": "2", "from_uom": case["id"]})
    check("2 cases convert to 24 units", d(conv["base_qty"]) == Decimal("24"), conv)
    po2 = owner.call("erp_procurement", "create_po", {"company": CO, "supplier": supplier["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
    line = owner.call("erp_procurement", "add_po_line", {"po": po2["id"], "item": widget["id"], "qty": "2", "uom": case["id"], "unit_price": "120.00"})
    check("the line is kept per unit: 24 at 10 each, entered as 2 cases", d(line["qty"]) == Decimal("24") and d(line["unit_price"]) == Decimal("10"), line)
else:
    print("  skip  no case-like unit in this organization")

# ---------------------------------------------------------------------------------------------------------------
print("5. scrap")
owner.call("erp_inventory", "scrap_stock", {"company": CO, "warehouse": WH, "item": widget["id"], "qty": "1", "reason": "dropped"})
q, v = on_hand(widget)
check("scrapping 1 leaves 15", q == 15, (q, v))
refused("scrap needs a reason", lambda: owner.call("erp_inventory", "scrap_stock", {"company": CO, "warehouse": WH, "item": widget["id"], "qty": "1", "reason": " "}), ["why", "reason"])

# ---------------------------------------------------------------------------------------------------------------
print("6. manufacturing: build, number on confirm, unbuild")
for it, qty, cost in ((part_a, "20", "5.00"), (part_b, "20", "3.00")):
    m = owner.call("erp_inventory", "draft_receipt", {"company": CO, "posting_date": D0, "currency": "USD"})
    owner.call("erp_inventory", "add_line", {"move": m["id"], "item": it["id"], "qty": qty, "warehouse": WH, "unit_cost": cost})
    owner.call("erp_inventory", "post_move", {"id": m["id"]})
fg = make_item("fg", "Finished")
bom = owner.call("erp_manufacturing", "create_bom", {"company": CO, "code": f"B{RUN}", "item": fg["id"], "qty": "1"})
owner.call("erp_manufacturing", "add_bom_line", {"bom": bom["id"], "item": part_a["id"], "qty": "2"})
owner.call("erp_manufacturing", "add_bom_line", {"bom": bom["id"], "item": part_b["id"], "qty": "1"})
mo = owner.call("erp_manufacturing", "create_mo", {"company": CO, "bom": bom["id"], "qty": "5", "warehouse": WH})
check("a draft order has no number yet", not mo.get("number"), mo.get("number"))
mo = owner.call("erp_manufacturing", "confirm_mo", {"id": mo["id"]})
check("confirming numbers it MO-<year>-#####", str(mo.get("number", "")).startswith("MO-"), mo.get("number"))
number = mo["number"]
owner.call("erp_manufacturing", "complete_mo", {"id": mo["id"]})
q_fg, v_fg = on_hand(fg)
qa, _ = on_hand(part_a)
qb, _ = on_hand(part_b)
check("5 finished goods made at 13 each (2x5 + 1x3), parts consumed", q_fg == 5 and v_fg == Decimal("65") and qa == 10 and qb == 15, (q_fg, v_fg, qa, qb))
owner.call("erp_manufacturing", "unbuild_mo", {"mo": mo["id"], "qty": "2", "reason": "test"})
q_fg, v_fg = on_hand(fg)
qa, _ = on_hand(part_a)
check("unbuilding 2 returns 4 of part A and leaves 3 finished worth 39", q_fg == 3 and v_fg == Decimal("39") and qa == 14, (q_fg, v_fg, qa))
refused("unbuilding more than was built is refused", lambda: owner.call("erp_manufacturing", "unbuild_mo", {"mo": mo["id"], "qty": "9"}))

# ---------------------------------------------------------------------------------------------------------------
print("7. material requests and the kernel workflow")
mr = buyer.call("erp_procurement", "create_material_request", {"company": CO, "warehouse": WH, "item": part_a["id"], "qty": "50", "unit_price": "5.00"})
check("the request is numbered MR-<year>-#####", str(mr.get("number", "")).startswith("MR-"), mr.get("number"))
buyer.call("erp_procurement", "submit_material_request", {"id": mr["id"]})
refused("a buyer cannot approve their own request (workflow roles)", lambda: buyer.call("erp_procurement", "approve_material_request", {"id": mr["id"]}), ["not allowed", "may not"])
refused("an unapproved request cannot be ordered", lambda: buyer.call("erp_procurement", "order_material_request", {"id": mr["id"], "supplier": supplier["id"]}), ["approved"])
owner.call("erp_procurement", "approve_material_request", {"id": mr["id"]})
ordered = buyer.call("erp_procurement", "order_material_request", {"id": mr["id"], "supplier": supplier["id"]})
check("ordering creates one purchase order and marks the request ordered", len(ordered["orders"]) == 1 and ordered["request"]["state"] == "ordered", ordered)

# ---------------------------------------------------------------------------------------------------------------
print("8. quality gate (only when erp_quality is installed)")
if installed(owner, "erp_quality", "list_templates"):
    owner.call("erp_procurement", "set_purchase_setting", {"company": CO, "require_inspection": True})
    gate = owner.call("erp_procurement", "create_po", {"company": CO, "supplier": supplier["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
    gl = owner.call("erp_procurement", "add_po_line", {"po": gate["id"], "item": part_a["id"], "qty": "5", "unit_price": "5.00"})
    owner.call("erp_procurement", "confirm_po", {"id": gate["id"]})
    refused("with inspection required, receiving without an accepted inspection is refused",
            lambda: owner.call("erp_procurement", "receive_po", {"po": gate["id"], "lines": [{"po_line": gl["id"], "qty": "5"}]}), ["inspection"])
    tpl = owner.call("erp_quality", "create_template", {"code": f"T{RUN}", "name": f"Check {RUN}", "applies_to": "receipt",
                                                       "parameters": [{"name": "Width", "kind": "numeric", "min": "9", "max": "11"}]})
    insp = owner.call("erp_quality", "create_inspection", {"company": CO, "template": tpl["id"], "item": part_a["id"], "source_kind": "erp_procurement.po", "source_id": gate["id"]})
    reading = owner.call("erp_quality", "list_readings", {"id": insp["id"]})[0]
    owner.call("erp_quality", "record_reading", {"reading": reading["id"], "value": "10"})
    done = owner.call("erp_quality", "complete_inspection", {"id": insp["id"]})
    check("a reading inside the range accepts the inspection", done["state"] == "accepted", done)
    owner.call("erp_procurement", "receive_po", {"po": gate["id"], "lines": [{"po_line": gl["id"], "qty": "5"}]})
    check("and then the goods can be received", on_hand(part_a)[0] == 19, on_hand(part_a))
    owner.call("erp_procurement", "set_purchase_setting", {"company": CO, "require_inspection": False})
else:
    print("  skip  erp_quality is not installed")

# ---------------------------------------------------------------------------------------------------------------
print("9. warehouse: bins, putaway, transfers, picking (only when erp_warehouse is installed)")
if installed(owner, "erp_warehouse", "list_bins"):
    a = owner.call("erp_warehouse", "create_bin", {"warehouse": WH, "code": f"A{RUN}"})
    b = owner.call("erp_warehouse", "create_bin", {"warehouse": WH, "code": f"B{RUN}"})
    refused("a bin code is unique in its warehouse", lambda: owner.call("erp_warehouse", "create_bin", {"warehouse": WH, "code": f"A{RUN}"}))
    owner.call("erp_warehouse", "create_putaway_rule", {"warehouse": WH, "bin": a["id"], "item": widget["id"]})
    s = owner.call("erp_warehouse", "suggest_bin", {"warehouse": WH, "item": widget["id"]})
    check("the putaway rule suggests bin A", s["bin"] == a["id"], s)
    owner.call("erp_warehouse", "put_away", {"company": CO, "warehouse": WH, "item": widget["id"], "qty": "10"})
    refused("moving more than a bin holds is refused", lambda: owner.call("erp_warehouse", "move_between_bins",
            {"company": CO, "warehouse": WH, "item": widget["id"], "qty": "11", "from_bin": a["id"], "to_bin": b["id"]}))
    owner.call("erp_warehouse", "move_between_bins", {"company": CO, "warehouse": WH, "item": widget["id"], "qty": "4", "from_bin": a["id"], "to_bin": b["id"]})
    stock = {(r["bin"]): d(r["qty"]) for r in owner.call("erp_warehouse", "bin_stock", {"warehouse": WH, "item": widget["id"]})["rows"]}
    check("bin A holds 6 and bin B holds 4", stock.get(a["id"]) == 6 and stock.get(b["id"]) == 4, stock)
    pick = owner.call("erp_warehouse", "create_pick_list", {"company": CO, "warehouse": WH, "lines": [{"item": widget["id"], "qty": "3", "bin": a["id"]}]})
    check("a pick list has no number until released", not pick.get("number"), pick.get("number"))
    pick = owner.call("erp_warehouse", "release_pick_list", {"id": pick["id"]})
    check("releasing numbers it PK-<year>-#####", str(pick.get("number", "")).startswith("PK-"), pick.get("number"))
    owner.call("erp_warehouse", "confirm_pick", {"id": pick["id"]})
    stock = {(r["bin"]): d(r["qty"]) for r in owner.call("erp_warehouse", "bin_stock", {"warehouse": WH, "item": widget["id"]})["rows"]}
    check("confirming the pick takes 3 out of bin A", stock.get(a["id"]) == 3, stock)
else:
    print("  skip  erp_warehouse is not installed")

# ---------------------------------------------------------------------------------------------------------------
print("10. the ledger agrees with stock and documents (only when gl_erp is installed)")
import time
if installed(owner, "gl_erp", "stock_rule_templates"):
    Y = int(D0[:4])
    try:
        owner.call("gl", "get_ledger")
    except Failed:
        owner.call("gl", "setup_ledger", {"name": "ERP test ledger", "base_currency": "USD"})
    have = {a["code"] for a in owner.call("gl", "list_accounts")}
    for code, name, kind, control in [
        ("1100", "Accounts receivable", "asset", True), ("1400", "Inventory", "asset", False), ("1450", "Work in progress", "asset", False),
        ("2000", "Goods received not invoiced", "liability", False), ("2100", "Accounts payable", "liability", True),
        ("3000", "Opening balances", "equity", False), ("4000", "Sales revenue", "income", False),
        ("5000", "Cost of goods sold", "expense", False), ("5100", "Inventory adjustment", "expense", False),
        ("5200", "Purchase price variance", "expense", False), ("5300", "Manufacturing absorbed", "expense", False),
    ]:
        if code not in have:
            owner.call("gl", "create_account", {"code": code, "name": name, "kind": kind, "is_control": control})
    have_j = {j["code"] for j in owner.call("gl", "list_journals")}
    for code, name, kind, prefix in [("SAL", "Sales", "general", "SAL"), ("PUR", "Purchases", "purchase", "PUR"), ("STK", "Stock", "general", "STK")]:
        if code not in have_j:
            owner.call("gl", "create_journal", {"code": code, "name": name, "kind": kind, "prefix": prefix})
    try:
        owner.call("gl", "create_fiscal_year", {"year": Y})
    except Failed:
        pass
    rules = {r["event"] for r in owner.call("gl", "list_posting_rules") if r.get("is_active")}
    for templates, journal in [("stock_rule_templates", "STK"), ("invoice_rule_templates", "SAL"), ("bill_rule_templates", "PUR")]:
        for event, t in owner.call("gl_erp", templates)["events"].items():
            # Every account a template names must exist; make the missing ones from the template's own map.
            known = {a["code"] for a in owner.call("gl", "list_accounts")}
            for code, name in (t.get("accounts") or {}).items():
                if code not in known:
                    kind = {"1": "asset", "2": "liability", "3": "equity", "4": "income"}.get(code[0], "expense")
                    owner.call("gl", "create_account", {"code": code, "name": name, "kind": kind, "is_control": False})
            if event not in rules:
                owner.call("gl", "create_posting_rule", {"event": event, "journal": journal, "source_path": t["source_path"],
                           "version_path": t.get("version_path"), "date_path": t["date_path"], "currency_path": t.get("currency_path"),
                           "memo": t.get("memo"), "lines": t["lines"]})

    def balances():
        tb = owner.call("gl", "trial_balance_report")
        out = {}
        for r in tb.get("rows", []):
            code = r.get("account") or r.get("code")
            out[code] = d(r.get("debit")) - d(r.get("credit"))
        return tb, out

    def wait_for(predicate, tries=45):
        for _ in range(tries):
            if predicate():
                return True
            time.sleep(2)
        return False

    _, before = balances()
    tie = make_item("tie", "Tie-out")
    # Receive 10 at 20 through a purchase order, sell 4 at 50.
    tpo = owner.call("erp_procurement", "create_po", {"company": CO, "supplier": supplier["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
    tl = owner.call("erp_procurement", "add_po_line", {"po": tpo["id"], "item": tie["id"], "qty": "10", "unit_price": "20.00"})
    owner.call("erp_procurement", "confirm_po", {"id": tpo["id"]})
    owner.call("erp_procurement", "receive_po", {"po": tpo["id"], "receipt_date": D0, "lines": [{"po_line": tl["id"], "qty": "10"}]})
    tso = owner.call("erp_selling", "create_so", {"company": CO, "customer": customer["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
    tsl = owner.call("erp_selling", "add_so_line", {"so": tso["id"], "item": tie["id"], "qty": "4", "unit_price": "50.00"})
    owner.call("erp_selling", "confirm_so", {"id": tso["id"]})
    owner.call("erp_selling", "deliver_so", {"so": tso["id"], "delivery_date": D0, "lines": [{"so_line": tsl["id"], "qty": "4"}]})
    tinv = owner.call("erp_selling", "create_invoice", {"so": tso["id"], "invoice_date": D0, "lines": [{"so_line": tsl["id"], "qty": "4"}]})
    owner.call("erp_selling", "approve_invoice", {"id": tinv["id"]})
    stock_q, stock_v = on_hand(tie)
    check("stock holds 6 worth 120", stock_q == 6 and stock_v == Decimal("120"), (stock_q, stock_v))
    # The events are posted in the background: wait until the ledger has caught up.
    settled = wait_for(lambda: (balances()[1].get("1400", Decimal(0)) - before.get("1400", Decimal(0))) == stock_v
                       and (balances()[1].get("4000", Decimal(0)) - before.get("4000", Decimal(0))) == Decimal("-200"))
    tb, after = balances()
    delta = lambda code: after.get(code, Decimal(0)) - before.get(code, Decimal(0))
    check("the ledger caught up with the events", settled, {k: str(delta(k)) for k in ("1400", "4000", "5000", "1100", "2000")})
    check("inventory in the ledger moved by exactly the stock value (120)", delta("1400") == stock_v, delta("1400"))
    check("cost of goods sold is 80 (4 x 20)", delta("5000") == Decimal("80"), delta("5000"))
    check("revenue is 200 and receivable is 200", delta("4000") == Decimal("-200") and delta("1100") == Decimal("200"), (delta("4000"), delta("1100")))
    check("goods received not invoiced holds the 200 received", delta("2000") == Decimal("-200"), delta("2000"))
    check("the trial balance balances", sum(after.values(), Decimal(0)) == 0, sum(after.values(), Decimal(0)))
    # A void of the delivery's stock reverses its postings exactly.
else:
    print("  skip  gl_erp is not installed")

# ---------------------------------------------------------------------------------------------------------------
print("11. several taxes on one line: groups, compound, withholding")
def new_tax(code, rate, **extra):
    return owner.call("erp_pricing", "create_tax", {"code": f"{code}{RUN}", "name": f"{code} {RUN}", "rate": rate, **extra})


def price(tax_id, qty="1", unit="100.00", discount="0"):
    return owner.call("erp_pricing", "compute_line", {"qty": qty, "unit_price": unit, "discount_pct": discount, "tax": tax_id})


vat = new_tax("VAT", "10")
levy = new_tax("LEV", "5")
wht = new_tax("WHT", "5", is_withholding=True)
single = price(vat["id"])
check("a plain tax still prices as before: 100 + 10 = 110", d(single["tax"]) == Decimal("10") and d(single["total"]) == Decimal("110"), single)
added = owner.call("erp_pricing", "create_tax_group", {"code": f"G1{RUN}", "name": "VAT + levy", "components": [{"tax": vat["id"]}, {"tax": levy["id"]}]})
r = price(added["id"])
check("two taxes on the same price: 10 + 5 = 15, total 115", d(r["tax"]) == Decimal("15") and d(r["total"]) == Decimal("115"), r)
check("the breakdown lists each tax with its amount", [d(b["amount"]) for b in r["breakdown"]] == [Decimal("10"), Decimal("5")], r.get("breakdown"))
comp = owner.call("erp_pricing", "create_tax_group", {"code": f"G2{RUN}", "name": "VAT then levy", "components": [{"tax": vat["id"]}, {"tax": levy["id"], "compound": True}]})
r = price(comp["id"])
check("compound: 10 then 5% of 110 = 5.50, total 115.50", d(r["tax"]) == Decimal("15.5") and d(r["total"]) == Decimal("115.5"), r)
held = owner.call("erp_pricing", "create_tax_group", {"code": f"G3{RUN}", "name": "VAT less withholding", "applies_to": "purchase", "components": [{"tax": vat["id"]}, {"tax": wht["id"]}]})
r = price(held["id"])
check("withholding: 100 + 10 - 5 = 105", d(r["tax"]) == Decimal("5") and d(r["total"]) == Decimal("105"), r)
inc_a = new_tax("INA", "12", price_includes_tax=True)
inc_b = new_tax("INB", "6", price_includes_tax=True)
inc = owner.call("erp_pricing", "create_tax_group", {"code": f"G4{RUN}", "name": "included pair", "components": [{"tax": inc_a["id"]}, {"tax": inc_b["id"]}]})
r = price(inc["id"], unit="118.00")
check("included taxes split out of 118: net 100, tax 18, total 118", d(r["net"]) == Decimal("100") and d(r["tax"]) == Decimal("18") and d(r["total"]) == Decimal("118"), r)
refused("a group cannot mix included and added taxes", lambda: owner.call("erp_pricing", "create_tax_group",
        {"code": f"G5{RUN}", "name": "bad", "components": [{"tax": inc_a["id"]}, {"tax": vat["id"]}]}), ["mix"])
refused("a group cannot hold another group", lambda: owner.call("erp_pricing", "create_tax_group",
        {"code": f"G6{RUN}", "name": "nested", "components": [{"tax": added["id"]}]}), ["another group"])
refused("a purchase-only group is refused on a sale", lambda: owner.call("erp_pricing", "compute_line",
        {"qty": "1", "unit_price": "100", "tax": held["id"], "side": "sales"}), ["apply"])

sp = owner.call("erp_procurement", "create_po", {"company": CO, "supplier": supplier["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
owner.call("erp_procurement", "add_po_line", {"po": sp["id"], "item": widget["id"], "qty": "10", "unit_price": "10.00", "tax": held["id"]})
spo = owner.call("erp_procurement", "get_po", {"id": sp["id"]})["po"]
check("a purchase order with VAT less withholding: subtotal 100, tax 5, total 105",
      d(spo["subtotal"]) == Decimal("100") and d(spo["tax_total"]) == Decimal("5") and d(spo["total"]) == Decimal("105"), spo)
so3 = owner.call("erp_selling", "create_so", {"company": CO, "customer": customer["id"], "currency": "USD", "order_date": D0, "warehouse": WH})
owner.call("erp_selling", "add_so_line", {"so": so3["id"], "item": widget["id"], "qty": "2", "unit_price": "100.00", "tax": comp["id"]})
so3 = owner.call("erp_selling", "get_so", {"id": so3["id"]})["so"]
check("a sales order with compound VAT + levy: subtotal 200, tax 31, total 231",
      d(so3["subtotal"]) == Decimal("200") and d(so3["tax_total"]) == Decimal("31") and d(so3["total"]) == Decimal("231"), so3)

passed = sum(1 for _, ok in results if ok)
print(f"\n{passed}/{len(results)} checks passed (run {RUN})")
sys.exit(0 if passed == len(results) else 1)
