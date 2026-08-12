-- 0008_money.sql — frozen bills, waiter settlements, and the drawer
--
-- §7.6 IS THE SINGLE MOST IMPORTANT LINE IN THE SPECIFICATION:
--
--   expected_cash(shift) = SUM(cash_movements.amount_minor WHERE shift_id = ?)
--
--     opening float
--   + waiter reconciliations received — CASH PORTION ONLY
--   - cash paid out, itemised by category
--   = what should be in the drawer
--
-- EXPECTED CASH COMES FROM RECONCILIATIONS, NEVER FROM TABS CLOSED.
--
-- Computing it from sales is the obvious implementation and it is what the
-- prior prototype did. It makes the drawer appear short by the value of every
-- unreconciled tab, every night, INDISTINGUISHABLY FROM THEFT. The cashier
-- then learns to ignore the variance, and the one control that would catch a
-- real theft is dead.
--
-- The structural consequence, stated in §7.6: THERE MUST BE NO CODE PATH FROM
-- `tab_payments` INTO `cash_movements`. Note below that cash_movements has no
-- tab_id and no tab_payment_id, and never gains one. INV-7 asserts it.

-- ---------------------------------------------------------------------------
-- Tab payments (§7.3) — the frozen bill, append-only
--
-- Written once when a tab closes. This row, not a recalculation, is what the
-- customer receipt prints and what the waiter owes.
-- ---------------------------------------------------------------------------

CREATE TABLE tab_payments (
    id                   INTEGER PRIMARY KEY,
    tab_id               INTEGER NOT NULL UNIQUE REFERENCES tabs(id),

    -- Frozen at close: the waiter who owned the tab THEN. A later transfer
    -- cannot move a liability that has already been counted against somebody.
    waiter_id            INTEGER NOT NULL REFERENCES staff(id),

    subtotal_minor       INTEGER NOT NULL CHECK (subtotal_minor       >= 0),
    service_charge_minor INTEGER NOT NULL CHECK (service_charge_minor >= 0),
    tax_minor            INTEGER NOT NULL CHECK (tax_minor            >= 0),
    total_minor          INTEGER NOT NULL CHECK (total_minor          >= 0),

    -- What the waiter must hand over. Zero for a comp, and only for a comp.
    liability_minor      INTEGER NOT NULL CHECK (liability_minor >= 0),

    is_comped            INTEGER NOT NULL DEFAULT 0 CHECK (is_comped IN (0,1)),
    comp_reason          TEXT,

    -- The rates AS APPLIED, so an old bill can always explain its own
    -- arithmetic after the settings change.
    tax_rate_bp          INTEGER NOT NULL CHECK (tax_rate_bp     >= 0),
    service_rate_bp      INTEGER NOT NULL CHECK (service_rate_bp >= 0),
    tax_inclusive        INTEGER NOT NULL CHECK (tax_inclusive IN (0,1)),

    -- §11.3: a row whose rates are unknown cannot produce a fiscal receipt if
    -- it carries charges — it stops for manual review rather than printing a
    -- false 0% document. Always 1 for anything this build writes; the column
    -- exists so imported history cannot masquerade as tax-exempt.
    charge_rates_known   INTEGER NOT NULL DEFAULT 1 CHECK (charge_rates_known IN (0,1)),

    shift_id             INTEGER NOT NULL REFERENCES shifts(id),
    created_by           INTEGER NOT NULL REFERENCES staff(id),
    created_at           INTEGER NOT NULL,

    -- §7.8: A COMPED TAB CARRIES ZERO LIABILITY. Otherwise the waiter is on
    -- the hook for the house's giveaway until they get a chance to declare it.
    CHECK (CASE WHEN is_comped = 1
                THEN liability_minor = 0 AND TRIM(COALESCE(comp_reason, '')) <> ''
                ELSE liability_minor = total_minor AND comp_reason IS NULL
           END)

    -- DELIBERATELY NO payment_method COLUMN, and it must never gain one
    -- (§7.3). The method is not known at close, and because reconciliation is
    -- batched across tabs it attaches to the SETTLEMENT, not the tab. A
    -- per-tab method field could never be filled reliably, and a column that
    -- is usually wrong is worse than no column.
);

