# ServePoint — Complete Business Logic (implementation-independent)

**Purpose:** everything you need to rebuild ServePoint from scratch on a different stack
(Tauri/Rust/web UI) without reading a line of the Java. This is the behaviour, the
arithmetic, the state machines and the invariants — not the code.

**Sources:** distilled from the shipped Java implementation (265 passing tests),
[07-specification-v2.md](07-specification-v2.md), [08-data-model.md](08-data-model.md),
[04-decisions.md](04-decisions.md) and [06-shift-report-design.md](06-shift-report-design.md).
Where the shipped code and the specification differ, **the code is described here**, and the
divergence is flagged.

---

# 0. What this system is

An offline-first, licensed desktop POS for a bar/club. One Windows machine, no network, no
server, no multi-terminal. Deployment target: Tonic Family Club, Mekelle, Ethiopia. Currency
ETB. About four tables plus counter, three waiters, one cashier per night.

## 0.1 The organising idea

**The paper is the authority.** The bartender releases drinks because a printed slip says so.
The software's job is to produce trustworthy slips, remember exactly what it authorised, and
track who owes whom.

**The person holding the cash is accountable for it.** The customer pays the waiter, not the
till. The waiter later hands cash to the cashier. Those are two separate events and the
system models them separately.

## 0.2 The night, end to end

```
waiter takes an order verbally
        │
cashier records it on an open tab
        │
BAR ISSUE RECEIPT (BR-) prints ───────► bartender keeps it, releases drinks
        │                                        │
        │                                  inventory decreases HERE
        │
   (more rounds, more slips)
        │
customer asks for the bill
        │
cashier CLOSES the tab ──────────────► liability now sits on the WAITER
        │                               (nothing has entered the drawer)
CUSTOMER RECEIPT (CR-) optional, on request — the fiscal document
        │
waiter collects from the customer
        │
waiter RECONCILES with the cashier ──► cash enters the DRAWER
        │
shift closes, immutable report printed
```

## 0.3 Roles

| Role | Logs in? | Notes |
|---|---|---|
| **Cashier** | Yes — code + PIN, slow hash (bcrypt cost 12) | The only system user. Stamped on every transaction. |
| **Waiter** | No | A master record, not a user. Every tab belongs to exactly one. |
| **Bartender** | No | Outside the system entirely. Receives slips, releases drinks. |
| **Owner** | No | Reads the shift report against the spike of returned paper. |

**There is deliberately no manager permission layer.** No manager PIN gates voids,
corrections, price changes or stock adjustments. One person operates the machine; a prompt
the same person always satisfies is theatre. The compensating controls are:

1. **The physical receipt requirement** (§6.4) — preventive.
2. **The shift report** (§9) — detective, and the primary one.

This is an accepted risk, recorded as such. A cashier can void an order and pocket the cash;
the design makes it *visible the next morning*, not *impossible tonight*. **Do not "fix" this
by adding a permission layer** without the owner's decision — and if you do, the shift report
still has to keep every exception itemised.

---

# 1. Numeric and temporal conventions

These are non-negotiable. Every one of them was chosen against a specific failure.

| Concept | Representation | Notes |
|---|---|---|
| **Money** | Signed 64-bit integer, **minor units** (santim). `12.50` is `1250`. | Never float, never decimal-as-string. `f64` cannot hold 0.10; a night of order lines drifts. In Rust: `i64` newtype. |
| **Quantity** | Signed 64-bit integer, **thousandths of a base unit** ("milli"). `2.5 shots` is `2500`, `1 bottle` is `1000`. | Recipes need fractions (half a bottle of tonic). Three decimal places is far finer than anyone can pour. |
| **Rates** | Integer **basis points**. 15% is `1500`, 10% is `1000`. 100% = `10_000`. | No floating percentages anywhere. |
| **Timestamps** | Integer **UTC epoch milliseconds**. | Local time is a display concern only. |
| **Business date** | `TEXT` `YYYY-MM-DD`, derived at write time, **never recomputed**. | See §1.3. |
| **Booleans** | Integer 0/1 with a CHECK. | |
| **Enums** | `TEXT` with a CHECK constraint. | Deliberately readable in a DB browser during a support call. |
| **IDs** | Integer surrogate primary key. Human-facing codes (`TAB-000421`, `BR-000123`) are separate columns. | |
| **Soft delete** | Does not exist. Business records are never deleted; `active` flags control visibility. | |

## 1.1 Money arithmetic (exact definitions)

All rounding is **half-up on the absolute value**, so positive and negative amounts round
symmetrically.

```
percentage_of(amount, bp):
    scaled  = |amount| * bp
    rounded = (scaled + 5000) / 10000          # integer division
    return  sign(amount) * rounded

net_of_tax_at(gross, bp):                       # gross / (1 + rate)
    divisor = 10000 + bp
    scaled  = |gross| * 10000
    rounded = (scaled + divisor/2) / divisor
    return  sign(gross) * rounded

tax_included_at(gross, bp) = gross - net_of_tax_at(gross, bp)
```

**`tax_included_at` is defined by subtraction, never by `net * rate`.** Two independent
roundings do not reconstruct the whole. This was a real bug: a menu price of 1000.00 billed
the customer 1000.01, and nobody at the till can answer that question.

Use checked arithmetic everywhere (`checked_add`/`checked_mul`, panic or error on overflow).
The Java uses `Math.addExact` throughout; silently wrapping money is worse than crashing.

## 1.2 Quantity helpers

```
split_by_pack(milli, units_per_pack_milli) -> (whole_packs, remainder_milli)
```

75 000 milli-shots of gin at 24 000 milli-shots per bottle reads back as "3 bottles + 3
shots" — how someone standing at the shelf actually counts it.

## 1.3 The business calendar

A club trades across midnight, so the calendar day is the wrong unit. A sale at 02:00 Saturday
belongs to **Friday night's** takings.

```
business_date_for(instant):
    local = instant in machine's local zone
    if local.time < shift.day_start:  return local.date - 1 day
    else:                             return local.date

expected_start(date) = date @ day_start
expected_end(date)   = (day_end > day_start ? date : date + 1 day) @ day_end
is_overdue(date, now) = now > expected_end(date)
```

**Only `day_start` decides which business date an instant falls in.** `day_end` is advisory —
it is what makes a shift "overdue" and still open at nine in the morning (the night someone
forgot to close). Keeping date assignment to a single boundary means *every* instant maps
somewhere, including the hours when the club is shut and someone is entering a delivery or
counting stock.

Defaults: `day_start = 18:00`, `day_end = 06:00`.

## 1.4 Sequences

Four named sequences: `TAB`, `SHIFT`, `ISSUE_RECEIPT`, `CUSTOMER_RECEIPT`.

**A number must be allocated inside the same transaction as the row that consumes it.**
Read-then-write leaves a window where the same number is handed out twice — for receipts that
means two customers holding paper with the same number.

```sql
UPDATE sequences SET next_value = next_value + 1 WHERE name = ? RETURNING next_value - 1
```

Increment and read are one statement. Format: `BR-000001`, `CR-000001`, `TAB-000421`
(zero-padded to 6).

---

# 2. Catalogue: products, sale items, recipes, prices

## 2.1 The separation

**What you count is not what you sell.**

```
PRODUCT     the physical thing counted on the shelf, held in a base unit
SALE ITEM   what appears on the menu
RECIPE      sale item ──► product(s) × quantity in base units   (the BOM)
PRICE       sale item ──► money, effective-dated
```

Example: one product `Gin` (base unit SHOT, 24 shots per bottle) carries three sale items:
"Gin, bottle" (recipe: 24 shots), "Gin, shot" (recipe: 1 shot), and "Gin & Tonic" (recipe: 2
shots gin + 0.5 bottle tonic).

**Every sale item has a recipe — no exceptions.** A beer is a one-line recipe consuming one
bottle. A shot is a one-line recipe consuming one shot. A cocktail has several lines. One
code path, no special cases. This is why adding cocktails was never structural work, and it
is why the inventory layer never learns that shots or cocktails exist.

## 2.2 Product attributes

| Field | Meaning |
|---|---|
| `code`, `name`, `category` | Identity |
| `base_unit` | `BOTTLE` \| `SHOT` \| `UNIT` |
| `base_units_per_pack` | milli. 1 bottle = 24 shots → `24000`. A beer bottle → `1000`. |
| `units_per_purchase_pack` | Default 1. Crate provisioning, invisible in the UI today. |
| `low_stock_threshold_milli` | **Per product.** One global threshold is meaningless across beer and premium spirits. |
| `tracks_inventory` | False → the item appears on the order and the slip but produces **no stock movement**. Food will land here. |
| `destination` | `BAR` \| `KITCHEN`. Drives receipt splitting (§6.6). |
| `avg_cost_minor` | Weighted average cost. **Derived cache**, recomputed on each purchase. |
| `active` | Never deleted; deactivated. |

### The conversion factor is an assumption, not a fact

750 ml ÷ 30 ml is 25 in theory; real yield is 23–24 after spillage and over-pour. Set it high
and stock runs permanently short; set it low and phantom stock appears. Start theoretical,
then use the **yield variance report** (§8.3) to find the club's real number. Shot-level stock
will never reconcile exactly, and that is expected rather than a defect.

## 2.3 Recipes are versioned, never edited

Editing a recipe closes the current version (`effective_to = now`) and opens a new one.
**Order lines snapshot `recipe_id`**, so a historical order always expands through the recipe
that was actually poured against, not today's.

## 2.4 Prices are effective-dated, and order lines snapshot them anyway

A price change tonight cannot restate what a customer was charged last night. The effective-
dated `prices` table is for lookup and audit; historical totals come from the order line's
own `unit_price_minor`.

