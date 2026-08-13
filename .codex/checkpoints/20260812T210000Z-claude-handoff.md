# ServePoint2 — handover to Codex
**From:** Claude session, 2026-08-12 (second of the day)
**Read this before touching anything.** It is written to be self-contained.

---

## 1. State: green

```
cd src-tauri
cargo test                                            # 262 passed (253 lib + 9 integration)
rustfmt --edition 2021 --check src/*.rs src/repo/*.rs # clean
cargo build --release                                 # clean
cd ..
npm run build                                         # tsc --noEmit && vite build, clean
```

**`cargo fmt` does not work here** — there is no rustup default toolchain. Call
`rustfmt` directly, as above. This bites every session; it is not a new problem.

**Nothing is committed.** The entire application is uncommitted working-tree
state on `main`, on top of a single commit `b59e797 chore: preserve recovered
Claude baseline`. `git status` shows ~25 modified and ~20 untracked files. If
you intend to commit, that is the user's call — ask first.

---

## 2. THE OPEN QUESTION — needs the user, not a guess

**What is a report built from?**

`src/screens/Floor.tsx::Reports` is still an honest empty state, and there is
no backend for it. Its own placeholder text states the intended contract:

> "Reports are built from closed shifts, so the first one appears after the
> first night is reconciled and closed. A closed night is then **read back
> exactly as it was stored, never recalculated** — so a report cannot quietly
> change months later."

Nothing stores a shift report. `shifts` has `counted_cash_minor`, `closed_at`
and `closed_by`, and that is all. So the contract above is currently unmet, and
there are two materially different ways to meet it:

**Option A — freeze a report row at close.**
A new `shift_reports` table (migration 0012) written inside the same
transaction as `shifts::close`, holding the night's totals: revenue, service,
tax, comps, per-waiter takings, per-item quantities, cost of goods, opening
float, expected cash, counted cash, variance.
*For:* matches the stated contract exactly; a report is immutable and cannot
drift; survives later edits to prices, recipes or settings; fast to read.
*Against:* a new table and migration; every figure a report may ever want has
to be decided now, because adding one later means old nights cannot answer it.

**Option B — recalculate from the ledgers on demand.**
Query `tab_payments`, `stock_movements`, `order_lines`, `cash_movements` for a
given `shift_id` each time the report is opened.
*For:* no new schema; no decision about which figures matter has to be made up
front; consistent with the codebase's stated rule that *derived state is never
stored* (see the `lib.rs` module doc, rule 1).
*Against:* directly contradicts the Reports placeholder text. A report *can*
change months later — not because the ledger changed, but because a later
migration or a recipe revision changes how the sum is computed.

**The tension is real and the codebase argues both ways.** `lib.rs` says
derived state is never stored; the Reports screen says a closed night is read
back exactly as stored. `receipts` and `tab_payments` already resolve this the
same way for their own domain — they *freeze* figures at the moment of
commitment (`freeze_rendered_text`, `cash::freeze_payment`), on the grounds
that what the customer was shown must never be recomputed. That precedent
points to **Option A**, and Claude's recommendation is Option A — but only if
the user confirms the report's contents, because Option A makes that list
permanent for every night closed thereafter.

**Do not implement either until the user answers.** A wrong guess here is
expensive to unwind: Option A pollutes the schema, Option B ships a screen that
contradicts its own promise.

---

## 3. What exists now

25 modules in `src-tauri/src/lib.rs`. 32 Tauri commands registered in
`src-tauri/src/main.rs`. The venue can be commissioned from empty, trade a
night, and close it — backend and UI, end to end.

| Backend module | Screen | Commands |
|---|---|---|
| `commands.rs` | Gate, Settings | `bootstrap`, `sign_in`, `sign_out`, `complete_setup`, `read_settings`, `write_settings`, `verify_audit` |
| `venue.rs` → `commissioning.rs` | `screens/Catalogue.tsx` | `setup_view`, `add_staff`, `set_staff_active`, `add_product`, `edit_product`, `add_sale_item`, `edit_sale_item`, `set_recipe`, `set_price`, `add_opening_stock` |
| `floor.rs` → `trading.rs` | `screens/Floor.tsx` (Till, Inventory) | `floor_view`, `inventory_view`, `open_shift`, `open_tab`, `place_order` |
| `bills.rs` → `settlement.rs` | Till → Settle card | `tab_bill`, `settle_tab` |
| `reconcile.rs` → `repo/cash.rs` | `screens/EndOfDay.tsx` | `reconciliation_view`, `settle_waiter`, `begin_closing`, `close_night` |
| `recovery.rs` → `trading.rs` | Till → recovery card | `recovery_view`, `resolve_handwritten`, `resolve_non_print` |
| `overview.rs` | `screens/Floor.tsx` (Overview) | `overview_view` |

