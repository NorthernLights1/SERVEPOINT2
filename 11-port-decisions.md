# ServePoint — Port Decisions

**Purpose:** the decisions taken for the Tauri/Rust rebuild, with the reasoning behind each.
Companion to [10-business-logic-for-port.md](10-business-logic-for-port.md), which remains the
authority on *behaviour*. This document records what changes, what is deferred, and what is
deliberately not inherited.

**Status:** agreed. Where a decision supersedes something in the business-logic document, it
says so explicitly.

---

# 0. What is being inherited, and what is not

**The business logic is inherited. The implementation is not.**

The domain knowledge in `10-business-logic-for-port.md` — cash accountability, corrections
versus returns, expected-cash-from-reconciliations, the print protocol — is hard-won and
largely independent of whether the Java was any good. It is inherited in full.

The shipped Java implementation is not. It was rejected for three stated reasons: the interface
was ugly, inconsistencies arose that could not be resolved from the UI, and it allowed sales
into negative stock rather than blocking them. Each is addressed below (D9, D10, D14).

**The sibling design documents (`04-decisions.md`, `06-shift-report-design.md`,
`07-specification-v2.md`, `08-data-model.md`) are deliberately not consulted.** They exist in
the previous project tree and were left out of this one on purpose. The business-logic document
is explicitly written to be sufficient without them — Appendix B gives every table and column,
§11 gives every constraint, trigger and permitted state transition.

That is not merely convenience. Appendix A bug #4 was a *schema* defect:
`stock_movements.shift_id` was `NOT NULL`, so a delivery could not be recorded while the club
was shut and a stock count could not be recorded at all. Working from the old data model risks
inheriting that class of mistake silently, because the constraint looks entirely reasonable
until someone tries to receive stock on a Tuesday afternoon. Indexes will be derived from the
queries actually written, not from guesses about queries not yet written.

---

# 1. Stack

## D1 — Tauri + Rust + SQLite, React/TypeScript frontend

Confirmed. §13.1's three reasons for desktop over browser all hold: a browser cannot write raw
ESC/POS bytes, browser storage has no triggers or foreign keys, and a browser app cannot be
fingerprinted for licensing.

Note that those reasons argue for **Rust**, not specifically for Tauri — the webview is
incidental to all three. Rust earns its place here on different grounds: `Money`/`Milli`/
`BasisPoints` newtypes make §1's numeric conventions unmixable at compile time, status enums
with exhaustive `match` turn §6.2's state machine into a compile error rather than a runtime
one, and `proptest` over generated trading histories is exactly the shape INV-1…INV-13 require.

Frontend is React + TypeScript + Vite — Tauri's default, and the widest maintenance pool for
the handover question left open in §14.2.

## D2 — Ship the fixed-version WebView2 runtime

**Non-negotiable.** Tauri renders through WebView2 on Windows, and the default installer ships
the *evergreen bootstrapper*, which downloads the runtime at install time. This system is
specified as offline. On an older or freshly-imaged Windows machine with no network, that
install simply fails — and it fails during commissioning, at the customer's site.

The fixed-version bundle (~180 MB) removes the network dependency and additionally stops Edge
updating the rendering engine underneath a POS that must work every night. The cost is
installer size and ownership of runtime updates. Worth it.

## D3 — The webview computes nothing

§13.1's rule, restated as an enforceable convention: Rust commands return **pre-formatted
display strings** alongside raw values (`{ total_minor, total_display }`), and the UI renders
only the formatted string. The frontend is never handed numbers it could re-sum. This is the
one real tax Tauri imposes over a native GUI, and it is paid by discipline at the command
boundary rather than by hoping nobody reaches for a running total in JavaScript.

## D4 — Windows artifacts are not built on the development machine

Development is on Linux; the target is Windows. Cross-compiling Tauri from Linux to Windows is
impractical. MSI/NSIS packaging happens on a Windows machine or a Windows CI runner. This is a
release-pipeline decision, not a code one, but it needs to exist before the first customer
install.

---

# 2. Scope

## D5 — Three pages, and the ledger beneath them

The requested deliverables are a reconciliation page, a reports page and a settings page. Those
are build phases 4, 5 and 2. They are not independent: reconciliation sits on shifts, tabs,
`tab_payments`, `reconciliations` and `cash_movements`; reports sit on orders, order lines,
sale items and stock movements. Phases 0, 1 and 3 are what make the three pages mean anything.

## D6 — Build order