## 2.5 Recipe expansion (the one function everything routes through)

```
expand(recipe, item_quantity) -> [(product_id, quantity_milli)]:
    totals = {}
    for line in recipe.lines:
        totals[line.product_id] += line.quantity_milli * item_quantity   # summed, not replaced
    drop any product where tracks_inventory = false
    return totals
```

Summing rather than overwriting matters: a recipe naming the same product twice (a double
measure written as two lines) must not produce two competing requirements.

```
expand_order(order) = merge over all order lines of expand(line.recipe, line.quantity)
```

## 2.6 Availability check

```
check_availability(required_by_product) -> [Shortfall]:
    for (product, required) in required_by_product:
        on_hand = SUM(stock_movements.quantity_milli WHERE product_id = product)
        if on_hand < required: emit Shortfall{product, required, on_hand}
```

**Checked on the aggregate across the whole order, never line by line.** A round drawing one
whole bottle of rum plus four shots from it passes both lines individually when one bottle is
on the shelf; the order does not fit. Only the aggregate answers the question being asked.

---

# 3. Inventory

## 3.1 Stock on hand is never stored

```
stock_on_hand(product) = SUM(stock_movements.quantity_milli WHERE product_id = ?)
```

There is no mutable stock column. A cached quantity is a number that drifts away from the
transactions that produced it — that was the prior prototype's core failure. Same reasoning
governs waiter held balances (§7.4).

## 3.2 The movement ledger

Append-only. No update, no delete — enforced by database trigger, not convention.

| Type | Sign | Source | Emitted when |
|---|---|---|---|
| `SALE` | negative | `order_id` | An order reaches ISSUED |
| `RETURN` | positive | `order_id` | A correction/void records stock the bartender signed back |
| `PURCHASE` | positive | `purchase_id` | A delivery is received |
| `ADJUSTMENT` | signed | `stock_count_id` | A stock count is applied |
| `DAMAGE` / `LOSS` | negative | none | Explicit manual entry — the only source-less movements permitted |

Every movement carries `unit_cost_minor`, `reason`, `shift_id` (nullable — deliveries and
counts happen while the club is shut), `created_at`, `created_by`.

## 3.3 **Inventory decreases when the issue receipt prints**

Not when payment occurs. The print is the moment the system can observe, and it stands as the
proxy for physical release of the drinks.

## 3.4 ★ Corrections adjust the bill; returns adjust the stock

**This is the single most important rule in the inventory model, and the one most likely to
be "fixed" by mistake.**

A correction does **not** reverse the original stock movement. The original sale stands — the
bottles genuinely left the shelf. What physically came back is recorded as a separate
`RETURN`, in the quantities the bartender wrote on the back of the receipt and signed.

```
receipt says 5 beers, 2 come back:

  SALE     -5     (stands, never reversed)
  RETURN   +2     (what the bartender signed for)
           ───
  net      -3     (matches physical reality)

corrected order:  3   (what the customer is billed)
```

Stock movements record **what physically happened**; the order records **what is owed**.
Inventory stays truthful even if the billing correction is wrong, and one mechanism covers
every case — a correction caught before the bartender poured is simply a full return.

**Stock not returned gets no movement at all.** Those bottles are already correctly gone via
the original sale; posting a `LOSS` as well would remove the same drinks twice. The difference
is recorded as a **write-off** on the correction line and surfaces on the shift report,
because it is *revenue* lost, not *stock* lost.

**A replacement order must never post its own sale.** This was a real shipped bug: correcting
5 beers to 3 removed 6 from stock. The correction posts only the *difference*.

## 3.5 Stock counts

Performed **between shifts only** — counting while sales continue produces a variance that
measures nothing, because the shelf moves under the counter's hands. The system refuses to
apply a count while a shift is open.

```
apply_count(counted[]):
    require no open shift
    require products distinct, quantities non-negative
    for each entry:
        system     = stock_on_hand(product)
        difference = counted - system
    ── one transaction ──
    insert stock_count (status = DRAFT)
    insert one stock_count_line per product (system, counted, variance)
    for each non-zero difference:
        post ADJUSTMENT movement of `difference` valued at product.avg_cost, linked to count
    UPDATE stock_counts SET status = 'APPLIED'          # frozen from here on
    audit
```

Both directions post identically — an unexplained gain is as much a signal as an unexplained
loss.

## 3.6 Zero stock

Governed by `inventory.allow_negative` (default **false**).

- **false** — the check runs *before* the receipt number is allocated, so a refused round
  burns nothing from the BR sequence and leaves no order to recover. The order is rejected
  with a per-product detail message.
- **true** — the sale proceeds into negative stock. The original reasoning still holds: the
  bar shelf is not the store, and a bartender who can physically pour should not be stopped by
  a bookkeeping figure. But negative counts hid genuine stock errors, so the club asked for
  refusal by default.

---

# 4. Shifts

One shift per business night. **No concurrent shifts, no multiple shifts per day, no split on
cashier handover.**

```
OPEN ──► CLOSING ──► CLOSED
```

`business_date` is **unique** across shifts, and only one shift may be `OPEN` at a time
(partial unique index at the database level, not trusted to application code).

Every transaction stores `shift_id` and a timestamp.

## 4.1 Open

```
open(cashier, opening_float):
    reject if any shift is active
    business_date = calendar.current_business_date()
    reject if that business date has already traded
    ── one transaction ──
    insert shift (OPEN, business_date, opened_at, opened_by, opening_float)
    if opening_float > 0: insert cash_movement OPENING_FLOAT +opening_float
    audit SHIFT_OPENED
```

**The opening float is the first cash movement, not a separate concept.** Expected cash is a
sum over one ledger; holding the float outside it would mean two numbers to reconcile
instead of one.

## 4.2 Begin closing

`OPEN → CLOSING`. New orders stop; the drawer is counted and waiters settle. Separating this
from the final close gives the cashier a state to work in without the till still taking sales
behind them.

**Print recovery must be complete before this transition** — checked *before* trading is
disabled. A failed issue slip may need to return to `DRAFT` and be retried, and moving to
`CLOSING` first would make that retry impossible and leave the night permanently unable to
close.

## 4.3 Close

```
close(cashier, counted_cash):
    ── one transaction ──
    require print recovery complete
    UPDATE shift SET status=CLOSED, closed_at, closed_by, counted_cash
    build the shift report from the now-closed shift
    render it
    INSERT shift_reports (report_json, rendered_text, generated_at, generated_by)
    audit SHIFT_CLOSED
```

**Closing the shift and storing its report are one all-or-nothing commit.** If report
rendering or storage fails, the shift stays `CLOSING`. It must never be possible to commit a
closed night with its sole fraud-control document missing.

## 4.4 The print-recovery gate

```
recovery_complete ⟺
    0 orders with status = 'PRINTING'
  AND 0 orders with status = 'DRAFT' that have an ISSUE receipt with status = 'VOID'
  AND 0 receipt_prints rows with outcome = 'UNKNOWN'
```

The second clause catches "recovered drafts" — an order returned to draft after a confirmed
non-print, which still needs a human to retry or abandon it.

## 4.5 What does NOT block the close

**Open tabs and unreconciled waiter balances do not block closing.** Both legitimately carry
over: a customer still drinking at close is normal, and so is a waiter who has not settled.
They are **displayed for acknowledgement** (count, value, unreconciled cash) so nothing is
carried forward silently, but they are not obstacles.

## 4.6 Overdue warning

`is_overdue()` compares now against `expected_end(business_date)`. Required for the night
someone forgets to close and goes home.

---

# 5. Tabs

## 5.1 Identity and reference

Every tab has an immutable sequential internal code (`TAB-000421`) plus a human-facing
**reference** built from a configurable mode:

| Mode | Label |
|---|---|
| `TABLE` | `"Table " + table_no` |
| `CUSTOMER_NAME` | `customer_name` |
| `CUSTOMER_PHONE` | `customer_name` or `"name (phone)"` if phone present |
| `CUSTOM` | `custom_ref` |

**Store `table_no`, `customer_name`, `customer_phone`, `custom_ref` as separate optional
columns plus a computed `display_label`, and store the mode on the tab itself.** Never
reinterpret one polymorphic field when the setting changes, or historical data becomes
ambiguous.

The label must be **unique among OPEN tabs** — two open tabs sharing a reference makes the
cashier's search ambiguous at exactly the wrong moment. Reuse after close is fine; a table
serves many parties in a night.

## 5.2 Lifecycle

```
OPEN ──────► CLOSED ──────► RECONCILED
accepting    bill final,    covered by a
orders       liability on   waiter
             the waiter     reconciliation
```

- **Tabs never auto-close and may remain open indefinitely, across nights.** Shift close does
  not resolve them.
- **A closed tab is never reopened.** Enforced by trigger: `CLOSED|RECONCILED → OPEN` aborts.
  If the customer orders again, a new tab is created. This removes voiding fiscal documents,
  reissued receipts, and any ambiguity about which receipt is authoritative. Accepted cost:
  two receipts for one visit.
- Because tabs persist indefinitely, the outstanding total is effectively **accounts
  receivable**. An outstanding-tabs report by waiter and by **age** is required, or abandoned
  tabs accumulate invisibly.

## 5.3 Running total

```
running_total(tab) = SUM(order_lines.line_total_minor)
                     over orders WHERE tab_id = ? AND status = 'ISSUED'
```

`DRAFT`, `PRINTING`, `REPLACED`, `VOIDED` and `ABANDONED` orders contribute nothing. This one
predicate is what keeps corrections from double-counting revenue.

## 5.4 Transfer

A tab may be transferred to another waiter when one leaves mid-shift. The tab must still
accept orders (status `OPEN`).

