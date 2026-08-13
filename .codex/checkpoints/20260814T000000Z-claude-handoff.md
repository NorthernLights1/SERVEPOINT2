# Handover — ServePoint, 14 August 2026

You are starting cold. This is everything needed; nothing is assumed.

Supersedes every earlier checkpoint here, including
`20260813T230000Z-claude-handoff.md`. Two things that file left open are now
closed: **"Count in" was removed** (§4) and **editing names and prices was
built** (§4). Nothing in this file is blocked.

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
  instruction. State the facts, then retry the identical call. It fires 6+
  times a session. `ECC_GATEGUARD=off` disables it if the user agrees.

---

## 2. State

Branch `main`, remote `origin`
(`https://github.com/NorthernLights1/SERVEPOINT2.git`).

```
e15a1b4        feat: editable names and prices, deliveries, one way to stock a shelf
f8948f3        feat: shift reports, frozen at close
0f81350        docs: handover for a fresh session
c94faee        feat: pour by measure — recipes in ml or grams (migration 0014)
```

The tree that produced this commit had carried **two sessions of uncommitted
work** — deliveries/receiving and the "Count in" removal from 13 August, plus
the editing work from 14 August. They overlapped in the same files and the same
regions of `venue.rs`, `commissioning.rs`, `api.ts` and `Catalogue.tsx`, so
they went in as one commit rather than as a split that would not have compiled
in the middle. **Do not leave work uncommitted across sessions again** — it is
what forced that.

Verification, all currently green:

