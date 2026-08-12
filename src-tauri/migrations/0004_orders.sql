-- 0004_orders.sql — orders, lines, corrections, and the frozen correction intent
--
-- This is the most delicate table in the system, and the reason is one line of
-- §6.3: THE `PRINTING` STATE COMMITS BEFORE THE PRINTER IS TOUCHED.
--
-- A single transaction around "print the slip and issue the order" looks
-- tidier and is wrong. If the power dies mid-print it rolls back to a draft
-- while a numbered slip may already be in the bartender's hand authorising
-- drinks the system has forgotten. Committing PRINTING first leaves the order
-- visibly stranded instead, and a stranded order can be asked about on restart:
-- "did BR-000123 come out?" A forgotten one cannot.
--
-- Every consequence below follows from that: the status machine, the retained
-- VOID numbers, the pending-correction tables that exist purely so a
-- half-finished correction can be finished or abandoned after a crash.
--
-- Receipts live in 0005 and stock movements in 0006. The reference runs
-- receipts -> orders, so nothing here points forward; the typed BR number is
-- stored as text and 0005 adds the trigger that validates it against the real
-- receipt.

-- ---------------------------------------------------------------------------
-- Orders (§6.1, §6.2)
-- ---------------------------------------------------------------------------

CREATE TABLE orders (
    id                INTEGER PRIMARY KEY,
    tab_id            INTEGER NOT NULL REFERENCES tabs(id),

    -- The shift the order was ISSUED in, which is what revenue reports on.
    -- A draft that failed to print last night is carried into tonight before
    -- the retry (§6.3), because a failed print is not a sale yet.
    shift_id          INTEGER NOT NULL REFERENCES shifts(id),

    -- Frozen at creation. §5.4: a later tab transfer moves who must SETTLE,
    -- never who SOLD. Rewriting this would falsify what actually happened.
    waiter_id         INTEGER NOT NULL REFERENCES staff(id),
    cashier_id        INTEGER NOT NULL REFERENCES staff(id),

    status            TEXT    NOT NULL DEFAULT 'DRAFT'
                              CHECK (status IN ('DRAFT','PRINTING','ISSUED',
                                                'REPLACED','VOIDED','ABANDONED')),

    created_at        INTEGER NOT NULL,
    issued_at         INTEGER,

    -- Correction chains (§6.5). NULL root means "I am the root" — one column
    -- fewer to keep consistent, and COALESCE(root_order_id, id) identifies the
    -- family everywhere.
    replaces_order_id INTEGER REFERENCES orders(id),
    root_order_id     INTEGER REFERENCES orders(id),

    void_reason       TEXT,
    voided_at         INTEGER,
    voided_by         INTEGER REFERENCES staff(id),

    CHECK (issued_at IS NULL OR issued_at >= created_at)
);

CREATE INDEX orders_by_tab   ON orders(tab_id, status);
CREATE INDEX orders_by_shift ON orders(shift_id, status);
CREATE INDEX orders_stranded ON orders(status) WHERE status IN ('DRAFT','PRINTING');
CREATE INDEX orders_by_root  ON orders(root_order_id);

-- §11.3 / INV-5: an order may be replaced AT MOST ONCE. Without this the chain
-- forks — A is replaced by B and also by C — and both leaves count toward the
-- tab total, so one round is billed twice.
CREATE UNIQUE INDEX orders_one_replacement
    ON orders(replaces_order_id) WHERE replaces_order_id IS NOT NULL;

-- §6.2. Any transition not on this list aborts at the database, not in a
-- service method that a future refactor could route around.
--
--   DRAFT     -> PRINTING | ABANDONED
--   PRINTING  -> ISSUED | DRAFT      (DRAFT = retry after a CONFIRMED non-print)
--   ISSUED    -> REPLACED | VOIDED
--   REPLACED / VOIDED / ABANDONED are terminal
CREATE TRIGGER orders_status_machine BEFORE UPDATE OF status ON orders
WHEN NOT (   OLD.status = NEW.status
          OR (OLD.status = 'DRAFT'    AND NEW.status IN ('PRINTING','ABANDONED'))
          OR (OLD.status = 'PRINTING' AND NEW.status IN ('ISSUED','DRAFT'))
          OR (OLD.status = 'ISSUED'   AND NEW.status IN ('REPLACED','VOIDED')))