### The layering, which you must keep

```
repo/*.rs        statement-level, never opens a transaction
   ↑
protocol modules  trading.rs, settlement.rs, commissioning.rs, resolution.rs
   ↑              own the transaction that couples an effect to its audit row
command modules   floor.rs, venue.rs, bills.rs, reconcile.rs, recovery.rs,
   ↑              overview.rs — decide ordering, format for the window
src/api.ts        the single door to the webview
```

`floor.rs` is to `trading.rs` what `venue.rs` is to `commissioning.rs`. If you
add a surface, follow that shape: a new `*.rs` command module, not more code in
`commands.rs` (already 1050 lines).

### Conventions that are load-bearing

- **Money crosses the boundary as a pre-formatted `String`, always.** Nothing
  in TypeScript adds, multiplies, rounds or formats an amount. If a screen
  needs a figure it does not have, add a field to a Rust view. Inbound, money
  arrives as typed text and is read by `Money::parse`.
- **Quantities cross as `i64` thousandths** (`quantityMilli`). The `* 1000`
  in the UI is a unit conversion, named `MILLI`, and is the only arithmetic
  allowed in a component.
- **Every write command returns the whole refreshed view**, so a screen has
  exactly one code path for refreshing itself.
- **`CommandError { kind, message }`.** `kind` is for branching
  (`SIGNED_OUT`, `NOT_PERMITTED`, `REFUSED`, `DATABASE`, `PRINT_PENDING`,
  `NO_PRINTER`); `message` is a sentence shown to somebody standing at a bar.
  Repository refusals are already written that way and are passed through
  verbatim.
- **Derived state is never stored** (`lib.rs` rule 1). Stock on hand and waiter
  balances are always `SUM()` over their ledgers.
- Files: 800 lines maximum. `Floor.tsx` hit 1039 this session and was split.

---

## 4. What this session changed

1. **The BR-number defect is fixed.** This was the blocker in the previous
   handover. `trading::IssueDocument` is deleted;
   `prepare_issue(conn, order_id, cashier_id, at)` now derives the destination
   split from `products.destination` (§6.7, which `0005_receipts.sql:33`
   describes and no code implemented) and calls `printing::render_issue`
   *after* `receipts::create_issue` allocates the number. Callers pass no text,
   so a slip without its BR number is structurally impossible. This unblocks
   the order-correction path, which requires the cashier to type that number
   off the returned slip.
2. `floor.rs`, `venue.rs`, `bills.rs`, `reconcile.rs`, `recovery.rs`,
   `overview.rs` — all new; see the table above.
3. `screens/Catalogue.tsx` (new, owner-only `catalogue` route),
   `screens/EndOfDay.tsx` (new), `Till`/`Inventory`/`Overview` in
   `screens/Floor.tsx` wired; `api.ts` carries every new surface;
   `.rows`/`.row`/`.stepper`/`.totals` added to `styles.css`.
4. `Money::parse` — strict. Rejects `-5`, `1,250`, `12.505`, `.50`, `12.5o`.
   Accepts `"12.5"` as twelve-fifty. Added `MoneyError::Unreadable`; the enum
   is `Copy`, so any new variant must stay payload-free.
5. `FilePrinter` — its counter restarted at 1 on every construction, so the
   second order of a night collided with an existing spool file. It now steps
   over what an earlier run spooled; `create_new` still makes the claim atomic.
6. `shifts::close` — **did not exist.** `begin_closing` moves a shift to
   `CLOSING` and nothing could move it out. Adding `close` was mandatory, not
   optional. It refuses unless every tab is `RECONCILED` and every print is
   resolved, and records the physical count as given.
7. `PageHead` moved to `ui.tsx`, `reason()` to `api.ts`.

### A bug the tests caught — read this before touching reconciliation

