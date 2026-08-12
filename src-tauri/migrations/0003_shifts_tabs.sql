-- 0003_shifts_tabs.sql — shifts, tabs, tab transfers
--
-- Two long-lived containers, with opposite lifetimes:
--
--   SHIFT  one business night. Exactly one may be OPEN, ever. Closing it is
--          the fraud-control event that produces the stored report.
--   TAB    one party's running bill. Outlives the shift — a tab opened on
--          Friday and never settled is still open on Sunday, deliberately.
--
-- The asymmetry is the point (§4.5): open tabs and unsettled waiters do NOT
-- block the shift close. Both legitimately carry over. They are shown for
-- acknowledgement so nothing is carried forward silently, and that is all.

-- ---------------------------------------------------------------------------
-- Shifts (§4)
-- ---------------------------------------------------------------------------

CREATE TABLE shifts (
    id                 INTEGER PRIMARY KEY,
    code               TEXT    NOT NULL UNIQUE,          -- SHIFT-000001 (§1.4)

    -- The night this shift trades for, derived once at open from the business
    -- calendar (§1.3) and NEVER recomputed. A sale at 02:00 belongs to the
    -- previous evening's date; recomputing later from a timestamp would move
    -- it, and every report built on it would silently disagree with the paper.
    business_date      TEXT    NOT NULL UNIQUE
                               CHECK (business_date GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]'),

    status             TEXT    NOT NULL DEFAULT 'OPEN'
                               CHECK (status IN ('OPEN','CLOSING','CLOSED')),

    opened_at          INTEGER NOT NULL,
    opened_by          INTEGER NOT NULL REFERENCES staff(id),

    -- §4.1: the float is ALSO written as the first cash_movement. It is stored
    -- here for the report header only. Expected cash is a sum over ONE ledger
    -- (§7.6) — if this column were the authority, there would be two numbers
    -- to reconcile instead of one.
    opening_float_minor INTEGER NOT NULL DEFAULT 0 CHECK (opening_float_minor >= 0),

    -- §4.6: snapshotted at open from shift.day_end rather than recomputed, so
    -- changing the setting tonight cannot retroactively make last week's
    -- shifts look overdue.
    expected_end_at    INTEGER NOT NULL,

    closed_at          INTEGER,
    closed_by          INTEGER REFERENCES staff(id),
    counted_cash_minor INTEGER CHECK (counted_cash_minor IS NULL OR counted_cash_minor >= 0),

    CHECK (closed_at IS NULL OR closed_at >= opened_at)
);

-- §11.3 / INV-9. A partial unique index, not application code: two OPEN shifts
-- would split one night's takings across two reports, and neither would
-- balance.
CREATE UNIQUE INDEX shifts_one_open ON shifts(status) WHERE status = 'OPEN';

CREATE INDEX shifts_by_date ON shifts(business_date);

-- OPEN -> CLOSING -> CLOSED, one direction, no skipping (§4.2).
--
-- CLOSING exists so the cashier has a state to count the drawer in without the
-- till still taking sales behind them. Allowing OPEN -> CLOSED directly would
-- let a close race a round being rung up.
CREATE TRIGGER shifts_status_machine BEFORE UPDATE OF status ON shifts
WHEN NOT (   OLD.status = NEW.status
          OR (OLD.status = 'OPEN'    AND NEW.status = 'CLOSING')
          OR (OLD.status = 'CLOSING' AND NEW.status = 'CLOSED'))
BEGIN SELECT RAISE(ABORT, 'shifts: only OPEN -> CLOSING -> CLOSED is permitted'); END;

-- A closed night is finished. Its report is already stored and may already be
-- on paper; changing the counted cash afterwards would make the two disagree
-- with no trace.
CREATE TRIGGER shifts_closed_frozen BEFORE UPDATE ON shifts
WHEN OLD.status = 'CLOSED'
BEGIN SELECT RAISE(ABORT, 'shifts: a closed shift is frozen'); END;

-- Identity and opening facts are immutable for the same reason the business
-- date is never recomputed.
CREATE TRIGGER shifts_identity_frozen BEFORE UPDATE ON shifts
WHEN NEW.code                <> OLD.code
  OR NEW.business_date       <> OLD.business_date
  OR NEW.opened_at           <> OLD.opened_at
  OR NEW.opened_by           <> OLD.opened_by
  OR NEW.opening_float_minor <> OLD.opening_float_minor
BEGIN SELECT RAISE(ABORT, 'shifts: opening facts are immutable'); END;

