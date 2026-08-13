# Handover — ServePoint, 13 August 2026

You are starting cold. This is everything needed; nothing is assumed.

Supersedes every earlier checkpoint here, except that the **Reports question**
in `20260812T210000Z-claude-handoff.md` §2 is still open and still must not be
guessed. Summary of it in §5 below.

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
- The project has **no prettier and no eslint**. Do not add either unasked.
- A **GateGuard hook** blocks the first Bash call and the first edit of every
  file, demanding importers / affected API / data shapes / the user's verbatim
  instruction. State the facts, then retry the identical call. It fires 15+
  times a session. `ECC_GATEGUARD=off` disables it if the user agrees.

---

## 2. State

Branch `main`, clean, pushed to `https://github.com/NorthernLights1/SERVEPOINT2.git`.

```
c94faee feat: pour by measure — recipes in ml or grams (migration 0014)
6a0ef17 chore: ignore .env files and spooled slips
7be767c feat: order corrections, generated codes, and a one-step catalogue
b59e797 chore: preserve recovered Claude baseline
```

Verification, all currently green:

```
cd src-tauri && cargo test            # 272 pass (259 lib + 13 integration)
cd src-tauri && cargo build --release
npm run build                         # tsc --noEmit && vite build
rustfmt --edition 2021 --check src-tauri/src/*.rs src-tauri/src/repo/*.rs
```

`cargo fmt` is broken in this checkout — call `rustfmt` directly as above.

Git identity is set **repo-locally** to `Temesgen <temeg242@gmail.com>`,
inferred from the environment and the owner row. Correct it if wrong.

### Running it

```
SERVEPOINT_DB=~/.cache/servepoint-dev/servepoint.db npm run app
```

X11 display `:0`. `npm run app` is `tauri dev`, which watches `src-tauri/` and
rebuilds and restarts on change — so a Rust edit re-runs migrations against the
dev database automatically. Screenshot with `gnome-screenshot -w` after
`xdotool windowactivate` (find the window via `wmctrl -l | grep ServePoint`).

The dev database holds real test data: three products, a composed drink, staff,
two settled sales, two spooled slips. **Do not delete it** — it is what proves
the migrations work. Slips are written as **files** to
`~/.cache/servepoint-dev/slips/`; there is no physical printer driver, only
`FilePrinter` behind the `Printer` trait.

There is no `sqlite3` binary on this machine. To inspect the database, compile a
throwaway reader against the already-built rusqlite:

```
rustc --edition 2021 -L target/debug/deps \
  --extern rusqlite=$(ls target/debug/deps/librusqlite-*.rlib | head -1) peek.rs -o peek
```

---

## 3. Architecture — the parts that are load-bearing

Tauri v2 desktop app. Rust backend in `src-tauri/`, React 19 + TypeScript +
Vite frontend in `src/`. SQLite via `rusqlite`, **14 numbered migrations**,
schema triggers as the final guard (the `guarded!` macro turns a constraint
error into a readable refusal).

```
repo/*.rs          statement-level, never opens a transaction
   ↓
trading.rs, settlement.rs, commissioning.rs, correction.rs
                   own transactions, couple effects to audit rows
   ↓
floor.rs, venue.rs, bills.rs, reconcile.rs, recovery.rs,
overview.rs, corrections.rs
                   command modules, roughly one per screen
   ↓
src/api.ts         the single door to the webview
```

Rules that are enforced and must not be quietly broken:

- **Money crosses the boundary as a pre-formatted `String`.** The webview
  computes nothing. Inbound money is typed text parsed by `Money::parse`.
  `api.ts` puts it best: *"A total worked out in JavaScript is a total that will
  one day disagree with the printed receipt, and the receipt is the thing the
  customer is holding."*
- **Quantities cross as `i64` thousandths** (`Milli`, `quantityMilli`). The only
  arithmetic allowed in a component is `× MILLI`, which is a unit conversion.
- **Derived state is never stored** (`lib.rs` rule 1). Stock and balances are
  always `SUM()` over ledgers.