**Orders already issued keep their original waiter** — that is what actually happened, and
rewriting it would falsify the record. What moves is responsibility for collecting. So "orders
issued by A" and "tabs A must settle" are deliberately different figures. The transfer is
recorded in an append-only `tab_transfers` table.

## 5.5 Search

By customer name, phone, table number, custom reference, waiter, or internal tab code — every
reference field, so staff need not recall which mode was in force when the tab was opened.

---

# 6. Orders and printing

## 6.1 Order contents

Products (via sale items), quantities, **unit price at sale time**, **recipe as applied**,
waiter, cashier, timestamp, shift, status. Plus `replaces_order_id` and `root_order_id` for
correction chains.

Each order line snapshots `sale_item_name`, `recipe_id` and `unit_price_minor` at creation, so
a renamed item, an edited recipe or a price change can never rewrite history.

## 6.2 Status machine (trigger-enforced)

```
DRAFT     ──► PRINTING | ABANDONED
PRINTING  ──► ISSUED | DRAFT           (DRAFT = retry after a confirmed non-print)
ISSUED    ──► REPLACED | VOIDED
REPLACED  ──► (terminal)
VOIDED    ──► (terminal)
ABANDONED ──► (terminal)
```

Any other transition aborts at the database. Orders are never deleted.

A **draft holds no receipt number** and may be abandoned freely. "Orders are never edited"
applies to *issued* orders.

## 6.3 ★ Issue: the three-transaction print protocol

This is the most delicate code in the system. **The `PRINTING` state commits before anything
reaches the printer, on purpose.**

```
issue(order, cashier):

  PRE-CHECKS (no transaction)
    require an open shift
    require order.status = DRAFT
    require tab OPEN
    if order.shift_id ≠ current shift:
        carry the draft into the current shift (own transaction + audit)
        — a failed-print draft is not a sale yet, so a later retry belongs to tonight
    if posting stock and NOT inventory.allow_negative:
        require availability for the whole expanded order  ← before any number is burned

  ── TRANSACTION 1 ──  (commits BEFORE the printer is touched)
    re-verify tab is still OPEN
    seq    = next(ISSUE_RECEIPT)
    number = "BR-" + pad6(seq)
    INSERT receipt (type=ISSUE, number, seq, order_id, destination, status=PENDING,
                    waiter_name stamped, shift_id)
    UPDATE order SET status = PRINTING
  ── COMMIT ──

  ── TRANSACTION 1b ──  (render frozen before device I/O)
    render the slip text
    UPDATE receipt SET rendered_text = ?  WHERE rendered_text IS NULL AND status = PENDING
      · immutable once set — a retry must never pick up changed settings, a renamed
        item, or a different waiter name
      · re-storing byte-identical text is a safe no-op (idempotent retry)
  ── COMMIT ──

  DEVICE I/O — send bytes to the printer

  if the device reports FAILURE:
      the number stays allocated, the order stays in PRINTING
      return (number, printed = false) — the cashier retries or resolves
      ⚠ never create a second order for the same round

  ── TRANSACTION 2 ──  (only after the device reports success)
    re-verify tab OPEN, receipt still PENDING for this order
    UPDATE receipt SET status = PRINTED, printed_at = now
    INSERT receipt_prints (print_no = 1, outcome = SUCCESS, shift_id, created_by)
    UPDATE order SET status = ISSUED, issued_at = now
    if posting stock: for each (product, qty) in expand_order(order):
        INSERT stock_movement SALE  −qty  linked to order_id
    audit ORDER_ISSUED
  ── COMMIT ──

  if TRANSACTION 2 fails after the device reported success:
      raise IssuePrintPending — paper may be in the bartender's hand.
      The order stays in PRINTING and MUST go through recovery. Treating this as an
      ordinary error would let the cashier create a second order while the first slip
      is already authorising drinks.
```

**Why two transactions.** A power cut leaves the order visibly stranded in `PRINTING`, and the
cashier can be asked on restart whether the slip emerged. One transaction would roll back to a
draft while a slip may already be in the bartender's hand, authorising drinks the system has
forgotten.

### Resolving a stranded print (startup gate, non-dismissible)

For every order in `PRINTING`, the cashier is asked whether that exact numbered slip emerged:

| Answer | Effect |
|---|---|
| **Yes, it printed** | Run Transaction 2 above (`confirm_issued`) |
| **No, nothing printed** | receipt `PENDING → VOID`; order `PRINTING → DRAFT`; audit `PRINT_ABANDONED`. **The number is retained as VOID, never reused**, so the sequence is gapless and every number is accounted for. |
| **Printer is dead, I wrote a chit** | receipt `PENDING → FAILED`; `receipt_prints` print 1 outcome `FAILED` with reason "Printer failed; handwritten chit authorised"; order → `ISSUED`; **stock posts normally**; audit `ORDER_ISSUED_HANDWRITTEN`. |

The handwritten path is not a guess about an ambiguous device result — the cashier first
confirms no generated slip emerged, then explicitly substitutes the handwritten chit. The
failed original attempt stays durable forever and appears itemised on the shift report.

## 6.4 ★ Correction and void require the paper

**A cashier may not void or correct without the printed issue receipt in hand.**

```
bartender writes what came back on the back of the slip, signs it
        │
waiter carries it to the cashier
        │
cashier voids or corrects, TYPING the receipt number
```

The receipt number must be **typed, not picked from a list**, and it is **validated** against
the database: it must be an `ISSUE` receipt for that exact order with status `PRINTED` or
`FAILED`. Merely recording whatever text was entered would make the control cosmetic — an
invented number would be accepted and the spike of returned receipts could no longer be
reconciled against the exception report.

The bartender's signed note supplies the physical disposition the system otherwise could not
know, which is exactly what makes §3.4 possible.

Returned receipts are spiked and retained for the owner's check.

## 6.5 Correction

### Validation

- Order status must be `ISSUED`.
- **The order must belong to the currently open shift.** An order in a closed shift cannot be
  corrected — including one on a still-open tab. After close, the remedy is a stock adjustment
  and a written note; money already banked is never restated. This keeps every shift report
  self-contained and eliminates prior-period corrections entirely.
- The tab must still be `OPEN`.
- Typed BR number must validate (§6.4).
- A reason is mandatory.
- At least one line — a correction to zero lines is a void, not a correction.
- Correcting an already-`REPLACED` order is forbidden (partial unique index on
  `replaces_order_id`), or the chain branches and revenue double-counts.

### Resolving the replacement's lines

**A correction fixes what was written on an existing slip; it is not a new sale at today's
catalogue terms.**

- A sale item **already on the original order** keeps the original's frozen name, `recipe_id`
  and `unit_price` — only the quantity changes.
- A sale item **new to the order** resolves against the current recipe and current price.

### Computing the stock delta (frozen before any paper moves)

```
before = expand_order(original)
after  = expand_lines(new_lines)
returned_by_product = the bartender's signed quantities

for each product appearing in before ∪ after ∪ returned:
    delta = after[p] - before[p]

    if delta >= 0:
        returned[p] must be zero  → else reject
          ("returned stock is only valid for a product removed from the bill")
        if delta > 0: emit ADDITIONAL SALE of delta
        written_off = 0

    if delta < 0:
        reduction = -delta
        returned[p] must be <= reduction → else reject ("more returned than removed")
        if returned[p] > 0: emit RETURN of returned[p]
        written_off = reduction - returned[p]      ← NO stock movement, ever
```

`written_off` is stored on `order_correction_lines` and reported. A database trigger enforces
`returned + written_off = the reduction` on every correction line.

### The transaction protocol

```
  ── TRANSACTION 1 ──  (freeze everything needed to finish, before paper)
    INSERT replacement order as DRAFT
           (replaces_order_id = original.id, root_order_id = original.root)
    INSERT pending_order_corrections (original, replacement, typed BR number,
                                      reason, shift, cashier)  — frozen intent
    INSERT pending_order_correction_lines (deltas, returned, written_off, notes)
    seq/number = next(ISSUE_RECEIPT); INSERT receipt PENDING for the replacement
    UPDATE replacement SET status = PRINTING
  ── COMMIT ──

  render + persist rendering; DEVICE I/O

  if failure or ambiguity → raise CorrectionPrintPending.
      The ORIGINAL ORDER STAYS ISSUED. The complete correction intent is durable and
      recovery will finish or abandon it. Never kill the original before the new slip
      is out.

  ── TRANSACTION 2 ──  (one atomic transition — all of it, or none)
    re-verify: original still ISSUED, shifts match, tab open, receipt still PENDING
    UPDATE receipt SET status = PRINTED; INSERT receipt_prints #1 SUCCESS
    UPDATE replacement SET status = ISSUED
    INSERT order_corrections (type = CORRECTION, original, replacement,
                              typed BR number, reason, shift, cashier)
    apply the frozen deltas: SALE / RETURN movements + order_correction_lines rows
    UPDATE original SET status = REPLACED
    DELETE the pending intent
    audit ORDER_ISSUED + ORDER_CORRECTED
  ── COMMIT ──
```

**The replacement never posts its own full sale.** Only the deltas move stock.

### Abandoning a correction whose slip did not print

```
receipt PENDING → VOID
replacement PRINTING → DRAFT
UPDATE replacement SET replaces_order_id = NULL      ← releases the unique chain slot
replacement DRAFT → ABANDONED
DELETE the pending intent
audit CORRECTION_ABANDONED
```

The original remains `ISSUED` and untouched; the correction can be attempted again. No
historical order row is ever deleted.

### Correction chains

Each order carries `replaces_order_id` and `root_order_id`. A chain A→B→C needs the root to
identify the family and the link to walk it. **Only the latest non-replaced, non-voided order
in a chain counts toward any total.** Exactly one non-`REPLACED` leaf per chain; no order may
be replaced twice.