BEGIN SELECT RAISE(ABORT, 'orders: that status transition is not on the state machine'); END;

-- Every order starts as a draft. A draft holds no receipt number and may be
-- abandoned freely; "orders are never edited" applies from ISSUED onwards.
CREATE TRIGGER orders_born_draft BEFORE INSERT ON orders
WHEN NEW.status <> 'DRAFT'
BEGIN SELECT RAISE(ABORT, 'orders: an order is always created DRAFT'); END;

CREATE TRIGGER orders_issue_needs_time BEFORE UPDATE OF status ON orders
WHEN NEW.status = 'ISSUED' AND NEW.issued_at IS NULL
BEGIN SELECT RAISE(ABORT, 'orders: issuing requires issued_at'); END;

-- §6.6: a void is permanent and must always answer "who, when, and why".
CREATE TRIGGER orders_void_needs_reason BEFORE UPDATE OF status ON orders
WHEN NEW.status = 'VOIDED'
 AND (NEW.voided_at IS NULL OR NEW.voided_by IS NULL
      OR TRIM(COALESCE(NEW.void_reason, '')) = '')
BEGIN SELECT RAISE(ABORT, 'orders: voiding requires a reason, a time and a person'); END;

-- The shift may only be rewritten while the order is still a draft (§6.3
-- carries a failed-print draft into tonight). Once issued, the shift is the
-- night the money was earned and a shift report has been built on it.
CREATE TRIGGER orders_shift_frozen_after_draft BEFORE UPDATE OF shift_id ON orders
WHEN OLD.status <> 'DRAFT' AND NEW.shift_id <> OLD.shift_id
BEGIN SELECT RAISE(ABORT, 'orders: only a draft may be carried into another shift'); END;

-- Likewise the chain link. Abandoning a correction releases the slot by
-- setting replaces_order_id back to NULL (§6.5), and that is only legal while
-- the replacement is a draft again.
CREATE TRIGGER orders_chain_frozen_after_draft BEFORE UPDATE OF replaces_order_id ON orders
WHEN OLD.status <> 'DRAFT'
 AND (NEW.replaces_order_id IS NOT OLD.replaces_order_id)
BEGIN SELECT RAISE(ABORT, 'orders: the chain link is frozen once the order leaves DRAFT'); END;

-- Identity facts never move.
CREATE TRIGGER orders_identity_frozen BEFORE UPDATE ON orders
WHEN NEW.tab_id     <> OLD.tab_id
  OR NEW.waiter_id  <> OLD.waiter_id
  OR NEW.cashier_id <> OLD.cashier_id
  OR NEW.created_at <> OLD.created_at
BEGIN SELECT RAISE(ABORT, 'orders: tab, waiter, cashier and creation time are immutable'); END;

-- A replacement joins an existing family and must carry its root; a first
-- order is its own root and carries none. Without both halves, chain walking
-- silently returns the wrong leaf and revenue double-counts.
CREATE TRIGGER orders_chain_root_ins BEFORE INSERT ON orders
WHEN (NEW.replaces_order_id IS NULL AND NEW.root_order_id IS NOT NULL)
  OR (NEW.replaces_order_id IS NOT NULL
      AND (NEW.root_order_id IS NULL
           OR NEW.root_order_id <> (SELECT COALESCE(root_order_id, id)
                                      FROM orders WHERE id = NEW.replaces_order_id)))
BEGIN SELECT RAISE(ABORT, 'orders: a replacement must carry the root of the chain it joins'); END;

-- §6.5: only an ISSUED order can be corrected. Replacing a draft, a voided
-- order or an already-replaced one produces a chain nobody can total.
CREATE TRIGGER orders_replaces_issued BEFORE INSERT ON orders
WHEN NEW.replaces_order_id IS NOT NULL
 AND (SELECT status FROM orders WHERE id = NEW.replaces_order_id) <> 'ISSUED'
BEGIN SELECT RAISE(ABORT, 'orders: only an issued order can be replaced'); END;

