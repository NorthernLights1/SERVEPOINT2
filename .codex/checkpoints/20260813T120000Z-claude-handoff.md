# Checkpoint — order corrections finished and wired

Supersedes `20260812T210000Z-claude-handoff.md`, which is now stale in one
respect only: it lists order corrections as unbuilt. They are built.

## State

Nothing is committed. Everything is working-tree state on `b59e797`.

```
cd src-tauri && cargo test          # 267 pass (258 lib + 9 integration)
cd src-tauri && cargo build --release
npm run build                       # tsc --noEmit && vite build
rustfmt --edition 2021 --check src-tauri/src/*.rs src-tauri/src/repo/*.rs
```

`cargo fmt` is still broken in this checkout; call `rustfmt` directly as above.

## What Codex left, and what was done to it

Codex took the order-corrections item and stopped mid-flight without a note.
It had written `correction.rs` (the protocol), `corrections.rs` (the command
surface), `OrderCorrection.tsx` (the screen), the `api.ts` types and methods,
and the `main.rs` handler registrations. All of that is good work and was kept.

Four things were finished on top of it.

**1. The build was broken.** `orders::expanded_products` had been changed to
return a `BTreeMap` for `correction::deltas`, leaving one caller in
`trading.rs::prepare_issue` passing it where a slice was expected. Fixed at the
call site.

**2. `main.rs` did not import `corrections`.** Four handlers were registered
against an unresolved module.

**3. `OrderCorrection.tsx` was unreachable.** Nothing rendered it. It is now in
`Till`, between the ring-up card and `Settle`, keyed on the tab.

**4. A stranded correction was double-billing the tab.** This one mattered.

`correction::prepare` commits the replacement order in `PRINTING` before any
printer I/O, exactly as an ordinary round does. If the slip then fails, that
replacement appears in the Recovery list, which is right — but both answers
there routed to `trading::authorize_handwritten` / `trading::confirm_non_print`,
which know nothing about corrections. Answering "I wrote it by hand" issued the
replacement as a **fresh round** while the original stayed `ISSUED`. The tab was
then billed for both, the stock movements were the full replacement rather than
the delta, and the `pending_order_corrections` row was never cleared — so the
night could not close either.

Proven by a test before fixing it: the tab totalled 150.00 where it should have
totalled 100.00. `recovery.rs` now asks `frozen_correction()` whether the
stranded order is a correction's replacement paper and routes to
`correction::complete` / `correction::abandon`, which already existed and were
already tested. `abandon` had no caller before this; it does now.

## One deliberate change to Codex's design

`correction::complete` and `correction::abandon` refused anyone but the cashier
who began the correction. That guard only ever fired on the recovery path — on
the normal path `prepare` and `complete` take the same session by construction,
so it was always trivially true there.

On the recovery path it was a hard lock. A stranded correction holds `PRINTING`,
`shifts::recovery_complete` blocks on `PRINTING`, and `shifts::close` blocks on
`recovery_complete`. So: cashier starts a correction, printer dies, cashier goes
home, and the venue can never close the night — from any screen, as any role.
That is a durable state with no way forward, which is the thing INV-13 exists to
prevent.

Both guards were removed. The audit events still record whoever actually
answered for the paper. Test:
`another_cashier_can_answer_for_a_correction_that_was_left_stranded`.

If you disagree with this and want the restriction back, it needs a different
exit for the abandoned case first — do not simply re-add the check.

## Also changed

`Till` now bumps a `strandedSince` counter on `PRINT_PENDING` from either
`place_order` or a correction, which remounts `Recovery` so a newly stranded
print appears without a reload. It previously fetched once on mount and never
again, so a print failure mid-session left the card invisible until the screen
was rebuilt.

Codex's new files were not rustfmt-clean. They are now; the whole tree passes.

## Known issues, noted not fixed

**`repeated_wrong_pins_lock_the_keypad` flakes under load.** It passes alone and
fails when the whole suite runs. The cause is real rather than a test artefact:
`FREE_ATTEMPTS` is 3 and `FIRST_LOCKOUT_MS` is 5 seconds, but hashing one PIN
costs roughly that long on modest hardware — that single test takes ~27s. So the
first lockout can expire *during the next attempt's own hashing*, and a correct
PIN gets through when it should be refused. On a slow till the first lockout
therefore buys almost nothing. Deliberately left alone: it is auth code, found
during unrelated catalogue work. Decide whether to raise `FIRST_LOCKOUT_MS`,
start the lock when the attempt begins rather than when the hash finishes, or
accept it.

**`base_units_per_pack` ("Units per pack") is inert.** It is stored, audited and
displayed, but no code multiplies by it — `grep` finds only struct fields and
audit strings. Recipe quantities and stock movements are both in a product's
base units, with no pack conversion anywhere. Same status as
`units_per_purchase_pack`, which `0002_catalogue.sql` already admits is
"invisible in the UI today". The field currently misleads: an owner setting
"24 shots per bottle" will reasonably expect the shelf to convert, and it does
not.

## Still open, unchanged from the previous checkpoint

- **Reports.** Still blocked on the same question: freeze a `shift_reports` row
  at close, or recalculate from the ledgers. Read section 2 of the 21:00
  checkpoint. Still do not guess.
- Purchases and deliveries (`0006_purchasing.sql` exists, no repo module).
- Stock counts (`DraftStockCount` has a resolution entry and no screen).
- `backup.rs` has no command surface.
- A re-audit of test-only `pub` functions.

## Verified about the IPC surface

Every `cmd_*` called from `api.ts` is registered in `main.rs`, and every
registered command is called from `api.ts`. 36 commands, nothing orphaned in
either direction.