## 6.6 Void

Mechanically a correction to nothing: `before = expand_order(order)`, `after = {}`.

```
  ── ONE TRANSACTION ──
    validate exactly as for a correction (issued, same shift, tab open, typed BR, reason)
    INSERT order_corrections (type = VOID, replacement = NULL, BR number, reason)
    apply deltas: RETURN for what came back; NO movement for the rest; write-offs recorded
    UPDATE order SET status = VOIDED, void_reason, voided_at, voided_by
    audit ORDER_VOIDED
```

A full void is a full return. The original sale is never reversed. Voided records are
permanent.

## 6.7 Two documents, two purposes

| | Issue receipt `BR-` | Customer receipt `CR-` |
|---|---|---|
| Purpose | Authorises the bartender to pour | Customer's record of payment |
| Audience | Internal — bartender keeps it | Customer |
| Fiscal status | **None** | **The fiscal document** |
| Quantity | One per order **per destination** | **One per tab, consolidated** |
| Prices shown | **No** — products and quantities only | Yes, with tax and service charge |
| Printing | Automatic, part of the order flow | **Optional, on request** |

### Issue receipt content

Marked prominently `BAR ISSUE RECEIPT` / `NOT A CUSTOMER RECEIPT`. Carries: receipt number,
tab code, tab reference, waiter name, products, quantities, time. Footer: "Bartender: keep
this slip".

**Deliberately shows no prices** — a slip with no money on it cannot be mistaken for a bill
if a customer is handed one by accident.

A replacement slip prints `REPLACES BR-000123 — PREVIOUS SLIP IS VOID`, so the bartender
knows the paper they hold is dead.

### Destination splitting — build this on day one

The receipt engine **groups order lines by product destination and emits one document per
group**. Today everything routes to `BAR`, so exactly one slip prints and nothing looks
different. Two beers and a burger would produce **two slips**.

Retrofitting this later would touch receipt numbering, the print queue and crash recovery —
the most delicate code in the system. Build it now even though food is out of scope.

### Customer receipt content

Configurable header (business name, address, phone, TIN), receipt number, tab code, tab
reference, date, time, **waiter name and cashier name**, all consolidated lines with prices,
subtotal, service charge (with rate), tax (with rate), total, `COMPLIMENTARY` marker if
comped, configurable footer.

- Consolidated across **every order on the tab across its whole life**, which may span nights.
- Includes only final-state orders: voided excluded, replaced excluded with their replacements
  in their place, latest-in-chain only.
- **Names are stamped at issue, not looked up later** — renaming or deactivating a staff member
  must not alter a receipt already in a customer's hands.
- The waiter is whoever owns the tab **at close**; the cashier is whoever **closed it** (not
  whoever happens to print the receipt days later).

## 6.8 Numbering rules

- **`BR-` numbers have no fiscal significance.** Allocated at `PRINTING`. If abandoned,
  retained with status `VOID` so the sequence is gapless and every number is accounted for. A
  gap here would be an operational curiosity; there are none.
- **`CR-` numbers are allocated only when a receipt is actually produced.** Every number in the
  fiscal sequence corresponds to a real document handed to a customer. Tabs closed without
  printing consume no number. A customer asking days later gets a first print, numbered then.

This split largely dissolves the print-failure risk on the sequence that matters: the fiscal
sequence is printed deliberately, not automatically mid-service.

## 6.9 Customer receipt print protocol

```
prepare_customer_receipt(tab, cashier):
    require an open shift
    require tab is CLOSED (or RECONCILED) — never OPEN
    if a customer receipt already exists:
        if PRINTED → reject; use the reprint path
        else       → re-prepare a retry attempt on the SAME number
    ── ONE TRANSACTION ──
      read the FROZEN bill from tab_payments (never recompute from current settings)
        · refuse if charge_rates_known = 0 and service or tax is non-zero
          (a legacy row would print a false 0% fiscal receipt)
      seq/number = next(CUSTOMER_RECEIPT)
      INSERT receipt (type=CUSTOMER, tab_id, PENDING, waiter_name, cashier_name = the
                      staff name of tab.closed_by, all money values, all rates as applied,
                      shift_id = the CLOSING shift)
      render → UPDATE receipt SET rendered_text
      INSERT receipt_prints (print_no = 1, outcome = UNKNOWN, shift_id = attempt shift)
    ── COMMIT ──
    DEVICE I/O
    resolve(number, print_no, printed?) → outcome SUCCESS or FAILED;
        receipt → PRINTED (+printed_at) or FAILED
```

**The attempt is recorded as `UNKNOWN` before the printer is touched.** A power cut after
paper emerges therefore leaves both the exact bytes and an explicit recovery question; it can
never leave a bare fiscal number with no reproducible document.

## 6.10 Reprints

A reprint creates **no new order, no new number, no new stock movement**. Same number, another
row in `receipt_prints`, with reason and count recorded. `rendered_text` stores exactly what
was sent, so a reprint months later reproduces the original rather than re-rendering against
changed settings.

- **Issue-slip reprint:** allowed when the receipt is `PRINTED` or `FAILED`, requires a reason,
  and is blocked while any earlier reprint attempt is still `UNKNOWN`. The copy is prefixed:
  `*** BAR ISSUE RECEIPT REPRINT #n *** / IF YOU ALREADY HOLD THIS SLIP, DISCARD THIS COPY`.
  If a `FAILED` (handwritten) issue later reprints successfully, the receipt advances to
  `PRINTED` while the failed original attempt is kept forever.
- **Customer receipt reprint:** only when already `PRINTED`, requires a reason, prefixed
  `*** CUSTOMER RECEIPT REPRINT #n ***`.
- **The two are counted and reported separately** — reprinting a fiscal document is a different
  kind of event from reprinting an internal slip.

**The house rule for the bartender:** *if you already hold a slip with this number, discard
the new one.* Safe in both the never-printed and already-printed cases, without the system
needing to know which occurred.

## 6.11 Printer failure

**Hardware failure is a business risk, not a software problem.** Nothing in the application
can make a dead printer print; the real mitigation is a spare printer on the shelf.

**The one decision that is ours: when printing fails, the order is still recorded.** The
system must never refuse to accept an order because it cannot print the slip — that turns a
hardware fault into a total trading stoppage. The order is written, the slip is marked
unprinted, and the club continues on handwritten chits. Unprinted slips can be printed later
if the printer returns, or left unprinted and visible on the shift report.

## 6.12 Encoding

Most thermal printers support only CP437/CP850-family codepages from font ROM. Latin-only
receipts have been confirmed with the client, so there is no font constraint — but verify
against the actual printer before finalising any layout.

Receipt width comes from `receipt.chars_per_line` (default 48 for 80 mm paper; 58 mm is 32).
The renderer lays out against that number rather than a hardcoded column count.

---

# 7. Money

## 7.1 The chain

```
customer ──pays──► waiter ──reconciles──► cashier ──► drawer
```

**Closing and reconciliation are different events with different meanings.** Closing finalises
what is owed; reconciliation settles who has the money.

| Event | Records | Effect |
|---|---|---|
| **Close** | What is owed | Amount joins the waiter's **held balance**. Nothing enters the drawer. **No payment method.** |
| **Reconciliation** | How the money arrived | Held balance reduced; **cash** enters the drawer |

Liability accrues **at close** — from the moment the waiter walks away with the receipt, they
owe that amount.

## 7.2 The bill calculator

Both tax and service charge are optional. **Service charge is taxable**, which fixes the order
of operations. Rounding is applied **once on the accumulated line total, never per line** —
rounding each line and summing drifts against a bill the customer can add up themselves.

```
INPUT: line_total = Σ order_lines.line_total_minor over ISSUED orders on the tab

CASE tax disabled:
    service  = service_enabled ? percentage_of(line_total, service_bp) : 0
    tax      = 0
    net      = line_total
    total    = line_total + service

CASE tax enabled, EXCLUSIVE (menu prices exclude tax):
    service  = service_enabled ? percentage_of(line_total, service_bp) : 0
    taxable  = line_total + service
    tax      = percentage_of(taxable, tax_bp)
    net      = line_total
    total    = taxable + tax

CASE tax enabled, INCLUSIVE (menu prices already contain tax):
    net           = net_of_tax_at(line_total, tax_bp)
    tax_on_lines  = line_total - net              ← BY SUBTRACTION, never net * rate
    service       = service_enabled ? percentage_of(net, service_bp) : 0
    tax_on_service= percentage_of(service, tax_bp)
    tax           = tax_on_lines + tax_on_service
    total         = net + service + tax
```

Note in inclusive mode the receipt's "Subtotal" line shows the **extracted net**, not the menu
line total. Tax is extracted from the lines while being *added* to the service charge — the
fiddliest of the four combinations and the one worth testing hardest.

**Both paths must be unit-tested against worked examples agreed with the client.** This is the
kind of arithmetic that silently loses money for a year.

**Whether prices are tax-inclusive is a fact about the business, not a preference.** Set it
wrong and every total is off by the tax rate and looks perfectly correct on screen. It belongs
on the commissioning checklist.

The rates used are **snapshotted onto the transaction as applied**. Changing a setting never
restates history.

## 7.3 Closing a tab