-- An order is a round on a tab that is still taking orders, rung up by
-- somebody who can log in, on the night that is trading.
CREATE TRIGGER orders_tab_open BEFORE INSERT ON orders
WHEN (SELECT status FROM tabs WHERE id = NEW.tab_id) <> 'OPEN'
BEGIN SELECT RAISE(ABORT, 'orders: the tab must be open'); END;

CREATE TRIGGER orders_actors_valid BEFORE INSERT ON orders
WHEN (SELECT role FROM staff WHERE id = NEW.waiter_id) <> 'WAITER'
  OR (SELECT role FROM staff WHERE id = NEW.cashier_id) NOT IN ('OWNER','CASHIER')
BEGIN SELECT RAISE(ABORT, 'orders: a waiter sells it, an owner or cashier rings it up'); END;

CREATE TRIGGER orders_no_delete BEFORE DELETE ON orders
BEGIN SELECT RAISE(ABORT, 'orders are voided or abandoned, never deleted'); END;

-- ---------------------------------------------------------------------------
-- Order lines (§6.1) — append-only
--
-- Every line SNAPSHOTS the sale item name, the recipe version and the unit
-- price as they were at creation. A renamed item, an edited recipe or a price
-- change can then never rewrite history, and the recipe snapshot is what makes
-- a historical order expand to the stock that was actually poured.
-- ---------------------------------------------------------------------------

CREATE TABLE order_lines (
    id               INTEGER PRIMARY KEY,
    order_id         INTEGER NOT NULL REFERENCES orders(id),
    sale_item_id     INTEGER NOT NULL REFERENCES sale_items(id),

    sale_item_name   TEXT    NOT NULL CHECK (TRIM(sale_item_name) <> ''),
    recipe_id        INTEGER NOT NULL REFERENCES recipes(id),

    quantity_milli   INTEGER NOT NULL CHECK (quantity_milli > 0),
    unit_price_minor INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    line_total_minor INTEGER NOT NULL CHECK (line_total_minor >= 0),

    -- §1.1 half-up, written out in SQL. Both operands are non-negative, so
    -- integer division truncates toward zero and +500 makes it round half up.
    -- This is not defensive typing: the line total is the number the customer
    -- is charged, and a service-layer bug that computed it any other way would
    -- be invisible until an owner added up a receipt by hand.
    CHECK (line_total_minor = (quantity_milli * unit_price_minor + 500) / 1000)
);

CREATE INDEX order_lines_by_order ON order_lines(order_id);

-- Lines may only be written while the order is a draft. Once PRINTING, the
-- slip text is being frozen and a new line would authorise a drink that never
-- appeared on the paper the bartender holds.
CREATE TRIGGER order_lines_draft_only BEFORE INSERT ON order_lines
WHEN (SELECT status FROM orders WHERE id = NEW.order_id) <> 'DRAFT'
BEGIN SELECT RAISE(ABORT, 'order lines: lines may only be added to a draft'); END;

CREATE TRIGGER order_lines_no_update BEFORE UPDATE ON order_lines
BEGIN SELECT RAISE(ABORT, 'order_lines is append-only'); END;

CREATE TRIGGER order_lines_no_delete BEFORE DELETE ON order_lines
BEGIN SELECT RAISE(ABORT, 'order_lines is append-only'); END;

-- ---------------------------------------------------------------------------
-- Corrections and voids (§6.4, §6.5, §6.6) — append-only
--
-- The rule that makes the rest coherent: A CORRECTION NEVER REVERSES THE
-- ORIGINAL SALE. It adjusts the bill. What physically came back is a separate
-- fact, supplied by the bartender's signed note on the back of the slip, and
-- only that produces a RETURN movement. The difference between what left the
-- bill and what came back is a write-off: no stock movement, ever, and it
-- appears on the exception report where somebody has to explain it.
--
-- The typed BR number (§6.4) is the physical control. It is typed, never
-- picked from a list, so the spike of returned slips can be counted against
-- the report. 0005 adds the trigger that checks it is really an ISSUE receipt
-- for this order in PRINTED or FAILED state.
-- ---------------------------------------------------------------------------