| Phase | Contents |
|---|---|
| **0** | Tauri scaffold; SQLite with WAL and `foreign_keys` set explicitly per connection; migrations as an explicit ordered `const` array via `include_str!` (never directory scanning — it behaves differently in a packaged build); money/quantity/rate newtypes; half-up arithmetic; business calendar; sequences allocated inside the consuming transaction; audit hash chain |
| **1** | Full schema and triggers; INV-1…INV-13 as property tests; verified backup (D17) |
| **2** | **Settings page**; bill calculator (§7.2, all four tax/service combinations); first-run wizard |
| **3** | Trading core (staff, catalogue, tabs, orders, issue protocol, corrections, voids, tab close); CSV catalogue import; seeder |
| **4** | **End-of-day reconciliation page** |
| **5** | **Reports page** |

Invariant tests are written alongside phases 0 and 1, not after. For a ledger system they are
worth more than UI tests.

## D7 — No cost or margin reporting in v1

Revenue and quantity only. Cost is still captured throughout the schema per §8.3, so profit
reporting can be switched on later and will work **retroactively** rather than finding a year
of blanks. `reporting.show_cost` stays unwired (see D15).

Consequence: the purchasing module (§8.1–8.2) is not required for the three pages and is not in
the phase plan. It will be needed before margin reporting or the yield variance report of §8.3
can mean anything.

## D8 — One venue per install

Productised, not multi-tenant. Each club installs its own copy with its own database. Neutral
defaults replace the Tonic-specific ones, a first-run wizard covers §12.1's commissioning
checklist, and currency and receipt header are configurable. This matches the single-instance
and per-machine licensing premise in §13.1; a venue switcher would require venue scoping on
every table and every query, and would contradict that design.

---

# 3. Changes to specified behaviour

## D9 — Insufficient stock always blocks the sale. No setting, no override.

**Supersedes §3.6 and removes the `inventory.allow_negative` row from §12 entirely.**

> *Revised. An earlier version of this decision made the policy configurable with four values
> (`BLOCK` / `OVERRIDE` / `FRACTIONAL` / `ALLOW`). That is withdrawn — there is no setting.*

The old build allowing sales into negative stock is one of the three stated reasons for the
rebuild. §3.6 already records the correction; this goes further and removes the choice. A round
the shelf cannot cover is refused. There is no policy key, no override, no typed-reason bypass,
and no per-venue variation.

The reason for removing the configurability rather than merely defaulting it: an override that
exists will be used, and used routinely it recreates precisely the condition being escaped —
stock figures that no longer mean anything. A setting also invites a support call whose answer
is "turn the safety off".

**Unchanged from §3.6:** the check runs in pre-checks, on the **aggregate** across the whole
expanded order, **before** the `BR-` sequence is touched. A refusal burns no receipt number and
leaves no half-order to recover. The rejection names each product and its shortfall.

### The relief valve is a stock correction, not a sale override

§2.2 states the shot conversion factor is an assumption, not a fact — theoretical 25 shots per
bottle against a real 23–24. Set slightly high, the till refuses gin *while there is gin in the
bottle*, mid-service, and §3.5 forbids a stock count while a shift is open.

So the escape hatch is a **single-product stock correction**: one product, a typed reason,
audited, posted as an `ADJUSTMENT` movement. It is explicitly **not** a stock count — §3.5's
objection is that a full count taken while sales continue measures nothing, which does not apply
to correcting one known-wrong figure. It fixes the cause (a wrong number) rather than bypassing
the effect, and cannot be used to push a sale through quietly, because it moves stock rather
than waiving the check.

**Reporting.** A **ninth exception block, `STOCK CORRECTIONS`**, joins the eight in §9.2 —
product, quantity before and after, reason, cashier, time. Itemised, never summarised, per
§9.1 rule 2. A correction made minutes before a large round is exactly the pattern an owner
should be able to see.

## D10 — Every reachable state has a resolution path, and it is a test

**Addresses the second stated reason for the rebuild: inconsistencies that could not be fixed
from the interface.**

This design has many states that require a human to resolve and cannot resolve themselves — an
order stranded in `PRINTING`, a draft recovered after a confirmed non-print, a `receipt_prints`
row at `UNKNOWN`, a `pending_order_correction` whose slip never emerged, a shift stuck in
`CLOSING` after a failed report render, a stock count left at `DRAFT`.

§4.4 makes an unresolved one **fatal**: print recovery must be complete before `OPEN → CLOSING`.
A single `UNKNOWN` receipt print with no screen behind it means the shift can never close. Not
a bug to work around — a dead end.

The structural reason it cannot be fixed from the UI is that everything is append-only. There
is no editing your way out; only compensating — correction, void, adjustment, write-off,
payout, settle-outstanding. **Any reachable bad state with no compensating action wired to a
button is permanent.**