-- A close with no closing facts is a half-written record that every report
-- would then have to defend against.
CREATE TRIGGER shifts_close_needs_facts BEFORE UPDATE OF status ON shifts
WHEN NEW.status = 'CLOSED'
 AND (NEW.closed_at IS NULL OR NEW.closed_by IS NULL OR NEW.counted_cash_minor IS NULL)
BEGIN SELECT RAISE(ABORT, 'shifts: closing requires closed_at, closed_by and counted cash'); END;

-- Only a role that can log in may run a till session (D22). A waiter has no
-- PIN and therefore cannot be the person who opened or closed the night.
CREATE TRIGGER shifts_operator_can_login BEFORE INSERT ON shifts
WHEN (SELECT role FROM staff WHERE id = NEW.opened_by) NOT IN ('OWNER','CASHIER')
BEGIN SELECT RAISE(ABORT, 'shifts: only an owner or cashier can open a shift'); END;

CREATE TRIGGER shifts_closer_can_login BEFORE UPDATE OF closed_by ON shifts
WHEN NEW.closed_by IS NOT NULL
 AND (SELECT role FROM staff WHERE id = NEW.closed_by) NOT IN ('OWNER','CASHIER')
BEGIN SELECT RAISE(ABORT, 'shifts: only an owner or cashier can close a shift'); END;

-- A shift is always born OPEN and reaches every other state through the machine
-- above. Without this, an INSERT could conjure a CLOSED night that no close
-- ever ran for — no counted cash, no report, and nothing to notice it.
CREATE TRIGGER shifts_born_open BEFORE INSERT ON shifts
WHEN NEW.status <> 'OPEN'
BEGIN SELECT RAISE(ABORT, 'shifts: a shift is always created OPEN'); END;

CREATE TRIGGER shifts_no_delete BEFORE DELETE ON shifts
BEGIN SELECT RAISE(ABORT, 'shifts are closed, never deleted'); END;

-- ---------------------------------------------------------------------------
-- Tabs (§5)
--
-- Identity is TWO things: an immutable internal code nobody says out loud, and
-- a human reference the cashier actually searches by.
--
-- The four reference fields are stored SEPARATELY and the mode is stored ON
-- THE TAB (§5.1). A single polymorphic "reference" column would be
-- reinterpreted the moment the setting changed, and last month's tabs would
-- silently become table numbers that were never table numbers.
-- ---------------------------------------------------------------------------

CREATE TABLE tabs (
    id              INTEGER PRIMARY KEY,
    code            TEXT    NOT NULL UNIQUE,             -- TAB-000421 (§1.4)

    opened_shift_id INTEGER NOT NULL REFERENCES shifts(id),
    waiter_id       INTEGER NOT NULL REFERENCES staff(id),

    -- The mode in force WHEN THIS TAB WAS OPENED, not today's setting.
    reference_mode  TEXT    NOT NULL
                            CHECK (reference_mode IN ('TABLE','CUSTOMER_NAME','CUSTOMER_PHONE','CUSTOM')),

    table_no        TEXT,
    customer_name   TEXT,
    customer_phone  TEXT,
    custom_ref      TEXT,

    -- Computed at open from the mode above and frozen. Searched, printed, and
    -- unique among OPEN tabs.
    display_label   TEXT    NOT NULL CHECK (TRIM(display_label) <> ''),

    -- D25. Optional, and only ever asked for when tabs.ask_customer_tin is on.
    -- A customer reclaiming VAT needs their own TIN on the receipt; a customer
    -- buying a beer does not.
    customer_tin    TEXT,

    status          TEXT    NOT NULL DEFAULT 'OPEN'
                            CHECK (status IN ('OPEN','CLOSED','RECONCILED')),

    opened_at       INTEGER NOT NULL,
    opened_by       INTEGER NOT NULL REFERENCES staff(id),

    -- The shift the bill was FROZEN in, which is often not the shift it was
    -- opened in (§5.2: tabs cross nights). Revenue is reported on the issue,
    -- not on the close, so these two shift ids answer different questions.
    closed_shift_id INTEGER REFERENCES shifts(id),
    closed_at       INTEGER,
    closed_by       INTEGER REFERENCES staff(id),

    -- A comped tab is closed with zero liability (§7.8). The reason lives on
    -- the immutable tab_payments row, not here, because that is the row the
    -- report reads.
    is_comped       INTEGER NOT NULL DEFAULT 0 CHECK (is_comped IN (0,1)),

    -- The mode must actually be answerable. A CUSTOM tab with no custom_ref
    -- has a label that can never be rebuilt or verified.
    CHECK (CASE reference_mode
             WHEN 'TABLE'           THEN TRIM(COALESCE(table_no, ''))      <> ''
             WHEN 'CUSTOMER_NAME'   THEN TRIM(COALESCE(customer_name, '')) <> ''
             WHEN 'CUSTOMER_PHONE'  THEN TRIM(COALESCE(customer_name, '')) <> ''
             WHEN 'CUSTOM'          THEN TRIM(COALESCE(custom_ref, ''))    <> ''
           END),

    CHECK (closed_at IS NULL OR closed_at >= opened_at)
);