```
close_tab(tab, comped?, comp_reason, cashier):
    require an open shift
    require tab.status = OPEN
    if comped: require payments.comps_enabled AND a non-blank reason
    require NO orders on this tab in DRAFT or PRINTING
      · a tab's bill cannot be frozen while a round awaits print resolution, or a later
        recovery would authorise drinks absent from the immutable close-time bill
    require at least one ISSUED order
    line_total = running_total(tab)
    bill       = calculate(line_total)
    liability  = comped ? 0 : bill.total

    ── ONE TRANSACTION ──
      UPDATE tabs SET status=CLOSED, closed_shift_id, closed_at, closed_by, is_comped
      INSERT tab_payments (tab, waiter, subtotal, service, tax, total, is_comped,
                           comp_reason, liability, tax_rate_bp, service_rate_bp,
                           tax_inclusive, charge_rates_known = 1, shift, created_by)
      audit TAB_CLOSED or TAB_COMPED
```

**`tab_payments` has no payment-method column, and must never gain one.** The method is not
known at close, and because reconciliation is batched across tabs, the method attaches to the
*reconciliation*, not the tab. A per-tab method field could never be filled reliably.

## 7.4 ★ Held balance is derived, never stored

```
held_balance(waiter) =
      Σ tab_payments.liability_minor            WHERE waiter_id = w
    − Σ (cash_minor + non_cash_minor + written_off_minor)
        FROM reconciliations WHERE waiter_id = w AND finalized_at IS NOT NULL
```

A stored balance is a cache that drifts. Same reasoning as stock on hand.

## 7.5 Reconciliation

Batched across tabs, after close — not per tab as the waiter goes. Partial reconciliation
works for free under a running balance.

```
reconcile(waiter, tab_ids[], cash, non_cash, written_off, write_off_reason, cashier):
    require an open shift
    require tab_ids non-empty and distinct
    require all amounts >= 0
    require AT MOST ONE of {cash, non_cash, written_off} is positive
        ← split tender is not supported: one method per settlement
    require a reason if written_off > 0

    expected = Σ over tab_ids of tab_payments.liability
               (each tab must be CLOSED and belong to THIS waiter)
    settled  = cash + non_cash + written_off

    if settled > expected: REJECT
        "Settled X exceeds the Y owed; return the difference to the waiter"

    shortfall = expected - settled

    ── ONE TRANSACTION ──
      INSERT reconciliations (waiter, cashier, expected, cash, non_cash, shortfall,
                              written_off, reason, shift, created_at, finalized_at = NULL)
      for each tab: INSERT reconciliation_tabs (recon, tab, amount = that tab's EXACT
                                                liability)
                    UPDATE tabs SET status = RECONCILED  (from CLOSED)
      if cash > 0: INSERT cash_movement RECONCILIATION +cash, reference = recon id
      UPDATE reconciliations SET finalized_at = now   ← the ONE permitted mutation; seals it
      audit WAITER_RECONCILED
```

Database rules back this up: a tab may appear in **only one** reconciliation ever; each
allocation must repeat that tab's exact immutable liability for the same waiter; nothing may
be appended to a finalized reconciliation.

The reconciliation belongs to the shift in which the cash actually reaches the drawer, which
may be a different night from the one the tab closed in.

### Overages are never booked as income

More cash than owed is almost certainly the waiter's own tip money or a counting error.
**Show the amount owed and prompt to return the difference.** There is no overage column. An
unexplained surplus is as much a control failure as a shortfall.

### Walkouts need no special workflow

A customer leaving without paying produces a reconciliation shortfall — the same path as any
other discrepancy.

### Settling an old shortfall

A separate operation for a balance carried from previous nights:

```
settle_outstanding_balance(waiter, cash, non_cash, written_off, reason, cashier):
    require an open shift; same amount validations
    require NO unreconciled closed tabs remain for this waiter
      ← linking the tabs again would count their liability twice
    expected = held_balance(waiter);  require > 0;  require settled > 0 and <= expected
    same transaction shape, but NO reconciliation_tabs rows
```

## 7.6 ★ Expected cash comes from reconciliations, never from tabs closed

```
expected_cash(shift) = Σ cash_movements.amount_minor WHERE shift_id = ?
```

```
  opening float
+ waiter reconciliations received — CASH PORTION ONLY
− cash paid out (itemised by category)
= expected cash in drawer
```

**This is the single most important line in the specification.** Computing it from sales — the
obvious implementation, and what the prior prototype did — makes the drawer appear short by
the value of every unreconciled tab, every night, indistinguishably from theft.

**Enforce it structurally: there must be no code path from `tab_payments` into
`cash_movements`.** Do not make this a convention someone has to remember.

Only the **cash** portion enters the drawer. A non-cash (mobile money) reconciliation clears
the waiter's liability without adding to expected cash.

## 7.7 Cash movements

| Type | Sign | Notes |
|---|---|---|
| `OPENING_FLOAT` | + | Written at shift open |
| `RECONCILIATION` | + | Cash portion only, references the reconciliation |
| `PAYOUT` | − | **Category is mandatory** — an uncategorised payout is a hole in the control |
| `ADJUSTMENT` | signed | |

Payouts require amount > 0 (stored negated), a non-blank category, and an open shift. A
trigger enforces that payouts are negative and categorised.

Variance = `counted_cash − expected_cash`. A **denomination breakdown** is required at count
time — without it a variance can only be noted, never investigated.

## 7.8 Comps

Disabled by default (`payments.comps_enabled = false`).

When enabled, a comp is a **close-time** decision, because it answers "is this tab
chargeable" — a revenue question. Cash-versus-mobile is a reconciliation-time detail, a
settlement question. **A comped tab never reaches a waiter's balance** (liability = 0); it
would otherwise make the waiter liable for the house's giveaway until they got a chance to
declare it.

A comp requires a non-blank reason (trigger-enforced), carries zero liability (CHECK), and is
**itemised on the shift report with tab, waiter, cashier, value and reason** — the aggregate
alone is not an adequate owner control.

While disabled, a free drink is simply not recorded. The bottles still leave the shelf, so it
surfaces at stock count as unexplained loss, where it looks identical to theft. Acceptable at
the stated volume; the setting exists for when it stops being.

## 7.9 What is deliberately not modelled

| | Why |
|---|---|
| **Tips** | Out of scope. They belong to the waiters, the club takes no share, and because liability is the tab total rather than the cash in a waiter's pocket, tips never enter the arithmetic. |
| **Change floats** | Waiters carry none. Held balance is pure tab liability. |
| **Split tender** | Not supported. One method per settlement. |
| **Partial customer payment** | Subsumed by the shortfall mechanism — a customer paying part simply leaves the waiter short. |
| **Fiscal devices** | Client decision on a legal grey area, recorded as theirs. Keep printing behind an interface, keep tax breakdown fields on every transaction (zero when VAT is off), keep the TIN configurable. If a device is ever mandated, **only the `CR-` path moves to it**. |

---

# 8. Purchasing and costing

## 8.1 Receiving a delivery

**Purchases do not require an open shift.** Deliveries arrive during the day when the club is
shut; refusing to record one because nobody is trading would push the club into entering it
wrong later. `shift_id` is the current shift if one is open, otherwise null.

```
receive(supplier_name, phone, invoice_ref, lines[], actor):
    supplier = find by LOWER(TRIM(name)), preferring active; reactivate a sole inactive
               master if needed; else create
    require lines non-empty and product-distinct
    require invoice_ref unused for this NORMALIZED supplier name
        ← including across retired duplicate legacy supplier ids; prevents the same
          delivery being posted twice
    total = Σ line.total_cost

    ── ONE TRANSACTION ──
      INSERT purchase (supplier, invoice_ref, received_at, shift_id, total, created_by)
      INSERT ALL purchase_lines FIRST                ← freeze the whole invoice
      then for each line:
          update weighted average cost               ← BEFORE posting the movement
          INSERT stock_movement PURCHASE +qty, unit_cost, purchase_id, reason=invoice_ref
      audit PURCHASE_RECEIVED
```

Lines are inserted before any movement posts, because a trigger then rejects any line appended
to an already-posted purchase.

Purchase quantities are in the product's **base units** — shots for spirits, bottles for beer.
A helper converts whole packs: 5 bottles of gin at 24 shots each becomes 120 shots.

## 8.2 Weighted average cost

Recalculated on each purchase, **before** the new movement is written so "on hand" is
genuinely the quantity the existing average applies to.

```
existing_units  = max(0, stock_on_hand_milli)      # negative stock would invert the average
incoming_units  = line.quantity_milli
existing_value  = product.avg_cost_minor * existing_units
incoming_value  = line.total_cost_minor * 1000     # the supplier's EXACT line total, scaled
new_avg         = half_up((existing_value + incoming_value) / (existing_units + incoming_units))
```

```
on hand   10 @ 100 = 1,000
purchase  10 @ 120 = 1,200
          20       = 2,200  ──►  average 110
```

Weighted average rather than FIFO: bar stock is fungible and prices move slowly, so FIFO would
demand lot tracking on every bottle for precision nobody would notice. Returns and adjustments
are valued at the current average, not original cost.

`avg_cost_minor` is a **derived cache** — the authority is the purchase history, and an
invariant test recomputes it from the ledger.

### Line cost consistency

Each purchase line stores both `unit_cost_minor` (a rounded per-base-unit rate) and
`line_cost_minor` (the exact amount on the supplier invoice). The exact line total is
authoritative; the unit cost must agree with it within one minor unit:

```
derived_unit_cost = half_up(total_cost * 1000 / quantity_milli)
require |unit_cost - derived_unit_cost| <= 1
```

## 8.3 Reporting

- **Purchase cost is captured always; only its display is optional.** Cost sits on the
  supplier invoice being typed in anyway, so capturing it is free — and a club that enables
  profit reporting after six months gets a report that works **retroactively** rather than
  finding six months of blanks.
- **Stock is valued at cost, never at selling price.** Valuing at retail overstates the asset.

### Yield variance — the payoff for tracking shots at all

For every active product where `base_units_per_pack > 1000` (i.e. sold in fractions of a
pack):