- Every write command returns **the whole refreshed view**.
- `CommandError { kind, message }`. Kinds: `SIGNED_OUT`, `NOT_PERMITTED`,
  `REFUSED`, `DATABASE`, `PRINT_PENDING`, `NO_PRINTER`, `LOCKED`, `BAD_PIN`.
- Hash-chained audit ledger via `ledger::append`.
- **Two-phase issue protocol**: durable `PRINTING` reservation → device I/O with
  no lock held → stock and audit commit. This is the most delicate code in the
  system; corrections reuse it exactly.
- `serde(rename_all = "camelCase")` on views, `UPPERCASE` on wire enums.
- Files ≤ 800 lines.
- **Any `can_do_x` in a view must have the identical check in the command.** A
  bug was found where they disagreed and stranded a waiter with money and no way
  to settle. Do not repeat it.

37 registered commands in `main.rs`. Every `cmd_*` called from `api.ts` is
registered and every registered command is used — worth re-diffing the two lists
after any command change.

---

## 4. What the last session changed

### Order corrections and voids (`7be767c`)

Codex had written the correction surface and stopped mid-flight without a note.
Finished: fixed the build, registered the module in `main.rs`, and reached
`OrderCorrection.tsx` from the Till.

**The bug worth knowing about.** `correction::prepare` commits the replacement
order in `PRINTING` before any printer I/O, exactly as an ordinary round does.
When that slip failed, the replacement appeared in the Recovery list — correct —
but both answers there routed to `trading::authorize_handwritten` /
`confirm_non_print`, which know nothing about corrections. "I wrote it by hand"
issued the replacement as a **fresh round** while the original stayed `ISSUED`:
the tab was billed twice, stock moved by the full replacement rather than the
delta, and the `pending_order_corrections` row was never cleared, so the night
could not close either. `recovery.rs` now asks `frozen_correction()` and routes
to `correction::complete` / `abandon`.

**A guard was deliberately removed.** `complete` and `abandon` refused anyone but
the cashier who began the correction. That check only ever fired on the recovery
path — on the normal path `prepare` and `complete` take the same session by
construction — and there it was a hard lock: a stranded correction holds
`PRINTING`, `shifts::recovery_complete` blocks on `PRINTING`, and `shifts::close`
blocks on that. A cashier going home meant the venue could never close the night,
from any screen, as any role. Both guards are gone; the audit records whoever
actually answered. **If you want the restriction back, it needs a different exit
for the abandoned case first — do not simply re-add it.**

### Generated codes (`7be767c`, migration 0012)

Products, menu items and staff take `PRD-` / `ITM-` / `STF-` numbers from the
same `seq.rs` allocator that issues `BR-` receipt numbers, inside the same
transaction as the insert. Existing hand-typed codes were renumbered in **two
passes** so the rewrite could not collide on the `UNIQUE` constraint. Codes are
immutable once assigned — `update_*` reads the existing code and passes it
through. Category became a `<datalist>` combobox (native, no dependency).

Codes are **not** derived from category or name: a recategorised product would
carry a code that lies, and reissuing it breaks anything already printed.

### One-step catalogue (`7be767c`, migration 0013)

A club sells bottles, so adding an item creates the shelf product, the menu
entry, a 1:1 recipe and the price **in a single transaction**, unless "sell this
on the menu" is unticked. There is no longer a state where the screen shows a
finished item the till refuses — a real failure the user hit. 
`sale_items.from_product_id` records the pairing; existing one-for-one items
were linked by backfill. The Catalogue is now **Items** + **Composed drinks** +
**Who works here**.

The product/menu split was deliberately **kept**. It is what lets one gin bottle
pour 24 shots and what makes stock reconcile. The auto-created recipe is an
ordinary recipe, so availability, issue slips and corrections work on it
unchanged, and the audit log cannot tell which screen was used.

### Pour by measure (`c94faee`, migration 0014)

Researched how the industry does it — Restaurant365 fixes a measure type per
item; Backbar and the pour-cost tools cost recipes in ml or oz — and adopted it.
An item declares what one counted unit holds:

```
products.content_measure         NONE | ML | GRAM
products.content_per_unit_milli  750000 for a 750ml bottle
```

