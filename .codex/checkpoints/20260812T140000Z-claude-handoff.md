# ServePoint2 handoff — end of Claude session, 2026-08-12

## State: green

- `cargo test` — 241 passed, 0 failed
- `rustfmt --check` — clean (run `rustfmt` directly; `cargo fmt` fails, no rustup default toolchain)
- `tsc --noEmit`, `vite build`, `cargo build --release` — all clean
- Backend for trading is complete. **Nothing is wired to the UI.**

## What happened this session

1. Codex built the trading core between sessions (12:09–12:11): `trading.rs`,
   `settlement.rs`, `printing.rs`, `commissioning.rs`, `backup.rs`,
   `resolution.rs`, repo `{cash,orders,receipts,shifts,tabs}.rs`, migrations
   0010/0011.
2. Ponytail audit applied at the user's request. **20,851 → 18,342 lines.**
   Deleted: `tools/schema-check.mjs` (1906, superseded by `db.rs`),
   `tools/make-icons.mjs` (152, use `npx tauri icon`; icons already committed),
   the `settlement.rs` customer-print trio + its two private helpers (236),
   six dead fns, `<Stat>` in `ui.tsx`, the `serde_json` dep, and two
   `trading.rs` tests calling `finalize_issue`/`ReceiptResolution` — symbols
   that never existed, so `cargo test` was already broken before the session.
3. Withdrawn findings, deliberately: the `guarded!` macro (52 call sites, not
   worth inlining) and collapsing the 11 migration files (zero line savings,
   real risk).

## ~~THE OPEN DEFECT~~ — FIXED 2026-08-12, option 1 taken

`IssueDocument` is gone. `prepare_issue(conn, order_id, cashier_id, at)` derives
the destination split from product destinations (§6.7) and calls
`printing::render_issue` *after* `receipts::create_issue` returns the number.
Callers pass no text, so a slip without its BR number is unrepresentable.
`cargo test` 241 passed, `rustfmt --check` clean. Next: `floor.rs`, below.

Original report, kept for context:

**Bar issue slips never carry their BR number.**

`trading.rs::prepare_issue` accepts pre-rendered text from the caller and
freezes it (line ~121), *then* allocates the receipt number (line ~120 inside
the same loop, but after the text is fixed). So the number can never reach the
paper.

- Customer receipts do this correctly — `printing.rs:149` stamps
  `CUSTOMER RECEIPT {receipt_number}`.
- `printing.rs::render_issue(receipt_number, destination, tab_code,
  tab_reference, waiter_name, lines)` was clearly written for this and has
  exactly ONE caller in the tree: its own unit test at `printing.rs:424`.

Why it blocks the till: `0005_receipts.sql` trigger
`order_corrections_typed_number` requires the cashier to type the BR number
off the returned slip before a correction can be recorded. The slip has no
number on it, so the correction path cannot be exercised from the counter.
This is the same shape as the defect that got the previous build rejected —
a control that exists in the data model and is unreachable from the interface.

**Proposed fix:** stop passing rendered text into `prepare_issue`. It already
has `order_id`, so it can reach the tab, waiter and lines itself. Have it call
`printing::render_issue` per destination *after* `receipts::create_issue`
returns the number. `IssueDocument` then carries facts (destination), not
text. Makes the omission structurally impossible and gives `render_issue` its
real caller. Touches a module covered by 232 tests.

## Pending decision — RESOLVED, option 1

Fixed the BR number first. The till now gets a correct `prepare_issue`
signature to call once.

## Next work after that — floor.rs DONE 2026-08-12

Written and registered in `main.rs`. `floor_view`, `inventory_view`,
`open_shift`, `open_tab`, `place_order`, plus `resolve_handwritten` /
`resolve_non_print` so a dead printer cannot wedge the till. `cargo test` 246
passed, `rustfmt --check` and `cargo build --release` clean.

Also landed: `Money::parse` (strict, for typed cash) and a `FilePrinter` fix —
its counter restarted at 1 each construction, so the second order of a night
collided with an existing spool file.

**Still unwired:** `src/screens/Floor.tsx` is untouched — the backend surface
exists, no UI calls it yet. That is the next job, along with Phase 4/5 below.

Original design notes, all now implemented:

- `floor_view` — shift, open tabs + running totals, waiters, menu, in one call
- `open_shift(float)`, `open_tab(waiter, reference)`, `place_order(tab, lines)`
- `inventory_view` — `stock::levels` is ready
- Money crosses the boundary pre-formatted as `String` (D3). Reuse
  `commands::CommandError`; its constructors are private `fn`, so they need
  `pub(crate)`.
- Print target: derive from `SELECT file FROM pragma_database_list WHERE
  name='main'`, parent dir + `slips/`, `printing::FilePrinter`. No new setting,
  no new AppState field.

Then Phase 4 (reconciliation page) and Phase 5 (reports + overview). The
screens in `src/screens/Floor.tsx` are honest empty states today.

## Standing constraints

- **Do not read** `/home/temeg242/Documents/Project/ServePoint/ai-context/`
  (`04-decisions.md`, `06-shift-report-design.md`, `07-specification-v2.md`,
  `08-data-model.md`). Withheld deliberately — "the reason i didn't want it
  because it was a bad implementation."
- Nothing hardcoded for one venue. This ships to many bars and clubs.
- Reports to the user: short, plain language, no build narration.
- Ponytail was ON at `full` this session by user choice. It does not survive a
  session boundary — re-invoke `/ponytail full` to continue under it.
- 38 pub fns are reachable only from their own tests. Most is unwired Phase 3,
  not dead code. Re-audit after the till is connected.