-- §5.1: unique among OPEN tabs only. Two open "Table 7"s make the cashier's
-- search ambiguous at exactly the wrong moment — mid-round, with a queue.
-- Reuse after close is not merely allowed but expected: one table serves many
-- parties in a night.
CREATE UNIQUE INDEX tabs_open_label ON tabs(display_label) WHERE status = 'OPEN';

CREATE INDEX tabs_by_status_waiter ON tabs(status, waiter_id);
CREATE INDEX tabs_by_waiter        ON tabs(waiter_id, status);
CREATE INDEX tabs_by_opened        ON tabs(opened_at);
CREATE INDEX tabs_search_name      ON tabs(customer_name);
CREATE INDEX tabs_search_phone     ON tabs(customer_phone);
CREATE INDEX tabs_search_table     ON tabs(table_no);

-- §5.2, §11.2. THE tab rule: a closed tab is never reopened.
--
-- Everything else is permitted, including CLOSED -> RECONCILED and the
-- unwinding of a reconciliation. Reopening is singled out because the close
-- froze a bill and printed a fiscal receipt; reopening would mean voiding that
-- document and reissuing, and then two receipts exist for one visit with
-- nothing saying which is authoritative. The customer orders again -> new tab.
CREATE TRIGGER tabs_never_reopen BEFORE UPDATE OF status ON tabs
WHEN OLD.status IN ('CLOSED','RECONCILED') AND NEW.status = 'OPEN'
BEGIN SELECT RAISE(ABORT, 'tabs: a closed tab is never reopened — open a new one'); END;

CREATE TRIGGER tabs_close_needs_facts BEFORE UPDATE OF status ON tabs
WHEN NEW.status IN ('CLOSED','RECONCILED')
 AND (NEW.closed_at IS NULL OR NEW.closed_by IS NULL OR NEW.closed_shift_id IS NULL)
BEGIN SELECT RAISE(ABORT, 'tabs: closing requires closed_at, closed_by and the closing shift'); END;

-- Identity is frozen from the moment the tab opens, not from the moment it
-- closes. The label is on the running bill in front of the customer.
CREATE TRIGGER tabs_identity_frozen BEFORE UPDATE ON tabs
WHEN NEW.code            <> OLD.code
  OR NEW.opened_shift_id <> OLD.opened_shift_id
  OR NEW.reference_mode  <> OLD.reference_mode
  OR NEW.display_label   <> OLD.display_label
  OR NEW.opened_at       <> OLD.opened_at
BEGIN SELECT RAISE(ABORT, 'tabs: identity and reference are frozen at open'); END;

-- Once the bill is frozen the money facts are frozen with it. Only status and
-- the closing facts may still move (CLOSED -> RECONCILED).
CREATE TRIGGER tabs_closed_waiter_frozen BEFORE UPDATE ON tabs
WHEN OLD.status IN ('CLOSED','RECONCILED')
 AND (NEW.waiter_id <> OLD.waiter_id OR NEW.is_comped <> OLD.is_comped)
BEGIN SELECT RAISE(ABORT, 'tabs: a closed tab''s waiter and comp status are frozen'); END;

-- Every tab belongs to exactly one waiter, and a waiter is a master record,
-- not a user (§0.3). Pointing this at a cashier would put a liability on
-- somebody who never carried the drinks.
CREATE TRIGGER tabs_waiter_is_waiter_ins BEFORE INSERT ON tabs
WHEN (SELECT role FROM staff WHERE id = NEW.waiter_id) <> 'WAITER'
BEGIN SELECT RAISE(ABORT, 'tabs: a tab belongs to a waiter'); END;

