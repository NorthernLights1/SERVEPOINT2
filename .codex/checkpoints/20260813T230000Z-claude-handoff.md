# Handover — ServePoint, 13 August 2026 (late)

You are starting cold. This is everything needed; nothing is assumed.

Supersedes every earlier checkpoint here, including
`20260813T180000Z-claude-handoff.md`. **The Reports question it left open is
now answered and built** — see §4. Nothing in this file is still blocked.

---

## 1. Standing constraints — read these first

- **Do not read `/home/temeg242/Documents/Project/ServePoint/ai-context/`**
  (`04-decisions.md`, `06-shift-report-design.md`, `07-specification-v2.md`,
  `08-data-model.md`). Withheld deliberately by the user: *"the reason i didn't
  want it because it was a bad implementation."* Design questions get answered
  fresh, never from that folder.
- **Nothing hardcoded for one venue.** This ships to many bars and clubs.
- **Reports to the user: short, plain language, no build narration.** They read
  the final summary, not the reasoning.
- **The ponytail skill does not survive a session boundary.** Re-invoke
  `/ponytail full` at the start and confirm it is active in every reply — the
  user asked to be reminded each time.
- **Scope discipline, stated by the user directly:** *"otherwise we are building
  a mini erp for bars which is not the goal."* Suppliers, purchase orders and
  invoice matching are **out of scope**. When a feature starts growing an
  entity nobody asked for, stop and say so.
- The project has **no prettier and no eslint**. Do not add either unasked.
- A **GateGuard hook** blocks the first Bash call and the first edit of every
  file, demanding importers / affected API / data shapes / the user's verbatim
  instruction. State the facts, then retry the identical call. It fires 15+
  times a session. `ECC_GATEGUARD=off` disables it if the user agrees.

---

## 2. State

Branch `main`.

```
0f81350 docs: handover for a fresh session
c94faee feat: pour by measure — recipes in ml or grams (migration 0014)
6a0ef17 chore: ignore .env files and spooled slips
7be767c feat: order corrections, generated codes, and a one-step catalogue
```

Since `0f81350`, one commit landed (Reports, `f8948f3`) and the deliveries work
described in §4 sits on top of it.

Verification, all currently green:

```
cd src-tauri && cargo test            # 296 lib + 11 integration, 1 ignored
cd src-tauri && cargo build --release
npm run build                         # tsc --noEmit && vite build
rustfmt --edition 2021 --check src-tauri/src/*.rs src-tauri/src/repo/*.rs
```

`cargo fmt` is broken in this checkout — call `rustfmt` directly as above.
**40 registered commands** in `main.rs`, 40 called from `api.ts`. Re-diff the
two lists after any command change:

```
grep -cE '^\s+[a-z_]+::cmd_' src-tauri/src/main.rs
grep -oE '"cmd_[a-z_]+"' src/api.ts | sort -u | wc -l
```

### Running it

```
SERVEPOINT_DB=~/.cache/servepoint-dev/servepoint.db npm run app
```

X11 display `:0`. `npm run app` is `tauri dev`, which watches `src-tauri/` and
rebuilds and restarts on change — so a Rust edit re-runs migrations against the
dev database automatically. Find the window with
`wmctrl -l | grep ServePoint | grep -v "Visual Studio Code"` — VS Code's title
contains the project name and matches a naive grep. `xdotool` is **not
installed** and `gnome-screenshot` writes no file from an agent context, so
visual verification has to be done by the user.

The dev database holds real test data. **Do not delete it** — it is what proves
the migrations work. Slips are written as **files** to
`~/.cache/servepoint-dev/slips/`; there is no printer driver, only `FilePrinter`
behind the `Printer` trait.

There is no `sqlite3` binary. To inspect the database, compile a throwaway
reader against the already-built rusqlite:

```
rustc --edition 2021 -L target/debug/deps \
  --extern rusqlite=$(ls target/debug/deps/librusqlite-*.rlib | head -1) peek.rs -o peek
```

---