Two commitments:

1. **A resolution screen for every stuck state.** The non-dismissible startup gate of §13.3,
   plus an always-reachable "unresolved items" panel — not only at launch, because these arise
   mid-service.
2. **`INV-13`, added to the invariant list of §11.4:** for every reachable non-terminal state, a
   registered resolution command exists. A new state with no way out fails the test suite,
   rather than failing at three in the morning.

## D11 — Past shifts are read from the stored report, never recomputed

**New rule, arising from the reports page.**

The reports page and the §9 shift report overlap and must never disagree. The shift report is
immutable, signed, and stored as both `report_json` and rendered text precisely so a reprint
months later reproduces what was signed. If the reports page independently re-sums a past
shift's orders and gets a different figure — because of a later correction, a changed setting,
anything — there are two official truths and no way to know which one the owner signed.

- Past shifts: read from `shift_reports.report_json`. Never recomputed.
- Cross-period aggregates (week, month): sum the stored reports, not the raw ledger.
- Live computation only for the currently open shift, clearly marked provisional — the X-report
  of §9.3.

## D12 — Reporting basis: issued, with cash shown alongside

Headline revenue is orders reaching `ISSUED` within the window, matching §9.2 section 1 and
matching inventory movement. Cash actually received is shown as a **separate, labelled** figure.

This follows §9.1 rule 3 — every figure states its basis. Sales are recognised at issue and
payments at receipt, and for any tab open across nights those are different dates. Showing one
number without its basis is how a report appears to lose money.

## D13 — Drink categorisation from `sale_items.category`

"Shots vs bottles vs cocktails" is a **sale item** property, not a product one. One product
`Gin` carries the sale items "Gin, bottle", "Gin, shot" and "Gin & Tonic"; bucketing by
`products.base_unit` would dissolve every cocktail into its component spirits.

Inferring from recipe shape (one line = simple, two or more = cocktail) was rejected: a Gin &
Tonic whose tonic is marked `tracks_inventory = false` expands to a single line and would be
reported as a plain shot.

Cost: the categories must be set correctly at commissioning. Added to the §12.1 checklist and
covered by the CSV import of D18.

"Most popular" is reported by **quantity and by revenue, both, sortable** — they rank
differently, and the difference is the interesting part.

## D14 — All periods group on business date

§1.3's `business_date`, never the calendar date. A sale at 02:00 Saturday belongs to Friday's
night, Friday's week and Friday's month. Weeks start Monday.

## D15 — Inert settings stay out of the UI

`tabs.age_warning_days`, `payments.partial_enabled`, `reporting.show_cost` and
`locale.rounding` are read by nothing (§14). The rows remain in the database — the intent is
worth keeping visible — but they get no toggle until the enforcement behind them exists.

§14's own warning is the reason: *"an inert safety switch reads as working configuration."* A
switch that does nothing is worse than an absent one, because someone will rely on it.

## D16 — Audit hash chain uses a real field separator

§10.1 flags the Java's empty separator as a genuine defect: with no delimiter, `"ab" + "c"` and
`"a" + "bc"` hash identically, so adjacent fields could be transposed undetectably. The port
uses an explicit separator with length-prefixed fields and explicitly encoded nulls.

Also carried forward from §10.1: this is **tamper-evident, not tamper-proof**, and should be
described that way to the customer. The database sits on a machine the owner controls.

## D22 — The owner gets a login, and owns the settings

**Supersedes §0.3's "the cashier is the only system user" and the owner's "Logs in? No".**

`staff.role` gains `OWNER`. Two people authenticate now: the cashier, who operates the till, and
the owner, who reads and configures.

| | Cashier | Owner |
|---|---|---|
| Operate the till, tabs, orders, end of day | yes | no |
| Read Overview, Reports, past shift reports | yes | yes |
| Change settings | **no** | **yes** |

**This is not the permission layer §0.3 rejects.** That objection was to *approval gates* — a
manager PIN to authorise a void, which is theatre when one person satisfies it. This is a read
and configuration separation: it gates no operational action, and nothing about voids,
corrections or write-offs now requires a second person. The compensating control remains the
shift report with every exception itemised.

What it does change is **what the report is for**. §9 made it the owner's document precisely
because the owner did not touch the computer. They do now, so it is read on screen (D23).

**Two consequences that need solving, not discovering:**

1. **A fresh install has no owner, but commissioning *is* settings** — tax mode, rates, receipt
   identity. First-run must therefore create the owner account before anything else, and the
   §12.1 bootstrap account becomes an owner rather than a cashier.