CREATE TABLE order_corrections (
    id                   INTEGER PRIMARY KEY,
    correction_type      TEXT    NOT NULL CHECK (correction_type IN ('CORRECTION','VOID')),

    -- One correction per order, forever: the original becomes REPLACED or
    -- VOIDED, both terminal.
    original_order_id    INTEGER NOT NULL UNIQUE REFERENCES orders(id),
    replacement_order_id INTEGER REFERENCES orders(id),

    issue_receipt_number TEXT    NOT NULL CHECK (TRIM(issue_receipt_number) <> ''),
    reason               TEXT    NOT NULL CHECK (TRIM(reason) <> ''),

    shift_id             INTEGER NOT NULL REFERENCES shifts(id),
    created_by           INTEGER NOT NULL REFERENCES staff(id),
    created_at           INTEGER NOT NULL,

    -- A void is a correction to nothing (§6.6); a correction to nothing is a
    -- void. The two must not be confusable, or the exception report counts
    -- them in the wrong column.
    CHECK ((correction_type = 'VOID') = (replacement_order_id IS NULL))
);

CREATE INDEX order_corrections_by_shift ON order_corrections(shift_id, correction_type);

-- §6.5: the order must belong to the shift doing the correcting. After a night
-- closes, its report is stored and its money is banked; the remedy for a
-- mistake found later is a stock adjustment and a written note, never a
-- restatement. This one constraint is what makes every shift report
-- self-contained and eliminates prior-period corrections entirely.
CREATE TRIGGER order_corrections_same_shift BEFORE INSERT ON order_corrections
WHEN (SELECT shift_id FROM orders WHERE id = NEW.original_order_id) <> NEW.shift_id
BEGIN SELECT RAISE(ABORT, 'corrections: an order can only be corrected in its own shift'); END;

-- ...and the shift must actually be trading. Correcting during the close, or
-- after it, would change a figure the cashier has already counted against.
CREATE TRIGGER order_corrections_shift_open BEFORE INSERT ON order_corrections
WHEN (SELECT status FROM shifts WHERE id = NEW.shift_id) <> 'OPEN'
BEGIN SELECT RAISE(ABORT, 'corrections: only during a trading night'); END;

CREATE TRIGGER order_corrections_original_issued BEFORE INSERT ON order_corrections
WHEN (SELECT status FROM orders WHERE id = NEW.original_order_id) <> 'ISSUED'
BEGIN SELECT RAISE(ABORT, 'corrections: only an issued order can be corrected or voided'); END;

CREATE TRIGGER order_corrections_no_update BEFORE UPDATE ON order_corrections
BEGIN SELECT RAISE(ABORT, 'order_corrections is append-only'); END;

CREATE TRIGGER order_corrections_no_delete BEFORE DELETE ON order_corrections
BEGIN SELECT RAISE(ABORT, 'order_corrections is append-only'); END;

CREATE TABLE order_correction_lines (
    id                INTEGER PRIMARY KEY,
    correction_id     INTEGER NOT NULL REFERENCES order_corrections(id),
    product_id        INTEGER NOT NULL REFERENCES products(id),

    -- Expanded through the recipes, in base units. before/after are the whole
    -- order either side of the correction, so the delta is what actually moves.
    before_milli      INTEGER NOT NULL CHECK (before_milli >= 0),
    after_milli       INTEGER NOT NULL CHECK (after_milli  >= 0),
    delta_milli       INTEGER NOT NULL,

    -- The bartender's signed quantity. This is a physical fact the system
    -- cannot derive.
    returned_milli    INTEGER NOT NULL DEFAULT 0 CHECK (returned_milli    >= 0),
    written_off_milli INTEGER NOT NULL DEFAULT 0 CHECK (written_off_milli >= 0),

    note              TEXT    NOT NULL DEFAULT '',

    CHECK (delta_milli = after_milli - before_milli),

    -- §6.5, and §11.3's "returned + written_off = the stock reduction".
    --
    -- Nothing was taken off the bill -> nothing can have come back. Something
    -- was taken off -> every unit is either physically returned to the shelf
    -- or written off. The gap between them is the number worth watching: it is
    -- drink that left the building and was not paid for.
    CHECK (CASE
             WHEN delta_milli >= 0 THEN returned_milli = 0 AND written_off_milli = 0
             ELSE returned_milli + written_off_milli = -delta_milli
           END)
);