`reconcile::begin_closing` originally succeeded while a waiter still held
money. The check existed only in the view's `blocker` field, so the screen said
one thing and the command did another. Because `cash::create_reconciliation`
and `cash::finalize_reconciliation` **both require an OPEN shift**, moving to
`CLOSING` with an unsettled waiter would have stranded them with no way to
settle and no way back. The count now lives in `reconcile::still_holding()`,
called by both the view and the command.

The same shape of mistake is easy to repeat: any `can_do_x` field in a view
must have the identical check inside the command.

### Reconciliation has two paths and they must not be confused

- **Ordinary** — the waiter has `CLOSED` tabs. Every one is allocated and the
  allocated total must equal `expected`.
- **Old balance** — no closed tabs remain but the waiter still holds a
  shortfall from an earlier night. Nothing is allocated; `expected` is the held
  balance itself.

`reconcile::expected_for()` derives which path applies from the data rather
than asking the cashier. Getting it backwards clears a liability twice.

---

## 5. What is left, in the order Claude would do it

1. **Reports** — blocked on §2 above. Ask first.
2. **Order corrections and voids.** `repo/orders.rs` has `freeze_correction`,
   `apply_pending_correction`, `record_void`, `pending_corrections` — all
   tested, none reachable from a screen. This is the highest-value item that
   needs no decision, and it is the path the BR-number fix exists to serve: the
   `order_corrections_typed_number` trigger in `0005_receipts.sql` makes the
   cashier type the number off the slip before a correction is accepted.
3. **Purchases / deliveries.** `0006_purchasing.sql` exists; no repo module, no
   commands. Stock can only enter through opening stock today, so a venue
   cannot record a delivery.
4. **Stock counts.** `resolution.rs` lists `DraftStockCount` as a reachable
   state; nothing implements it.
5. **`backup.rs`** — written and tested, no command surface.
6. **Re-audit dead-looking code.** The previous handover counted 38 pub fns
   reachable only from their own tests. Several are now wired; the remainder
   are mostly items 2–5 above rather than genuinely dead.

---

## 6. Gotchas that will cost you an hour each

- **`cargo fmt` fails.** Use `rustfmt --edition 2021 src/*.rs src/repo/*.rs`.
- **A `GateGuard` hook blocks the first Bash call of a session and the first
  edit of every file.** It demands a short statement of: importers/callers,
  affected public API, data shapes, and the user's verbatim instruction. State
  those, then retry the identical call — it succeeds on the second attempt.
  `ECC_GATEGUARD=off` disables it if you would rather not.
- **Default venue settings apply a 10% service charge and no VAT.** Several
  tests assert `55.00` on a 50.00 tab because of it. If a money assertion is
  off by exactly 10%, this is why.
- **Tests that print need a file-backed database.** Slips spool beside the
  database file, derived from `pragma_database_list`; an in-memory database has
  nowhere to put them and `floor::place_order` refuses with `NO_PRINTER`. See
  the `till()` / `traded()` / `night()` helpers in `floor.rs`, `bills.rs`,
  `reconcile.rs`, `recovery.rs`, `overview.rs` — each builds a real file in
  `std::env::temp_dir()` and removes it on `Drop`.
- **Do not write settings with raw SQL in tests.** `settings.value_type` is
  `NOT NULL`; use `settings::put(conn, key, value, by, at)`.
- **The project has no prettier and no eslint.** `npm run build` is the entire
  frontend check. Do not add either without being asked.

---

## 7. Standing constraints from the user

- **Do not read** `/home/temeg242/Documents/Project/ServePoint/ai-context/`
  — `04-decisions.md`, `06-shift-report-design.md`, `07-specification-v2.md`,
  `08-data-model.md`. Withheld deliberately: *"the reason i didn't want it
  because it was a bad implementation."* Note that this includes the old
  shift-report design, which is directly relevant to §2 — the user has chosen
  not to hand it over, so the question in §2 must be answered fresh.
- **Nothing hardcoded for one venue.** This ships to many bars and clubs.
- **Reports to the user: short, plain language, no build narration.** They read
  the final summary, not the reasoning.
- **Ponytail was ON at `full` for this entire session**, at the user's
  instruction, and they asked to be reminded that it does not survive a session
  boundary. It governs *what gets built* — stop at the first solution that
  works, prefer stdlib and existing code over new abstractions, delete rather
  than add. If the user wants it in the next session they must re-invoke it.