CREATE TRIGGER tabs_waiter_is_waiter_upd BEFORE UPDATE OF waiter_id ON tabs
WHEN (SELECT role FROM staff WHERE id = NEW.waiter_id) <> 'WAITER'
BEGIN SELECT RAISE(ABORT, 'tabs: a tab belongs to a waiter'); END;

-- A tab may only be opened while a shift is trading. Without this, a tab could
-- be attached to last week's closed night and appear on a report that has
-- already been printed and signed.
CREATE TRIGGER tabs_need_open_shift BEFORE INSERT ON tabs
WHEN (SELECT status FROM shifts WHERE id = NEW.opened_shift_id) <> 'OPEN'
BEGIN SELECT RAISE(ABORT, 'tabs: a tab can only be opened in an open shift'); END;

-- Born OPEN, for the same reason as shifts: a tab inserted straight into
-- CLOSED would carry a frozen bill that no close ever calculated, and no
-- tab_payments row would exist behind it.
CREATE TRIGGER tabs_born_open BEFORE INSERT ON tabs
WHEN NEW.status <> 'OPEN'
BEGIN SELECT RAISE(ABORT, 'tabs: a tab is always created OPEN'); END;

CREATE TRIGGER tabs_no_delete BEFORE DELETE ON tabs
BEGIN SELECT RAISE(ABORT, 'tabs are closed, never deleted'); END;

-- ---------------------------------------------------------------------------
-- Tab transfers (§5.4) — append-only
--
-- A waiter leaves mid-shift and their open tabs move to somebody else.
--
-- WHAT MOVES IS RESPONSIBILITY FOR COLLECTING, NOTHING ELSE. Orders already
-- issued keep their original waiter, because that is what actually happened.
-- So "orders issued by A" and "what A must settle tonight" are deliberately
-- different figures, and the shift report shows both without either being
-- wrong.
-- ---------------------------------------------------------------------------

CREATE TABLE tab_transfers (
    id             INTEGER PRIMARY KEY,
    tab_id         INTEGER NOT NULL REFERENCES tabs(id),
    from_waiter_id INTEGER NOT NULL REFERENCES staff(id),
    to_waiter_id   INTEGER NOT NULL REFERENCES staff(id),
    shift_id       INTEGER NOT NULL REFERENCES shifts(id),
    transferred_at INTEGER NOT NULL,
    transferred_by INTEGER NOT NULL REFERENCES staff(id),
    reason         TEXT    NOT NULL DEFAULT '',

    -- A transfer to the same waiter is a no-op that would still print on the
    -- report as an event, inviting somebody to explain a movement that never
    -- happened.
    CHECK (from_waiter_id <> to_waiter_id)
);

CREATE INDEX tab_transfers_by_tab   ON tab_transfers(tab_id, transferred_at);
CREATE INDEX tab_transfers_by_shift ON tab_transfers(shift_id);

-- §5.4: the tab must still accept orders. Transferring a closed tab would move
-- a liability that tab_payments has already frozen against the original
-- waiter, and the held balances of two people would disagree with the ledger.
CREATE TRIGGER tab_transfers_tab_open BEFORE INSERT ON tab_transfers
WHEN (SELECT status FROM tabs WHERE id = NEW.tab_id) <> 'OPEN'
BEGIN SELECT RAISE(ABORT, 'tab transfers: only an open tab can be transferred'); END;

-- The row must agree with the tab it claims to move.
CREATE TRIGGER tab_transfers_from_matches BEFORE INSERT ON tab_transfers
WHEN (SELECT waiter_id FROM tabs WHERE id = NEW.tab_id) <> NEW.from_waiter_id
BEGIN SELECT RAISE(ABORT, 'tab transfers: from_waiter must be the tab''s current waiter'); END;

CREATE TRIGGER tab_transfers_to_is_waiter BEFORE INSERT ON tab_transfers
WHEN (SELECT role FROM staff WHERE id = NEW.to_waiter_id) <> 'WAITER'
BEGIN SELECT RAISE(ABORT, 'tab transfers: tabs transfer to a waiter'); END;

-- Append-only (§11.1). The transfer log is the answer to "why is this tab on
-- Sara when Dawit issued every round on it".
CREATE TRIGGER tab_transfers_no_update BEFORE UPDATE ON tab_transfers
BEGIN SELECT RAISE(ABORT, 'tab_transfers is append-only'); END;

CREATE TRIGGER tab_transfers_no_delete BEFORE DELETE ON tab_transfers
BEGIN SELECT RAISE(ABORT, 'tab_transfers is append-only'); END;