## 3. Architecture — the parts that are load-bearing

Tauri v2 desktop app. Rust backend in `src-tauri/`, React 19 + TypeScript +
Vite frontend in `src/`. SQLite via `rusqlite`, **14 numbered migrations**,
schema triggers as the final guard (the `guarded!` macro turns a constraint
error into a readable refusal, reusing the trigger's own message).

```
repo/*.rs          statement-level, never opens a transaction
   ↓
trading.rs, settlement.rs, commissioning.rs, correction.rs, receiving.rs
                   own transactions, couple effects to audit rows
   ↓
floor.rs, venue.rs, bills.rs, reconcile.rs, recovery.rs,
overview.rs, corrections.rs, report.rs
                   command modules, roughly one per screen
   ↓
src/api.ts         the single door to the webview
```

Rules that are enforced and must not be quietly broken:

- **Money crosses the boundary as a pre-formatted `String`.** The webview
  computes nothing. Inbound money is typed text parsed by `Money::parse`.
- **Quantities cross as `i64` thousandths** (`Milli`, `quantityMilli`). The only
  arithmetic allowed in a component is `× MILLI`, which is a unit conversion.
- **Derived state is never stored** (`lib.rs` rule 1). Stock and balances are
  always `SUM()` over ledgers. The one deliberate exception is a **closed**
  shift's report — see §4.
- Every write command returns **the whole refreshed view**.
- `CommandError { kind, message }`. Kinds: `SIGNED_OUT`, `NOT_PERMITTED`,
  `REFUSED`, `DATABASE`, `PRINT_PENDING`, `NO_PRINTER`, `LOCKED`, `BAD_PIN`.
- Hash-chained audit ledger via `ledger::append`.
- **Two-phase issue protocol**: durable `PRINTING` reservation → device I/O with
  no lock held → stock and audit commit. The most delicate code in the system.
- `serde(rename_all = "camelCase")` on views, `UPPERCASE` on wire enums.
- Files ≤ 800 lines.
- **Any `can_do_x` in a view must have the identical check in the command.**

---

## 4. What this session changed

### Reports (`f8948f3`) — the open question, answered

**The question was never actually open.** `0009_audit.sql` had already built
`shift_reports` with `report_json`, `rendered_text`, a partial unique index
`shift_reports_one_final … WHERE is_provisional = 0`, a trigger requiring a
closed shift, and no-update/no-delete triggers. Its own comment settles it:
*"D11: past shifts are READ from here and never recomputed, so the report and
the paper can never drift apart."* The UI placeholder had committed to the same
thing. Both halves of the system had already decided; only the module was
missing.

This does **not** violate `lib.rs` rule 1. Rule 1 forbids caching state that can
still change. A closed shift cannot change — corrections are refused outside
their own `OPEN` shift by trigger — so the report is a record, not a cache.

Built: `repo/reports.rs` (statement-level) and `report.rs` (compile → render →
freeze, plus `cmd_reports_view`). `reconcile::close_night` freezes the report
**inside the existing close transaction** and audits `SHIFT_REPORT_STORED`, so
a night that closes always has its report and one that fails has neither.

Money in the stored JSON is **already formatted**. A report read back years
later must not be re-rendered under today's currency setting. The test
`a_report_is_read_back_and_never_recompiled` changes `CURRENCY_CODE` after close
and asserts the stored report is unchanged. Do not "fix" this by storing minor
units.

Reports are **owner-only** (`require_owner`). Nights closed before this existed
show a distinct empty state rather than a fabricated report.

### Deliveries — restocking, with no supplier

Driven by the user spotting a real gap: *"its not a one time thing though
everytime it runs low needs to be added right ?"* They were right. Every
`stock::post` caller was traced and **there was no way to record a restock at
all**. `commissioning::record_opening_stock` ("Count in", on the Catalogue) is
once-only — it refuses any product that has stock history.

Scope was set by the user: *"no supplier at all this is out of scope … just a
batch to keep track of which purchased when at what price and the batch should
be automatic."*

**The conflict and how it was resolved.** `purchases.supplier_id` is
`NOT NULL REFERENCES suppliers(id)`, and `stock_movements` refuses a `PURCHASE`
without a `purchase_id`. Dropping the NOT NULL needs a table rebuild, and
`purchases` has inbound foreign keys — the rebuild migrations here cannot do
(see §8). The user agreed not to rebuild. So **one standing `suppliers` row
named "Deliveries" is created on first use and never shown anywhere.** The
column stays honest; if suppliers ever become in scope it is already there.

**No migration was needed.** The schema was already shaped for this:
`invoice_ref` is nullable and its own comment notes NULLs do not collide in a
unique index, so unlimited no-paperwork deliveries are legal. The batch is the
`purchases` row id — nobody types one.

New: `repo/purchases.rs` (`house`, `open`, `add_line`, `reaverage`, `recent`)
and `receiving.rs` (transaction owner + `cmd_receive_delivery`).
`InventoryView` gained a `deliveries` field; the Inventory screen gained a
"Receive a delivery" form and a "What came in" history.

Three things worth not breaking:

- **The UI asks only for the total paid**, never a per-unit price.
  `purchase_lines` has a CHECK that the two must agree within one santim;
  `unit_rate` derives the rate with the *same arithmetic as the CHECK*, so a
  line built from an exact total can never be refused for disagreeing with
  itself. Asking for both invites two numbers that drift.
- **`reaverage` runs BEFORE `stock::post`.** The standing average belongs to the
  shelf as it stood; posting first blends the crate into itself. The test
  `the_average_cost_blends_the_old_shelf_with_the_new_crate` fails if reordered.
- **§8.2 re-averages from the exact total, never the rounded rate**, so rounding
  cannot accumulate into the cost of goods.

Receiving requires **a session, not the owner**. §8.1's own comment explains
why: deliveries arrive while the club is shut, and refusing one because nobody
is trading pushes the venue into entering it wrong later. Who received it is
audited (`STOCK_RECEIVED`). One-line change if the user wants owner-only.

### Inventory: Items / Value toggle

The user rejected a proposed role gate on cost visibility and gave direction
instead: *"it should be a toggle between cash value view vs physical item
view."* Built as a `Segmented` control, frontend-only. The mode does not persist
across reloads — offered, not asked for.

### Flaky test marked skipped

`repeated_wrong_pins_lock_the_keypad` is now `#[ignore]`d with the diagnosis in
the attribute, at the user's request: *"skip the repeated wrong pins lock the
keypad, note it as skip."* Cause is real, not a test artefact — see §6.

### Count in removed; removal and the owner at the till added

Three changes the user asked for together.

**"Count in" is gone.** `commissioning::record_opening_stock`, its `OpeningStock`
input, `venue::add_opening_stock`, `cmd_add_opening_stock`, the `api.ts` entry
and the Catalogue component were all deleted — deliveries do the same job and
work forever. Its two integration tests went with it (13 → 11), but one guarded
a property deliveries also need: a failed audit must roll back the cost *and*
the stock. That test was **ported** to `receiving.rs` rather than dropped.

Its assertion is worth knowing: the fixture seeds `avg_cost_minor = 10000`, not
zero, so the test captures the standing cost first and asserts it is unchanged.
An `assert_eq!(cost, 0)` there looks right and fails.

**Removing catalogue items.** `commissioning::set_product_active` /
`set_sale_item_active`, reached from the Items and Composed drinks cards.
Removal is deactivation — the schema refuses `DELETE` outright, and an order
line printed last year names the product. Refusals, all on the deactivate path
only: stock still on hand, a live recipe still pouring the product (the message
names the drink), or the drink sitting on an open tab. Reactivation is never
refused, and **editing is never gated** — that was explicit
(*"it should always allow for editing"*).

`catalogue::every_product` was added because `products()` filters `active = 1`;
the Catalogue needs the unfiltered list or a removed item could never be
brought back.

**The owner may work the till.** This was gated in **two layers**: the command
layer, and three near-identical private `require_cashier(conn, id)` copies in
`repo/shifts.rs`, `repo/tabs.rs` and `repo/cash.rs`. Changing one alone lets a
command through the front door and refuses it two layers down. The three copies
are now one `staff::require_till_operator`, and `floor::require_cashier` was
renamed `require_till` — a function named for a rule it no longer enforces is
how someone loses an evening. Waiters are still refused, with tests.

Six existing tests asserted the old rule. They were **re-pointed at a waiter**
rather than deleted, so the negative case survives; only its subject changed.
`owner_cannot_begin_end_of_day` is now `a_waiter_cannot_begin_end_of_day`.

---

## 5. Open — needs the user, do not guess

Nothing is blocking. One decision was offered and not taken:

- **`reporting.show_cost` is wired to nothing.** Only its definition, the
  validation list and the Settings field reference it. Either wire it or delete
  it; a setting that does nothing is worse than either.

One cosmetic thing the user may want to revisit: the owner's sidebar is now
seven entries, with Till and End of day sitting beside Settings. Grouping or
reordering was offered and not answered.

## 6. Known issues, noted not fixed

**`repeated_wrong_pins_lock_the_keypad` flakes under load.** Passes alone, fails
in the full suite, now `#[ignore]`d. `FREE_ATTEMPTS` is 3 and `FIRST_LOCKOUT_MS`
is 5 seconds, but hashing one PIN costs about that long on this hardware. The
first lockout can expire *during the next attempt's own hashing*, so a correct
PIN gets through when it should be refused. On a slow till the first lockout
buys almost nothing. Options: raise `FIRST_LOCKOUT_MS`, start the lock when the
attempt begins rather than when the hash finishes, or accept it.

**`base_units_per_pack` and `units_per_purchase_pack` are inert.** Stored and
audited, never multiplied by. Deliveries were built in **base units**, which is
what the ledger keeps, so these still earn nothing. If the user ever asks to
receive "3 crates of 24", that is where the conversion goes.

---

## 7. What is left, in the order it is worth doing

1. **Stock counts.** `DraftStockCount` has a resolution entry in `resolution.rs`
   and no screen. `stock_counts` and the `ADJUSTMENT` movement type already
   exist, with a trigger requiring variance to equal counted − system exactly.
   This is the last inventory gap and the natural next piece.
2. **`backup.rs`** has no command surface.
3. A re-audit of test-only `pub` functions.

---

## 8. Gotchas that cost time

- The 10% default service charge makes money assertions look wrong. A 50.00
  round settles at 55.00.
- Printing tests need a **file-backed** database: `slip_directory` derives the
  spool path from `pragma_database_list`, and an in-memory database has none.
- Inserting settings with raw SQL fails on `NOT NULL value_type` — use
  `settings::put(conn, key, value, staff_id: Option<i64>, at: i64)`. It takes
  **five** arguments; a two-argument call is a compile error that reads as if
  the function were missing.
- An owner or cashier row needs `pin_hash`/`pin_salt`, enforced by a trigger.
- SQLite cannot `ALTER` a `CHECK`; widening one means rebuilding the table,
  which `0011` and `0012` both do. **A table with inbound foreign keys cannot be
  rebuilt that way** — migrations run inside a transaction where
  `PRAGMA foreign_keys` cannot be toggled. This blocks `products` *and*
  `purchases`. Use `ADD COLUMN`, which does accept a column-level `CHECK`.
- CSS tokens are `--r-sm`, `--surface`, `--line`, `--ink`, `--gap-3`. There is no
  `--radius-2` or `--surface-sunken`; grep `styles.css` before inventing one.
- `repo/fixture.rs` has no shift helpers. Tests needing a closed night build one
  through `shifts::open` / `begin_closing` / `close`.
- `cd` in a compound Bash command can trigger a permission prompt; prefer
  absolute paths, and remember the shell's cwd persists between calls.