```
purchased = Σ PURCHASE movements
sold      = −Σ SALE movements
returned  = Σ RETURN movements
adjusted  = Σ ADJUSTMENT movements          (negative when stock came up short)

packs_bought     = purchased / base_units_per_pack        (skip if 0)
net_sold         = sold − returned
expected_per_pack = base_units_per_pack
actual_per_pack   = (net_sold − adjusted) / packs_bought
```

"Should have given 24, gave 21." This is where over-pouring and theft become visible, and it
is how the club discovers its true conversion factor.

---

# 9. The shift report

Fully designed in [06-shift-report-design.md](06-shift-report-design.md). It carries two jobs
nothing else does.

**It is the only fraud control.** There is no manager permission layer. What stands in its
place is that every exception is itemised here with receipt numbers, so the owner can match it
against the spike of returned paper. **Aggregate an exception into a total and the control is
gone** — "7 voids totalling 4,300" tells the owner nothing they can check.

**It is the cross-shift reconciliation.** Tabs stay open across nights and waiters carry cash
between them, so sales and the money settling them routinely fall in different shifts. The
report must make that legible rather than appear to lose money.

## 9.1 Three rules

1. **Expected cash from reconciliations received, never from tabs marked paid.**
2. **Exceptions itemised, never summarised.** Seven lines with receipt numbers, waiters,
   values, reasons and times — not one total.
3. **Every figure states its basis.** Sales are recognised at *issue*; payments at *receipt*.
   Those are different nights for any tab that stays open.

## 9.2 Sections and their queries

**Header** — shift number, business date, opened/closed timestamps and cashiers.

**1. Sales issued this shift** — recognised at issue; what drove inventory, whether or not
anyone has paid.
```sql
COUNT(DISTINCT o.id), SUM(ol.quantity), SUM(ol.line_total_minor)
FROM orders o JOIN order_lines ol ON ol.order_id = o.id
WHERE o.shift_id = ? AND o.status = 'ISSUED'
```
Voided and replaced orders are excluded from every total (they are not `ISSUED`) but should be
shown as visible deductions so the arithmetic is legible rather than silently netted.

**2. Tabs closed this shift** — recognised when the tab was settled.
```sql
COUNT(*), SUM(total_minor), SUM(liability_minor),
SUM(CASE WHEN is_comped THEN total_minor ELSE 0 END)
FROM tab_payments WHERE shift_id = ?
```
`net_revenue = billed − comped`. Payment method is known only at reconciliation, so a tab
closed tonight and reconciled tomorrow appears here as *unreconciled* tonight and as *cash* or
*mobile money* tomorrow. That is correct, and the split makes it legible instead of looking
like missing money.

**3. Waiter positions** — the heart of the accountability model, per waiter:

| Column | Source |
|---|---|
| brought forward | held balance from previous nights |
| closed tonight | `Σ tab_payments.liability WHERE waiter AND shift = this` |
| handed over | `Σ (cash + non_cash) FROM finalized reconciliations WHERE waiter AND shift = this` |
| written off | `Σ written_off` from the same |
| shortfall | `max(0, held_balance − Σ liabilities of still-unreconciled closed tabs)` |
| held balance | derived (§7.4); carries forward to the next night |
| open tabs | count and value — **a different thing from held cash; never add the two together** |

Waiters with nothing to report are skipped so the report stays readable.

**4. Cash reconciliation**
```
opening float
+ reconciliations received (CASH portion only)
− paid out, ITEMISED by category with id, cashier, time and note
= EXPECTED
  COUNTED
  VARIANCE
  denomination breakdown
```

**5. Open tabs carried forward** — code, reference, waiter, value, opened-at and **age**
("tonight" / "N night(s) old"). Age is shown because tabs never auto-close; without it,
abandoned tabs accumulate invisibly and the outstanding total slowly becomes meaningless.

**6. Exceptions — itemised, each block ending with a count** so the owner can check the number
of paper slips against the number of lines before reading any of them:

| Block | Content |
|---|---|
| **VOIDS** | BR number, tab reference, waiter, value, reason |
| **CORRECTIONS** | BR number, tab, `old total → new total`, reason. Show **returned to bar** and **write-off** separately — a correction where nothing came back is a very different event from one where the bottles were returned, and collapsing them hides exactly the pattern worth watching |
| **COMPS** | Tab code, reference, value, **waiter and cashier**, reason, time |
| **LIABILITY WRITE-OFFS** | Reconciliation id, amount, waiter, reason, time |
| **ISSUE PRINT FAILURES** | BR number marked HANDWRITTEN, tab, waiter, order id, reason, time |
| **CUSTOMER PRINT FAILURES** | CR number, tab, time |
| **REPRINTS / RETRIES** | Receipt number, `REPRINT #n` or `RETRY #n`, outcome, **type (ISSUE vs CUSTOMER)**, reason. Fiscal reprints are a different kind of event from internal ones |
| **WRITE-OFFS** | BR number, product, quantity, reason — drinks consumed but never billed |

**7. Inventory movement** — opening stock value at weighted average cost, + purchases,
− cost of sales, − damage/loss/write-offs, = closing value. Plus low-stock and negative-stock
lists. **At cost, never at selling price.**

**8. Footer** — settings changed this shift (`old → new`, who, when), then cashier and owner
signature lines. Settings changes belong here because rates are snapshotted per transaction: a
rate change mid-shift is invisible in the totals but visible here.

## 9.3 Report behaviour

- **Immutable once generated.** Stored as **both** structured data (`report_json`) and the
  exact rendered text, so reprinting months later reproduces exactly what was signed, even if
  later corrections occurred. `shift_reports` rejects UPDATE and DELETE.
- **The X-report is the same document run mid-shift** without closing, clearly marked
  provisional.
- Generating the report triggers the automatic backup.

---

# 10. Audit

Every significant action is recorded immutably: order creation, issue, correction, void, tab
open/close/transfer, reconciliation, inventory movement, price change, recipe change,
**settings change**, reprint, shift open/closing/close, purchase, stock count, backup, restore,
licence activation, stranded-print resolution, handwritten-chit authorisation.

Each entry: `sequence_no`, `staff_id`, `action`, `entity_type`, `entity_id`, `old_value`,
`new_value`, `shift_id`, `created_at`, `prev_hash`, `row_hash`.

**An audit entry must commit or roll back with the business change it describes** — always
inside the caller's transaction. An audit row for a sale that was rolled back is worse than no
audit row.

## 10.1 Tamper-evidence, not tamper-proofing

The database file sits on a machine the owner controls, and anyone with a SQLite browser can
rewrite it. **Be honest about this.** What is achievable:

- Triggers that `RAISE(ABORT)` on UPDATE/DELETE of append-only tables (stops the *application*
  from doing it, stops nobody outside it).
- A **hash chain**: each row hashes the previous, so any later edit breaks every hash after it.
- An integrity-check report that verifies the chain end to end and names the first broken row.

```
row_hash = SHA256( sequence_no ‖ staff_id ‖ action ‖ entity_type ‖ entity_id
                   ‖ old_value ‖ new_value ‖ created_at ‖ prev_hash )
genesis prev_hash = "0" * 64
```

**Port note — fix this:** the Java sets its field separator to the empty string, so `"ab"+"c"`
and `"a"+"bc"` hash alike and adjacent fields could be shuffled undetectably. Use a real
separator (e.g. ``) and length-prefix or explicitly encode nulls.

Verification walks rows in `sequence_no` order and fails on: a gap in the sequence, a
`prev_hash` that does not match the running chain, or a recomputed hash that does not match
the stored `row_hash`.

---

# 11. Enforcement: what the database must guarantee

These are not left to the service layer, because a future bug or a support session in a SQLite
browser would otherwise break them. Reproduce every one of these in the Tauri port — SQLite
triggers work identically from Rust.

## 11.1 Append-only tables

`orders` (delete), `order_lines`, `order_corrections`, `order_correction_lines`,
`stock_movements`, `purchases`, `purchase_lines`, applied `stock_counts` and their lines,
`tab_payments`, `reconciliations`, `reconciliation_tabs`, `cash_movements`, `receipts`,
`receipt_prints`, `audit_log`, `shift_reports`, `pending_order_corrections`.

Mutable (state/master, all changes audited): `staff`, `products`, `sale_items`, `recipes`,
`prices`, `settings`, `tabs`, `shifts`, `suppliers`, `sequences`.

## 11.2 Whitelisted state transitions

| Table | Permitted mutation |
|---|---|
| `orders.status` | The state machine in §6.2 only |
| `tabs.status` | Anything except `CLOSED\|RECONCILED → OPEN` |
| `receipts.status` | `PENDING → PRINTED\|FAILED\|VOID`, `FAILED → PRINTED`, or unchanged |
| `receipts.rendered_text` | `NULL → non-blank` while `PENDING`, once |
| `receipts.printed_at` | `NULL → timestamp` only when status becomes `PRINTED` |
| `receipt_prints.outcome` | `UNKNOWN → SUCCESS\|FAILED`, exactly once |
| `reconciliations.finalized_at` | `NULL → timestamp`, once; everything frozen thereafter |
| `stock_counts.status` | `DRAFT → APPLIED`, then frozen |

Everything else on those rows — receipt identity, money values, rates, actors, attempt shift —
is immutable.

## 11.3 Structural constraints

- **At most one `OPEN` shift** (partial unique index).
- **`shifts.business_date` unique.**
- **An order may be replaced at most once** (partial unique index on `replaces_order_id`) —
  stops chains forking and revenue double-counting.
- **A receipt is either an order's slip or a tab's fiscal receipt**, never both (CHECK).
- **`UNIQUE (receipt_type, sequence_no)`** and unique `receipt_number`.
- **Comped tabs carry zero liability** (CHECK) and require a non-blank reason (trigger); a
  non-comped tab may not carry a comp reason.