```
cd src-tauri && cargo test            # 297 lib + 11 integration, 1 ignored
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
  **A view that feeds an edit box carries the amount twice** — see §4.
- **Quantities cross as `i64` thousandths** (`Milli`, `quantityMilli`). The only
  arithmetic allowed in a component is `× MILLI`, which is a unit conversion.
- **Derived state is never stored** (`lib.rs` rule 1). Stock and balances are
  always `SUM()` over ledgers. The one deliberate exception is a **closed**
  shift's report: a closed shift cannot change, so the report is a record, not
  a cache.
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

## 4. What the last two sessions changed

### Editable names and prices (14 August)

The user: *"i need price of items to be edittable as well as the names of the
items."* No new commands were needed — `cmd_edit_product`, `cmd_edit_sale_item`
and `cmd_set_price` already existed and were already wired in `api.ts`. The
whole feature was a screen that never offered them, plus two defects underneath
that would have made the edits look done and be half done.

**Both defects are the interesting part. Do not reintroduce either.**

1. **A rename stopped at the shelf.** A product sold one for one has a menu
   entry holding its *own copy* of the name — `sell_within` copies it across
   when the pair is made. `commissioning::update_product` renamed only the
   product, so the till went on offering the old name while the Catalogue
   showed the new one. It now renames the twin inside the same transaction and
   audits it as `SALE_ITEM_CHANGED`. `catalogue::twin_of` finds the pair. The
   twin's own `active` flag is deliberately left alone: what is on the menu is
   a separate question from what is on the shelf. Test:
   `venue::tests::renaming_a_shelf_item_renames_what_the_till_offers`.

2. **The price box was filled with the formatted figure.**
   `settings::format_money` produces `"1,200.00 ETB"`; `Money::parse` refuses
   commas and letters. So any price of 1000 or more, or any venue with a
   currency code set, could not be saved back — including a price nobody had
   touched. This was **already live** in the composed-drinks panel before this
   session. `ProductLine` and `SaleItemLine` now carry **`price_value`**
   alongside `price`: same amount, `Money::to_display()`, no grouping and no
   code. `price` is for showing, `price_value` is for editing. The webview
   still computes nothing — Rust formats both.

On the screen: the Items card's row button opens one panel with **Name** and
**Sale price**. It reads "Sell it" until the item has a price and "Edit"
after, because typing the first price is what puts it on the menu
(`sellProduct`) while a later one is a reprice (`setPrice`). The old
`StartSelling` component was deleted into it. The composed-drinks panel gained
**Name** beside the Price it already had, saved by one button.

`formOf(product, name)` in `Catalogue.tsx` rebuilds the whole `ProductForm`
from the line Rust sent, so an edit that changes the name cannot quietly
restate anything else. It omits `salePrice` because `ProductUpdate` ignores it.

### Deliveries, and one way to stock a shelf (13 August)

Driven by the user spotting a real gap: *"its not a one time thing though
everytime it runs low needs to be added right ?"* They were right — every
`stock::post` caller was traced and there was **no way to record a restock at
all**.

Scope was set by the user: *"no supplier at all this is out of scope … just a
batch to keep track of which purchased when at what price and the batch should
be automatic."*

**The conflict and how it was resolved.** `purchases.supplier_id` is
`NOT NULL REFERENCES suppliers(id)`, and `stock_movements` refuses a `PURCHASE`
without a `purchase_id`. Dropping the NOT NULL needs a table rebuild, which
`purchases` cannot have (see §8). The user agreed not to rebuild. So **one
standing `suppliers` row named "Deliveries" is created on first use and never
shown anywhere.** The column stays honest; if suppliers ever become in scope it
is already there.

**No migration was needed.** `invoice_ref` is nullable and NULLs do not collide
in a unique index, so unlimited no-paperwork deliveries are legal. The batch is
the `purchases` row id — nobody types one.

New: `repo/purchases.rs` (`house`, `open`, `add_line`, `reaverage`, `recent`)
and `receiving.rs` (transaction owner + `cmd_receive_delivery`). `InventoryView`
gained a `deliveries` field; the inventory surface in `Floor.tsx` gained a
`Receive` form and a "What came in" history.

Three things worth not breaking:

- **The UI asks only for the total paid**, never a per-unit price.
  `purchase_lines` has a CHECK that the two must agree within one santim;
  `unit_rate` derives the rate with the *same arithmetic as the CHECK*, so a
  line built from an exact total can never be refused for disagreeing with
  itself. Asking for both invites two numbers that drift.
- **`reaverage` runs BEFORE `stock::post`.** The standing average belongs to the
  shelf as it stood; posting first blends the crate into itself. The test
  `the_average_cost_blends_the_old_shelf_with_the_new_crate` fails if reordered.
- **Re-averaging is from the exact total, never the rounded rate**, so rounding
  cannot accumulate into the cost of goods.

Receiving requires **a session, not the owner**: deliveries arrive while the
club is shut, and refusing one because nobody is trading pushes the venue into
entering it wrong later. Who received it is audited (`STOCK_RECEIVED`).
One-line change if the user wants owner-only.

**"Count in" was removed.** `cmd_add_opening_stock` and
`commissioning::record_opening_stock` are gone, along with 95 lines of their
tests. A first delivery against an empty shelf blends to exactly the delivery's
own cost, so receiving did the job identically — and two ways to stock a shelf,
on two screens, is a way to enter one wrong.

### Also on the tree

- The owner's navigation gained **Till** and **End of day** (`App.tsx`). In a
  small venue the owner covers the bar themselves, and an owner who could open
  a night but not close it would strand the till.
- The Inventory surface has an **Items / Value toggle**, built as a `Segmented`
  control after the user rejected a role gate on cost visibility: *"it should
  be a toggle between cash value view vs physical item view."* Frontend-only;
  the mode does not persist across reloads.
- `repeated_wrong_pins_lock_the_keypad` is `#[ignore]`d with the diagnosis in
  the attribute, at the user's request. Cause is real, not a test artefact —
  see §6.

---

## 5. Open — needs the user, do not guess

Nothing is blocking. Two things were offered and not taken:

- **Category is not editable** from either edit panel. The user asked for name
  and price; this was flagged and left. `edit_product` and `edit_sale_item`
  both already accept a category, so it is a field on a form, nothing more.
- **`reporting.show_cost` is wired to nothing.** Only its definition, the
  validation list and the Settings field reference it. Either wire it or delete
  it; a setting that does nothing is worse than either.

---

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
- **`settings::format_money` output is not `Money::parse` input.** Grouping and
  the currency code both break it. Anything an owner can type back needs the
  plain figure — see §4.
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