CREATE INDEX tab_payments_by_waiter ON tab_payments(waiter_id);
CREATE INDEX tab_payments_by_shift  ON tab_payments(shift_id);

CREATE TRIGGER tab_payments_tab_closed BEFORE INSERT ON tab_payments
WHEN (SELECT status FROM tabs WHERE id = NEW.tab_id) NOT IN ('CLOSED','RECONCILED')
BEGIN SELECT RAISE(ABORT, 'tab payments: the bill is frozen when the tab closes'); END;

CREATE TRIGGER tab_payments_no_update BEFORE UPDATE ON tab_payments
BEGIN SELECT RAISE(ABORT, 'tab_payments is append-only'); END;

CREATE TRIGGER tab_payments_no_delete BEFORE DELETE ON tab_payments
BEGIN SELECT RAISE(ABORT, 'tab_payments is append-only'); END;

-- ---------------------------------------------------------------------------
-- Reconciliations (§7.5) — append-only but for one seal
--
-- §7.4: HELD BALANCE IS DERIVED, NEVER STORED.
--
--   held(waiter) = SUM(tab_payments.liability)
--                - SUM(cash + non_cash + written_off) over FINALIZED rows
--
-- A stored balance is a cache that drifts, for the same reason stock on hand
-- is never stored. Partial settlement then works for free: the waiter hands
-- over what they have and the running balance carries the rest.
-- ---------------------------------------------------------------------------

CREATE TABLE reconciliations (
    id                INTEGER PRIMARY KEY,
    waiter_id         INTEGER NOT NULL REFERENCES staff(id),
    cashier_id        INTEGER NOT NULL REFERENCES staff(id),

    expected_minor    INTEGER NOT NULL CHECK (expected_minor    >= 0),
    cash_minor        INTEGER NOT NULL DEFAULT 0 CHECK (cash_minor        >= 0),
    non_cash_minor    INTEGER NOT NULL DEFAULT 0 CHECK (non_cash_minor    >= 0),
    written_off_minor INTEGER NOT NULL DEFAULT 0 CHECK (written_off_minor >= 0),
    shortfall_minor   INTEGER NOT NULL CHECK (shortfall_minor   >= 0),
    write_off_reason  TEXT,

    -- The shift the CASH REACHED THE DRAWER, which may be a different night
    -- from the one the tab closed in (§7.5).
    shift_id          INTEGER NOT NULL REFERENCES shifts(id),
    created_at        INTEGER NOT NULL,

    -- The one permitted mutation on this table: NULL -> timestamp, once. It
    -- seals the row, and nothing may be appended afterwards.
    finalized_at      INTEGER,

    -- §7.5: SPLIT TENDER IS NOT SUPPORTED — one method per settlement. Two
    -- methods on one row would need a per-method breakdown that nothing in the
    -- reporting model asks for.
    CHECK ((CASE WHEN cash_minor        > 0 THEN 1 ELSE 0 END)
         + (CASE WHEN non_cash_minor    > 0 THEN 1 ELSE 0 END)
         + (CASE WHEN written_off_minor > 0 THEN 1 ELSE 0 END) <= 1),

    -- A write-off is somebody deciding money will never arrive. It says why.
    CHECK (written_off_minor = 0 OR TRIM(COALESCE(write_off_reason, '')) <> ''),

    -- §7.5: OVERAGES ARE NEVER BOOKED AS INCOME. More cash than owed is
    -- almost certainly the waiter's own tip money or a counting error, so
    -- there is no overage column and settling more than expected is refused
    -- outright — the cashier returns the difference.
    CHECK (cash_minor + non_cash_minor + written_off_minor <= expected_minor),
    CHECK (shortfall_minor
             = expected_minor - cash_minor - non_cash_minor - written_off_minor)
);

CREATE INDEX reconciliations_by_waiter ON reconciliations(waiter_id, finalized_at);
CREATE INDEX reconciliations_by_shift  ON reconciliations(shift_id);