- **A tab may appear in only one reconciliation**, and the allocation must equal that tab's
  exact immutable liability for that waiter.
- **Nothing may be appended to a finalized reconciliation.**
- **Payouts must be negative and categorised.**
- **Correction lines:** `returned + written_off = the stock reduction`.
- **Purchase movements must match their immutable invoice line** and may post at most once per
  product; no line may be appended after a purchase has posted stock.
- **Count adjustments must match a count line whose variance is exactly `counted − system`**
  and may post at most once.
- **New operational SALE/RETURN/PURCHASE/ADJUSTMENT movements must carry the correct source
  id**; only explicit manual `DAMAGE`/`LOSS` entries may be source-less.
- **Normalized supplier identity** prevents an old invoice being received through a duplicate
  supplier row.
- **A legacy row with non-zero charges but unknown rates cannot produce a fiscal receipt** —
  it stops for manual review rather than printing a false 0% document.

## 11.4 The invariant tests — write these first

For a ledger system these are worth far more than UI tests. Each should be a property test
over generated trading histories.

| # | Invariant |
|---|---|
| INV-1 | Stock on hand equals `SUM(stock_movements.quantity_milli)` per product. No stored quantity exists to disagree. |
| INV-2 | Each `PURCHASE` movement matches an immutable purchase line; `avg_cost` recomputes from those exact line totals with checked half-up arithmetic. |
| INV-3 | Tab total equals `Σ order_lines.line_total` over orders in final state — `REPLACED`, `VOIDED`, `ABANDONED`, `DRAFT`, `PRINTING` excluded. |
| INV-4 | No gaps in `receipts.sequence_no` per type. Abandoned `BR-` numbers exist with `status = 'VOID'`. |
| INV-5 | Every correction chain has exactly one non-`REPLACED` leaf; no order is replaced twice. |
| INV-6 | Waiter held balance equals liabilities minus reconciliations; never negative outside a single transaction. |
| INV-7 | Expected cash equals `SUM(cash_movements.amount_minor)`, and **no `tab_payments` row contributes to it**. |
| INV-8 | The audit hash chain verifies end to end. |
| INV-9 | At most one shift with `status = 'OPEN'`. |
| INV-10 | Shift report totals equal the sum of their underlying transactions. |
| INV-11 | Tax arithmetic matches agreed worked examples in **both** inclusive and exclusive modes. |
| INV-12 | Every `ISSUED` order has a receipt row: `PRINTED`, or `FAILED` with a durable handwritten authorisation. `UNKNOWN` is non-terminal and blocks trading/close until resolved. |

Plus a full **trading-night** integration test: open shift → open tabs → issue rounds →
correct → void → close tabs → reconcile → pay out → close shift → assert every figure on the
report against the ledgers.

---

# 12. Settings

**Nothing about the business is compiled in.** The prior prototype hardcoded the service
charge, the low-stock threshold, the business day start and the receipt header, so each was a
code change to adjust.

Storage: typed key/value rows (`key`, `value`, `value_type`, `updated_at`, `updated_by`), so
adding a setting is a data change. Types: `STRING`, `INTEGER`, `BOOLEAN`, `RATE` (basis
points), `TIME` (`HH:mm`).

**Every settings write is audited with old and new value, and never restates historical
transactions.**

| Key | Type | Default | Notes |
|---|---|---|---|
| `tax.enabled` | BOOLEAN | `false` | Club is not currently charging VAT |
| `tax.rate_bp` | RATE | `1500` | 15% |
| `tax.inclusive` | BOOLEAN | `true` | ⚠ A fact about the business. Set wrong, every total is off by the rate and looks correct |
| `service_charge.enabled` | BOOLEAN | `true` | |
| `service_charge.rate_bp` | RATE | `1000` | 10%, observed. Taxable |
| `shift.day_start` | TIME | `18:00` | Decides business-date assignment |
| `shift.day_end` | TIME | `06:00` | Advisory; drives the overdue warning |
| `inventory.allow_negative` | BOOLEAN | `false` | Enforced at issue, before number allocation |
| `tabs.reference_mode` | STRING | `TABLE` | `TABLE\|CUSTOMER_NAME\|CUSTOMER_PHONE\|CUSTOM` |
| `tabs.age_warning_days` | INTEGER | `3` | **Currently inert — see §14** |
| `payments.comps_enabled` | BOOLEAN | `false` | |
| `payments.partial_enabled` | BOOLEAN | `false` | **Currently inert — see §14** |
| `reporting.show_cost` | BOOLEAN | `false` | Cost always captured; only display is optional. **Currently inert** |
| `receipt.business_name` | STRING | `Tonic Family Club` | |
| `receipt.address` | STRING | `Mekelle, Kebelle 16` | |
| `receipt.phone` | STRING | `` | |
| `receipt.tin` | STRING | `` | |
| `receipt.footer` | STRING | `` | |
| `receipt.chars_per_line` | INTEGER | `48` | 80 mm paper; 58 mm is 32 |
| `locale.currency_code` | STRING | `ETB` | |
| `locale.rounding` | STRING | `NONE` | **Currently inert** |

## 12.1 Commissioning checklist — a required deliverable

Tax mode **and especially whether prices are tax-inclusive**; tax rate; service charge rate;
business day start and end; receipt header and TIN; per-product low-stock thresholds; shot
conversion factors; opening stock; real staff records; printer device; backup destination;
licence activation.

**Two things are logged loudly at every startup on a fresh database and must be replaced
before going live:**

1. **The placeholder catalogue.** 11 products and 18 sale items are seeded because the club
   had no exportable data. **Every price is invented.** So is the 24-shots-per-bottle
   conversion.
2. **The bootstrap cashier `C01` / PIN `1234`** — deliberately obvious so changing it is a
   visible commissioning step rather than a forgotten one.

The seeder must run **only on an empty database**, so it can never overwrite real data.

---

# 13. Platform requirements (and what changes for Tauri)

| Concern | The requirement | Java did | Tauri/Rust should |
|---|---|---|---|
| **Money** | Integer minor units, checked arithmetic | `long` + `Math.*Exact` | `i64` newtype + `checked_*` |
| **Database** | SQLite, **WAL mode**, `foreign_keys = ON` | JDBC | `rusqlite` or `sqlx` — set both pragmas explicitly on every connection |
| **Migrations** | Versioned, listed **explicitly** (never classpath/dir scanning — it behaves differently in a packaged build), applied on startup with a pre-migration backup. v1.1 must upgrade a live database holding a year of history with no DBA present. **Add migrations, never edit them.** | 14 SQL files in an ordered list | `include_str!` each file into an ordered `const` array |
| **Single instance** | Exclusive lock on a lock file. The OS must release it on process death **including a hard kill**, avoiding a stale lock after a power cut | `FileChannel.tryLock()` | `fs2::FileExt::try_lock_exclusive`, or `tauri-plugin-single-instance` |
| **Time** | Store UTC epoch ms + business date + shift id. Detect a non-monotonic clock against the last recorded timestamp and log it | | |
| **Sequences** | Allocated inside the consuming transaction | `UPDATE … RETURNING` | Same statement; SQLite ≥ 3.35 supports `RETURNING` |
| **Printing** | Raw **ESC/POS bytes** — cut, drawer kick, failure detection. Off the UI thread. Behind an interface, with a file-writing fallback for development | `EscPosPrinter` / `FilePrinter` | A `Printer` trait; `serialport` or raw device write on Windows. **This is why it is a desktop app: a browser cannot write raw bytes.** |
| **UI** | Keyboard-first, minimal mouse | JavaFX | Any web UI in the Tauri window — but see below |

## 13.1 Why desktop, not a browser app

Three independent reasons, all still true:

1. A browser cannot write raw ESC/POS bytes — no cut, no drawer kick, no failure detection,
   which the entire printing state machine depends on.
2. Browser storage offers no transactions, foreign keys, triggers or hash chain.
3. A browser app on the customer's own machine cannot be fingerprinted for licensing.

Tauri satisfies all three: the Rust side owns SQLite and the printer, and the webview is only
the UI. **Keep every business rule in Rust, behind commands. Never let the webview compute a
total, a balance or a stock level.**

Also: `F5` is Reload and `F3` is Find in a browser. In Tauri you must intercept these before
the webview does, or disable the default shortcuts entirely.

## 13.2 Keyboard-first UI

Shortcuts as shipped: `F2` new tab, `F3` find a tab, `F5` issue & print, `F8` close tab,
`Enter` confirm, `Esc` cancel.

**Fast entry syntax** — the parser is pure logic and belongs in Rust with its own tests, because
a silent misparse bills the wrong drink:

```
"3*12"   "3x12"   "3 12"    → 3 of token "12"
"12"                        → 1 of token "12"
"2*BEER-01-B"               → 2 of that sale item code
blank or malformed          → no result (do not throw; the field validates per keystroke)
max quantity 999            (guards against a stuck key becoming a thousand beers)
```

The token is a sale item code **or** a menu line number. Input is trimmed and upper-cased.

## 13.3 Screen structure

Five views, worth reproducing closely because staff and customers already recognise the
format: **Overview · Sale · Warehouse · Waiters · Reports**, plus a login screen.

Modal business actions must **disable window-close paths while committing**, so a second click
cannot duplicate a write.

**A non-dismissible startup gate** must resolve, before any trading: stranded `PRINTING`
orders, recovered drafts, unresolved issue-slip reprint attempts, and unresolved customer
receipt attempts.

## 13.4 Not yet built (in any version)