CREATE INDEX order_correction_lines_by_correction
    ON order_correction_lines(correction_id);

CREATE TRIGGER order_correction_lines_no_update BEFORE UPDATE ON order_correction_lines
BEGIN SELECT RAISE(ABORT, 'order_correction_lines is append-only'); END;

CREATE TRIGGER order_correction_lines_no_delete BEFORE DELETE ON order_correction_lines
BEGIN SELECT RAISE(ABORT, 'order_correction_lines is append-only'); END;

-- ---------------------------------------------------------------------------
-- Pending corrections (§6.5) — the frozen intent
--
-- Written and committed BEFORE the replacement slip is printed, and deleted
-- once the correction completes. Its whole purpose is the crash in between:
-- the deltas, the returned quantities and the write-offs are computed while
-- the original order is still intact, so recovery can finish the correction
-- exactly as intended or abandon it cleanly — without recomputing against a
-- catalogue that may have changed in the meantime.
--
-- Append-only in the sense that matters (§11.1): never edited. Deletion is
-- part of the completion protocol, not a data change.
-- ---------------------------------------------------------------------------

CREATE TABLE pending_order_corrections (
    id                   INTEGER PRIMARY KEY,
    original_order_id    INTEGER NOT NULL UNIQUE REFERENCES orders(id),
    replacement_order_id INTEGER NOT NULL UNIQUE REFERENCES orders(id),
    issue_receipt_number TEXT    NOT NULL CHECK (TRIM(issue_receipt_number) <> ''),
    reason               TEXT    NOT NULL CHECK (TRIM(reason) <> ''),
    shift_id             INTEGER NOT NULL REFERENCES shifts(id),
    created_by           INTEGER NOT NULL REFERENCES staff(id),
    created_at           INTEGER NOT NULL
);

CREATE TABLE pending_order_correction_lines (
    id                INTEGER PRIMARY KEY,
    pending_id        INTEGER NOT NULL REFERENCES pending_order_corrections(id),
    product_id        INTEGER NOT NULL REFERENCES products(id),
    before_milli      INTEGER NOT NULL CHECK (before_milli >= 0),
    after_milli       INTEGER NOT NULL CHECK (after_milli  >= 0),
    delta_milli       INTEGER NOT NULL,
    returned_milli    INTEGER NOT NULL DEFAULT 0 CHECK (returned_milli    >= 0),
    written_off_milli INTEGER NOT NULL DEFAULT 0 CHECK (written_off_milli >= 0),
    note              TEXT    NOT NULL DEFAULT '',

    CHECK (delta_milli = after_milli - before_milli),

    -- The same arithmetic as the applied lines. The intent must already be
    -- valid when it is frozen; discovering it was not, during recovery, would
    -- leave a correction that can neither be finished nor explained.
    CHECK (CASE
             WHEN delta_milli >= 0 THEN returned_milli = 0 AND written_off_milli = 0
             ELSE returned_milli + written_off_milli = -delta_milli
           END)
);

CREATE INDEX pending_order_correction_lines_by_pending
    ON pending_order_correction_lines(pending_id);

CREATE TRIGGER pending_order_corrections_no_update
BEFORE UPDATE ON pending_order_corrections
BEGIN SELECT RAISE(ABORT, 'a frozen correction intent is never edited'); END;

CREATE TRIGGER pending_order_correction_lines_no_update
BEFORE UPDATE ON pending_order_correction_lines
BEGIN SELECT RAISE(ABORT, 'a frozen correction intent is never edited'); END;

-- Deleting the header must not orphan its lines: recovery reads them together
-- and a dangling line would look like an intent that is still outstanding.
CREATE TRIGGER pending_order_corrections_lines_first
BEFORE DELETE ON pending_order_corrections
WHEN EXISTS (SELECT 1 FROM pending_order_correction_lines WHERE pending_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'clear the intent lines before the intent itself'); END;