-- Settlement happens at the till, during a trading night.
CREATE TRIGGER reconciliations_shift_open BEFORE INSERT ON reconciliations
WHEN (SELECT status FROM shifts WHERE id = NEW.shift_id) <> 'OPEN'
  OR NEW.finalized_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'reconciliations: settle during an open shift, and seal afterwards'); END;

CREATE TRIGGER reconciliations_seal_once BEFORE UPDATE ON reconciliations
WHEN OLD.finalized_at IS NOT NULL
  OR NEW.waiter_id         <> OLD.waiter_id
  OR NEW.cashier_id        <> OLD.cashier_id
  OR NEW.expected_minor    <> OLD.expected_minor
  OR NEW.cash_minor        <> OLD.cash_minor
  OR NEW.non_cash_minor    <> OLD.non_cash_minor
  OR NEW.written_off_minor <> OLD.written_off_minor
  OR NEW.shortfall_minor   <> OLD.shortfall_minor
  OR NEW.shift_id          <> OLD.shift_id
BEGIN SELECT RAISE(ABORT, 'reconciliations: finalizing is the only permitted change, and it happens once'); END;

CREATE TRIGGER reconciliations_no_delete BEFORE DELETE ON reconciliations
BEGIN SELECT RAISE(ABORT, 'reconciliations is append-only'); END;

-- ---------------------------------------------------------------------------
-- Which tabs a settlement covered (§7.5) — append-only
-- ---------------------------------------------------------------------------

CREATE TABLE reconciliation_tabs (
    id                INTEGER PRIMARY KEY,
    reconciliation_id INTEGER NOT NULL REFERENCES reconciliations(id),

    -- §11.3: A TAB MAY APPEAR IN ONLY ONE RECONCILIATION, EVER. Twice and its
    -- liability is settled twice, so the waiter's balance goes negative and
    -- the shortfall report stops meaning anything.
    tab_id            INTEGER NOT NULL UNIQUE REFERENCES tabs(id),

    amount_minor      INTEGER NOT NULL CHECK (amount_minor >= 0)
);

CREATE INDEX reconciliation_tabs_by_recon ON reconciliation_tabs(reconciliation_id);

-- §11.3: the allocation must repeat that tab's EXACT immutable liability, and
-- the tab must belong to the waiter being settled. Anything else lets a
-- settlement quietly discount a bill.
CREATE TRIGGER reconciliation_tabs_exact BEFORE INSERT ON reconciliation_tabs
WHEN NOT EXISTS (SELECT 1 FROM tab_payments p
                  WHERE p.tab_id = NEW.tab_id
                    AND p.liability_minor = NEW.amount_minor
                    AND p.waiter_id = (SELECT waiter_id FROM reconciliations
                                        WHERE id = NEW.reconciliation_id))
BEGIN SELECT RAISE(ABORT, 'reconciliation: the allocation must equal this waiter''s frozen liability for that tab'); END;

-- §11.3: nothing may be appended to a finalized reconciliation.
CREATE TRIGGER reconciliation_tabs_not_sealed BEFORE INSERT ON reconciliation_tabs
WHEN (SELECT finalized_at FROM reconciliations WHERE id = NEW.reconciliation_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'reconciliation: nothing may be added once it is sealed'); END;

CREATE TRIGGER reconciliation_tabs_no_update BEFORE UPDATE ON reconciliation_tabs
BEGIN SELECT RAISE(ABORT, 'reconciliation_tabs is append-only'); END;

CREATE TRIGGER reconciliation_tabs_no_delete BEFORE DELETE ON reconciliation_tabs
BEGIN SELECT RAISE(ABORT, 'reconciliation_tabs is append-only'); END;

-- ---------------------------------------------------------------------------
-- The drawer (§7.7) — append-only
--
-- NOTE WHAT IS NOT HERE: no tab_id, no tab_payment_id, no order_id. There is
-- deliberately no way to write a cash movement from a tab closing. Only the
-- float, the cash portion of a settlement, payouts and explicit adjustments
-- reach the drawer. That absence IS the enforcement §7.6 asks for.
-- ---------------------------------------------------------------------------