- **Backup and restore.** Never filesystem-copy a WAL database while open — use the SQLite
  online backup API or `VACUUM INTO`. A backup on the same disk is not a backup: an external
  target is required, with a retention policy. **Verify after writing** (`PRAGMA
  integrity_check` plus row counts); an unverified backup is a guess. Backups carry customer
  names and phone numbers, so consider encryption. **Restore rewinds receipt sequences**,
  reissuing numbers already in customers' hands — so it must archive the current database
  first, be heavily logged, write a restore marker, and require a credential the cashier does
  not hold. Offline password recovery is impossible; a vendor override code derived from the
  licence key is the recommended answer.
- **Licensing.** Signed licence file verified offline (Ed25519, private key never ships);
  machine fingerprint matched **n-of-m** so one component change does not invalidate it;
  monotonic last-seen timestamp to detect clock rollback. **On expiry: nag, never hard-block,
  and never during an open shift.** A determined pirate wins on an offline box regardless; the
  goal is that honest customers cannot accidentally fall out of compliance.
- **Windows packaging.** For Tauri this is `tauri build` (MSI/NSIS) — much simpler than the
  Java `jpackage`-on-a-Windows-runner route. Unsigned installers still trigger SmartScreen; a
  certificate is roughly USD 200–400/year (undecided).
- **Sales-by-product reporting.**

---

# 14. Known gaps and inert settings

| | |
|---|---|
| **Bartenders are not modelled** | Bar shrinkage cannot be attributed to any individual. Recording on-duty bartenders per shift would close it; deferred, not rejected. The `staff.role` column exists so reintroducing `BARTENDER` is a data change, not a schema change. |
| **`tabs.age_warning_days`** | Read by nothing. The 3-day tab warning never fires. Age is displayed but never compared to a threshold. |
| **`payments.partial_enabled`** | Read by nothing. |
| **`reporting.show_cost`** | Read by nothing — cost is always captured, but the display toggle is not wired. |
| **`locale.rounding`** | Read by nothing; implies no missing behaviour (rounding is fixed half-up at the total). |
| **Audit hash field separator** | Empty string; see §10.1. Fix in the port. |

The owner decided to leave the inert keys in place rather than delete them or build the
enforcement — the intent is worth keeping visible. **But an inert safety switch reads as
working configuration.** If someone asks why an old tab shows no warning, the answer is that
the enforcement was never written; building it is real feature work, not cleanup.

## 14.1 Deliberately out of scope for v1

Bartenders serving customers directly · food and kitchen (provisioned via `tracks_inventory`,
`destination` and receipt splitting) · tips · change floats · comps (off by default) · split
tender · crate purchasing (provisioned via `units_per_purchase_pack`) · fiscal devices ·
multi-terminal · networking · tab merging · discounts and happy hour · staff drinks.

## 14.2 Open questions for the client

Printer make/model/interface · whether a waiter shortfall may be written off and by whom · the
age at which an open tab is escalated or written off · two open tabs sharing one reference ·
tab merging · discounts, happy hour, staff drinks · partial-bottle counting method · licence
model, expiry behaviour, reactivation after a Windows reinstall · backup destination,
retention, encryption · restore credential recovery · code signing certificate · cash payout
categories · who maintains the system after handover.

---

# 15. The five things most likely to be "fixed" by mistake

Read this before changing any behaviour that looks wrong.

1. **A correction never reverses the original sale.** The bottles left the shelf. It posts a
   return for what came back, and stock not returned gets *no movement at all* — the sale
   already removed it. Posting a loss too would remove the same drinks twice.

2. **Expected cash comes from reconciliations, never from tabs closed.** The customer pays the
   waiter, not the till. There must deliberately be no path from `tab_payments` into
   `cash_movements`.

3. **`PRINTING` commits before anything reaches the printer.** Two transactions on purpose, so
   a power cut leaves the order visibly stranded rather than silently rolled back while a slip
   sits in the bartender's hand.

4. **Tax contained in an inclusive price is derived by subtraction**, never recomputed from the
   net. Two roundings do not reconstruct the whole.

5. **Held balances and stock on hand are summed from ledgers, never stored.**

And the sixth, which is a design decision rather than a mechanism: **there is no manager
permission layer.** This is an accepted risk. The shift report is the compensating control,
which is why its exceptions must stay itemised with receipt numbers rather than aggregated
into totals.

---

# Appendix A — The bugs the tests caught

Recorded because each was invisible on inspection, and because they show which tests were
worth writing. Expect to hit all five again in a fresh implementation.

| # | Bug | How it was caught |
|---|---|---|
| 1 | Replacement orders posted their own sale on top of the correction difference — correcting 5 beers to 3 removed 6 from stock | Asserting **stock levels** after a correction |
| 2 | A failed replacement print left an orphan in `PRINTING`; recovery would have billed the round twice | Testing the **print-failure path of a correction** |
| 3 | Tax-inclusive pricing drifted a santim — 1000.00 billed as 1000.01 | Asserting **the customer pays the menu price**, not internal self-consistency |
| 4 | `stock_movements.shift_id` was NOT NULL, so a delivery could not be recorded while the club was shut, nor a stock count at all | Testing **purchasing with no shift open** |
| 5 | Reading a null-check after a later column made every superseded recipe look current | Testing **recipe versioning** |

Bug 3 is the instructive one: the self-consistency test (`net + service + tax == total`) passed
throughout. **Only asserting the property the business actually cares about found it.**

---

# Appendix B — Entity summary

```
settings(key, value, value_type, updated_at, updated_by)
sequences(name, next_value)
staff(id, code, full_name, role[CASHIER|WAITER], active, pin_hash, pin_salt, created_at)

products(id, code, name, category, base_unit, base_units_per_pack,
         units_per_purchase_pack, low_stock_threshold_milli, tracks_inventory,
         destination, avg_cost_minor, active, created_at)
sale_items(id, code, name, category, active, created_at)
recipes(id, sale_item_id, version, effective_from, effective_to, created_by)
recipe_lines(id, recipe_id, product_id, quantity_milli)
prices(id, sale_item_id, price_minor, effective_from, effective_to, created_by)

shifts(id, shift_number, business_date UNIQUE, status, opened_at, opened_by,
       closed_at, closed_by, opening_float_minor, counted_cash_minor)
       + partial unique index: only one OPEN

tabs(id, tab_code, waiter_id NOT NULL, reference_mode, table_no, customer_name,
     customer_phone, custom_ref, display_label, status, opened_shift_id, opened_at,
     closed_shift_id, closed_at, closed_by, is_comped)
tab_transfers(id, tab_id, from_waiter_id, to_waiter_id, reason, created_at, created_by)

orders(id, tab_id, status, waiter_id, cashier_id, shift_id, replaces_order_id,
       root_order_id, void_reason, voided_at, voided_by, created_at, issued_at)
       + partial unique index on replaces_order_id
order_lines(id, order_id, sale_item_id, sale_item_name, recipe_id, quantity,
            unit_price_minor, line_total_minor)
order_corrections(id, original_order_id, replacement_order_id, correction_type[CORRECTION|
                  VOID], issue_receipt_number, reason, shift_id, created_at, created_by)
order_correction_lines(id, correction_id, product_id, delta_milli, returned_milli,
                       written_off_milli)
pending_order_corrections(replacement_order_id, original_order_id, issue_receipt_number,
                          reason, shift_id, created_at, created_by)
pending_order_correction_lines(replacement_order_id, product_id, delta_milli,
                               returned_milli, written_off_milli, return_note)

receipts(id, receipt_type[ISSUE|CUSTOMER], receipt_number UNIQUE, sequence_no, order_id,
         tab_id, destination, status[PENDING|PRINTED|FAILED|VOID], waiter_name,
         cashier_name, subtotal_minor, service_minor, tax_minor, total_minor,
         tax_rate_bp, service_rate_bp, tax_inclusive, rendered_text, shift_id,
         created_at, printed_at)   UNIQUE(receipt_type, sequence_no)
receipt_prints(id, receipt_id, print_no, reason, outcome[SUCCESS|FAILED|UNKNOWN],
               shift_id, created_at, created_by)   UNIQUE(receipt_id, print_no)

stock_movements(id, product_id, movement_type, quantity_milli, unit_cost_minor,
                order_id, purchase_id, stock_count_id, reason, shift_id NULLABLE,
                created_at, created_by)

suppliers(id, name, phone, active, created_at)
purchases(id, supplier_id, invoice_ref, received_at, shift_id NULLABLE,
          total_cost_minor, created_by)
purchase_lines(id, purchase_id, product_id, quantity_milli, unit_cost_minor,
               line_cost_minor)
stock_counts(id, counted_at, status[DRAFT|APPLIED], created_by)
stock_count_lines(id, stock_count_id, product_id, system_qty_milli,
                  counted_qty_milli, variance_milli)

tab_payments(id, tab_id, waiter_id, subtotal_minor, service_minor, tax_minor,
             total_minor, is_comped, comp_reason, liability_minor, tax_rate_bp,
             service_rate_bp, tax_inclusive, charge_rates_known, shift_id,
             created_at, created_by)        ← NO payment_method column, ever
reconciliations(id, waiter_id, cashier_id, expected_minor, cash_minor, non_cash_minor,
                shortfall_minor, written_off_minor, write_off_reason, shift_id,
                created_at, finalized_at)   ← NO overage column, ever
reconciliation_tabs(reconciliation_id, tab_id, amount_minor)   PK(recon, tab)
cash_movements(id, shift_id, movement_type, amount_minor, category, reference_id,
               note, created_at, created_by)

audit_log(id, sequence_no UNIQUE, staff_id, action, entity_type, entity_id, old_value,
          new_value, shift_id, created_at, prev_hash, row_hash)
shift_reports(id, shift_id UNIQUE, report_json, rendered_text, generated_at, generated_by)
```