2. **A forgotten owner PIN locks settings permanently** on an offline machine with no recovery
   path. The vendor override code derived from the licence key, already proposed in §13.4 for
   restore, is the natural answer — it is the same problem.

## D23 — Report printing off by default; the report is a screen, not a slip

**Supersedes §9.3's assumption that the shift report is a printed document.**

The report is **always generated and stored** at close — that stays inside the close transaction
(§4.3), so a night can never be committed without it. What changes is that nothing is printed.

- Rendered as a **proper screen layout**, not thermal-slip text at 48 characters.
- `printing.report_enabled` defaults **off**. A venue that wants paper can turn it on.
- **Printing happens after the close commits and can never block it.** A dead printer must not
  strand a night — that is the class of failure this rebuild exists to escape.
- Bar issue slips still always print. That is not reporting; it is how drinks get released.

The signature lines of §9.2's footer only mean something on paper. A venue that wants a signed
record turns printing on; otherwise the owner's own login is the accountability path.

## D24 — Receipt footer carries bank accounts and an EthSwitch QR

New optional settings, printed at the foot of the customer receipt when configured and omitted
entirely when not:

- **Bank accounts** — a list of name/number pairs for transfer.
- **EthSwitch QR** — encodes the amount owed.

**The QR is informational, not a payment record.** It does not know whether the bill has been
paid, and nothing reconciles against it. Deliberately so — tracking payment state through it
would mean a second, unreconciled source of truth about what is owed, which is exactly what the
waiter-liability model exists to avoid.

**This tightens the printer specification.** A QR needs the ESC/POS `GS ( k` 2D barcode command,
or a raster-bitmap fallback. Printer make, model and interface remain open (§14.2); this now
belongs on that spike alongside the print-failure detection the whole issue protocol depends on
(D20).

## D25 — VAT is always itemised; customer TIN is optional

- **VAT is broken out on the customer receipt even when menu prices include it.** §7.2 already
  computes it by subtraction; this makes showing it mandatory, because a business customer needs
  the figure to reclaim the tax.
- **The tax line is suppressed when the tax amount is zero** — whether because tax is disabled or
  the rate is 0%. One rule covers both.
- **`tabs.customer_tin`**, optional, captured at the cashier's discretion and snapshotted onto
  the receipt like every other value. Prompting for it is itself a setting, and it is hidden when
  tax is off.

Note this adds a third piece of customer data alongside name and phone, which strengthens the
case for encrypting backups (D17).

## D26 — Overview is a dashboard that answers what to buy next

A fifth screen, ahead of the three requested ones: revenue and cash for the week, best waiter,
best seller, items below their minimum, and a reorder table.

**The reorder table ranks by nights of stock remaining, not by quantity.** Units on hand answers
nothing on its own; on-hand divided by units sold per night answers "what runs out first", and
pairing that with earnings per night answers "what runs out first *that I cannot afford to be
without*". A top earner with 1.4 nights of cover outranks a fast mover with nine.

Without cost (D7) this ranks by revenue and velocity. With cost it would rank by earnings per
birr invested, which is the better question — a reason to revisit D7 once purchasing exists.

## D27 — The recipe BOM already exists; do not "add" it

Recorded because it will otherwise be raised as missing. §2.1–2.5 already define exactly this:
every sale item resolves through a recipe to products × quantity in base units. A beer is a
one-line recipe, a Gin & Tonic is two. **There are no special cases** — which is why cocktails
were never structural work and why the inventory layer never learns that shots or cocktails
exist. D13's categorisation sits on top of this for reporting; it does not replace it.

---

# 4. Deferred, with seams left

## D17 — Backup in Phase 1; restore later

§13.4 lists backup and restore as *"not yet built (in any version)"*, while §9.3 states that
generating the shift report triggers an automatic backup. Those contradict. Today the reality
is: one offline machine, a year of trading history, and a dead disk is total loss of every tab,
reconciliation and fiscal receipt number.

**Phase 1** delivers automatic verified backup on shift close: `VACUUM INTO` (never a
filesystem copy of an open WAL database), an external target, `PRAGMA integrity_check` plus row
counts after writing, and a retention policy. An unverified backup is a guess. Backups carry
customer names and phone numbers, so encryption is a live question for the customer.

**Restore is deferred** because it is the harder half: it rewinds the receipt sequences,
reissuing `CR-` numbers already in customers' hands. When built it must archive the current
database first, log loudly, write a restore marker, and require a credential the cashier does
not hold. Offline password recovery being impossible, a vendor override code derived from the
licence key remains the recommended answer.

## D18 — Catalogue import, and a starter catalogue that cannot be mistaken for data