CREATE TABLE cash_movements (
    id                INTEGER PRIMARY KEY,
    shift_id          INTEGER NOT NULL REFERENCES shifts(id),

    movement_type     TEXT    NOT NULL
                              CHECK (movement_type IN ('OPENING_FLOAT','RECONCILIATION',
                                                       'PAYOUT','ADJUSTMENT')),

    amount_minor      INTEGER NOT NULL CHECK (amount_minor <> 0),

    -- Mandatory on a payout. An uncategorised payout is a hole in the control:
    -- money left the drawer and nothing says what for.
    category          TEXT,
    reason            TEXT    NOT NULL DEFAULT '',

    reconciliation_id INTEGER REFERENCES reconciliations(id),
    created_by        INTEGER NOT NULL REFERENCES staff(id),
    created_at        INTEGER NOT NULL,

    CHECK (CASE movement_type
             WHEN 'OPENING_FLOAT'  THEN amount_minor > 0 AND reconciliation_id IS NULL
                                        AND category IS NULL
             -- The CASH PORTION ONLY. A mobile-money settlement clears the
             -- waiter's liability without adding to expected cash, which is
             -- why the reconciliation row and this one are separate facts.
             WHEN 'RECONCILIATION' THEN amount_minor > 0 AND reconciliation_id IS NOT NULL
                                        AND category IS NULL
             WHEN 'PAYOUT'         THEN amount_minor < 0 AND TRIM(COALESCE(category, '')) <> ''
                                        AND reconciliation_id IS NULL
             WHEN 'ADJUSTMENT'     THEN TRIM(reason) <> '' AND reconciliation_id IS NULL
                                        AND category IS NULL
           END)
);

CREATE INDEX cash_movements_by_shift ON cash_movements(shift_id, movement_type);

-- One float per night, and it is the first cash movement rather than a
-- separate concept (§4.1) — so expected cash stays a sum over ONE ledger.
CREATE UNIQUE INDEX cash_movements_one_float
    ON cash_movements(shift_id) WHERE movement_type = 'OPENING_FLOAT';

-- Cash reaching or leaving the drawer is a live event; it cannot be booked to
-- a night that has already been counted and reported.
CREATE TRIGGER cash_movements_shift_open BEFORE INSERT ON cash_movements
WHEN (SELECT status FROM shifts WHERE id = NEW.shift_id) = 'CLOSED'
BEGIN SELECT RAISE(ABORT, 'cash movements: the shift is already closed and counted'); END;

CREATE TRIGGER cash_movements_no_update BEFORE UPDATE ON cash_movements
BEGIN SELECT RAISE(ABORT, 'cash_movements is append-only'); END;

CREATE TRIGGER cash_movements_no_delete BEFORE DELETE ON cash_movements
BEGIN SELECT RAISE(ABORT, 'cash_movements is append-only'); END;

-- §7.7: a denomination breakdown is required at count time — without it a
-- variance can only be noted, never investigated. One row per denomination per
-- shift; the sum must equal the shift's counted cash, which the close command
-- checks because SQLite cannot express a cross-row sum in a CHECK.
CREATE TABLE cash_count_denominations (
    id                 INTEGER PRIMARY KEY,
    shift_id           INTEGER NOT NULL REFERENCES shifts(id),
    denomination_minor INTEGER NOT NULL CHECK (denomination_minor > 0),
    quantity           INTEGER NOT NULL CHECK (quantity >= 0),

    UNIQUE (shift_id, denomination_minor)
);

CREATE TRIGGER cash_count_denominations_no_update
BEFORE UPDATE ON cash_count_denominations
BEGIN SELECT RAISE(ABORT, 'the counted breakdown is written once, at close'); END;

CREATE TRIGGER cash_count_denominations_no_delete
BEFORE DELETE ON cash_count_denominations
BEGIN SELECT RAISE(ABORT, 'the counted breakdown is written once, at close'); END;