Recipes are then written in that measure — `30` for 30ml — and
`catalogue::measure_to_units` converts **in Rust**, once, at definition.

**Stock deliberately did not change.** `stock_movements` and `recipe_lines` keep
meaning thousandths of a *counted* unit, so no existing row was restated. 30ml of
a 750ml bottle stores as `40`. Lines are read back in the measure they were typed
in via `units_to_measure`, so the screen never shows a fraction.

Ceiling, marked with a `ponytail:` comment in the migration: lines round to a
thousandth of a counted unit, 0.75ml on a 750ml bottle. It happens once, so every
pour draws an identical amount rather than drifting, and it is an order of
magnitude under real pour variance. The upgrade, if ever needed, is holding stock
in the measure itself and restating on-hand — a migration of ledger rows.

Also **removed the "Units per pack" and "Bought in packs of" inputs**. Both were
inert — nothing multiplied by either — and the first actively misled, since
setting "24 shots per bottle" looked like it would make the shelf convert. The
columns remain for the purchasing work that will use them.

---

## 5. Open — needs the user, do not guess

**Reports.** Still blocking that screen. Freeze a `shift_reports` row at close,
or recalculate from the ledgers? `lib.rs` says derived state is never stored; the
screen promises a night reads back exactly as stored. The precedent of
`freeze_rendered_text` and `cash::freeze_payment` argues for freezing, and that
was the standing recommendation. The withheld `ai-context/` folder contains
`06-shift-report-design.md`, so the question must be answered fresh.
**A wrong guess here is expensive to unwind.**

---

## 6. Known issues, noted not fixed

**`repeated_wrong_pins_lock_the_keypad` flakes under load.** Passes alone, fails
in the full suite. The cause is real, not a test artefact: `FREE_ATTEMPTS` is 3
and `FIRST_LOCKOUT_MS` is 5 seconds, but hashing one PIN costs about that long on
this hardware — the test alone takes ~27s. The first lockout can therefore expire
*during the next attempt's own hashing*, and a correct PIN gets through when it
should be refused. On a slow till the first lockout buys almost nothing. Left
alone because it is auth code found during unrelated catalogue work; the user
said "note it and move on". Options: raise `FIRST_LOCKOUT_MS`, start the lock
when the attempt begins rather than when the hash finishes, or accept it.

**`base_units_per_pack` and `units_per_purchase_pack` are inert.** Stored and
audited, never multiplied by. Their UI inputs were removed in `c94faee`;
`0002_catalogue.sql` already admits the second is "invisible in the UI today".

---

## 7. What is left, in the order it is worth doing

1. **Reports** — blocked on the question above.
2. **Purchases and deliveries.** `0006_purchasing.sql` exists with no repo
   module. This is where the two inert pack columns finally earn their place,
   and where purchase price per delivery date belongs. The user has already said
   cost belongs on receiving, which is how `add_opening_stock` already works.
3. **Stock counts.** `DraftStockCount` has a resolution entry in `resolution.rs`
   and no screen.
4. **`backup.rs`** has no command surface.
5. A re-audit of test-only `pub` functions.

---

## 8. Gotchas that cost time last session

- The 10% default service charge makes money assertions look wrong. A 50.00
  round settles at 55.00.
- Printing tests need a **file-backed** database: `slip_directory` derives the
  spool path from `pragma_database_list`, and an in-memory database has none.
- Inserting settings with raw SQL fails on `NOT NULL value_type` — use
  `settings::put`.
- An owner or cashier row needs `pin_hash`/`pin_salt`, enforced by a trigger.
- SQLite cannot `ALTER` a `CHECK`; widening one means rebuilding the table, which
  `0011` and `0012` both do. **`products` cannot be rebuilt that way** — it has
  inbound foreign keys, and migrations run inside a transaction where
  `PRAGMA foreign_keys` cannot be toggled. Use `ADD COLUMN`, which does accept a
  column-level `CHECK`.
- Schema-version assertions derive from `MIGRATIONS` rather than hardcoding a
  number, so adding a migration no longer breaks six tests.
- `cd` in a compound Bash command can trigger a permission prompt; prefer
  absolute paths, and remember the shell's cwd persists between calls.