A consequence of D8 that §12.1 never had to face. Per club, commissioning requires every
product, sale item, recipe and price, plus conversion factors, low-stock thresholds, opening
stock and staff. That is hours of typing repeated for every sale.

§12.1's answer was a seeded placeholder catalogue of 11 products and 18 sale items where
**every price is invented**, as is the 24-shots-per-bottle factor. Acceptable for one client who
was going to overwrite it; actively dangerous shipped to many venues, because an unnoticed
placeholder price bills real customers wrong.

Phase 3 delivers CSV import for catalogue and opening stock, and a starter catalogue that is
obviously a template rather than plausible data. §12.1's rule stands: the seeder runs **only on
an empty database**.

## D19 — Licensing deferred, seams left

Not built now, but the fingerprint surface, licence-check call site and nag UI slot are stubbed
so that adding it is not surgery. §13.4's design intent is kept when it is built: Ed25519
signed licence verified offline with the private key never shipping, machine fingerprint
matched **n-of-m** so one component change does not invalidate it, monotonic last-seen to catch
clock rollback, and **nag, never hard-block, and never during an open shift**.

## D20 — Printing stubbed behind a trait

The `Printer` trait with a file-writing implementation, exactly as §13 recommends for
development. The three-transaction protocol of §6.3, the recovery gate of §4.4 and the
resolution screens of D10 are all built for real; only the device layer is fake.

**This is the highest-risk unknown in the port and it is not a Tauri question.** §14.2 still
lists printer make, model and interface as open, while §6.3 branches on *"if the device reports
FAILURE"* — a branch that is only real with bidirectional status (`DLE EOT`). Straightforward
over serial; materially harder over Windows `winspool` RAW; impossible on some interfaces, in
which case every print is ambiguous, the handwritten-chit path of §6.3 becomes the common case
rather than the rare one, and the UX changes.

**Recommended: a one-week spike against the actual hardware before Phase 3.** Independent of
everything else in this plan.

## D21 — Data seeded through the real command layer

No hand-written SQL fixtures. A development seeder drives realistic nights through the actual
Rust commands — open shift, open tabs, issue rounds, correct, void, close tabs, reconcile, pay
out, close shift — so every row is ledger-valid by construction and the invariant tests are
testing something. The seeded history deliberately includes edge cases: a tab carried across
nights, a reconciliation shortfall, a comp, a write-off, a stranded print.

---

# 5. Risks carried forward

| | |
|---|---|
| **Printer interface unknown** | §14.2 open question, and the print state machine depends on failure detection. D24 adds a second dependency: the QR needs `GS ( k` or a raster fallback. Both belong on one hardware spike before Phase 3. See D20. |
| **Owner PIN has no recovery** | D22 gates settings behind the owner account on an offline machine. Forget it and the venue cannot change a tax rate. Same shape as the restore-credential problem in §13.4, and should share its answer — a vendor override derived from the licence key. |
| **Owner and cashier share one machine** | D22 separates what each may do, not where they do it. An owner who leaves their session open has effectively handed over settings access. Session timeout on the owner role is the cheap mitigation and should be built with the login, not after. |
| **Receipt encoding constrains the market** | §6.12 — thermal printers render CP437/CP850 from font ROM, and Latin-only was confirmed *with one client*. The UI localises cheaply; receipts do not, without driving the printer in graphics mode. Relevant the moment a venue wants Tigrinya or Amharic receipts. |
| **Bar shrinkage is unattributable** | §14 — bartenders are not modelled, so a stock-count shortfall cannot be pinned to anyone. `staff.role` already exists, so adding on-duty bartenders per shift is a data change, not a schema change. |
| **Clock tampering** | §13 — changing the Windows date moves business dates and therefore which night money landed in. Non-monotonic clock detection is cheap in Phase 0 and awkward to retrofit. |
| **Sales-by-product has no prior art** | §13.4 — never built in any version. The reports page is net-new rather than a port: no bad implementation to inherit, but also no worked examples to check the numbers against. |
| **No manager permission layer** | Unchanged from §0.3, and deliberately so. The shift report remains the sole compensating control, which is why D9's stock overrides had to become a ninth itemised exception block rather than a total. |

---

# 6. Open questions still with the client

Carried from §14.2, minus those resolved above: printer make, model and interface · whether a
waiter shortfall may be written off and by whom · the age at which an open tab is escalated or
written off · two open tabs sharing one reference · tab merging · discounts, happy hour, staff
drinks · partial-bottle counting method · backup destination, retention and encryption ·
restore credential recovery · code signing certificate (roughly USD 200–400/year) · cash payout
categories · who maintains the system after handover.
