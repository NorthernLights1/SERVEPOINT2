-- 0005_receipts.sql — the two documents, and the print-attempt ledger
--
-- §6.7. Two pieces of paper that are not variations of each other:
--
--   BR-  ISSUE RECEIPT      authorises the bartender to pour. Internal, one per
--                           order PER DESTINATION, NO PRICES ON IT, printed
--                           automatically. No fiscal significance whatever.
--   CR-  CUSTOMER RECEIPT   the fiscal document. One per tab, consolidated
--                           across its whole life, with tax and service charge,
--                           printed only on request.
--
-- The issue slip deliberately shows no money, so a customer handed one by
-- accident cannot mistake it for a bill. That is enforced here as a CHECK, not
-- left to the renderer.
--
-- The numbering split (§6.8) is what dissolves most of the print-failure risk:
-- BR- numbers are burned at PRINTING and retained as VOID if abandoned, so the
-- sequence is gapless and every number is accounted for. CR- numbers are only
-- allocated when a document is actually produced, so every number in the
-- FISCAL sequence corresponds to real paper in a customer's hand.

CREATE TABLE receipts (
    id                   INTEGER PRIMARY KEY,
    receipt_type         TEXT    NOT NULL CHECK (receipt_type IN ('ISSUE','CUSTOMER')),
    sequence_no          INTEGER NOT NULL CHECK (sequence_no >= 1),
    receipt_number       TEXT    NOT NULL UNIQUE,        -- BR-000123 / CR-000045

    -- §11.3: a receipt is either an order's slip or a tab's fiscal receipt,
    -- never both and never neither.
    order_id             INTEGER REFERENCES orders(id),
    tab_id               INTEGER REFERENCES tabs(id),

    -- §6.7: the engine groups order lines by destination and emits one
    -- document per group. Everything routes to BAR today so exactly one slip
    -- prints — but the grouping exists from day one, because retrofitting it
    -- would touch numbering, the print queue and crash recovery at once.
    destination          TEXT    CHECK (destination IN ('BAR','KITCHEN')),

    status               TEXT    NOT NULL DEFAULT 'PENDING'
                                 CHECK (status IN ('PENDING','PRINTED','FAILED','VOID')),

    -- Exactly what was sent to the printer. A reprint months later reproduces
    -- this rather than re-rendering against settings that have since changed.
    rendered_text        TEXT,

    -- §6.7: names are STAMPED at issue, never looked up later. Renaming or
    -- deactivating a staff member must not alter a receipt already in a
    -- customer's hands.
    waiter_name          TEXT    NOT NULL CHECK (TRIM(waiter_name) <> ''),
    cashier_name         TEXT,                            -- CUSTOMER only

    -- The frozen bill, copied from tab_payments — never recomputed from
    -- current settings (§6.9). Rates travel with it so an old receipt can
    -- always explain its own arithmetic.
    subtotal_minor       INTEGER CHECK (subtotal_minor       IS NULL OR subtotal_minor       >= 0),
    service_charge_minor INTEGER CHECK (service_charge_minor IS NULL OR service_charge_minor >= 0),
    tax_minor            INTEGER CHECK (tax_minor            IS NULL OR tax_minor            >= 0),
    total_minor          INTEGER CHECK (total_minor          IS NULL OR total_minor          >= 0),
    tax_rate_bp          INTEGER CHECK (tax_rate_bp          IS NULL OR tax_rate_bp          >= 0),
    service_rate_bp      INTEGER CHECK (service_rate_bp      IS NULL OR service_rate_bp      >= 0),
    tax_inclusive        INTEGER CHECK (tax_inclusive        IS NULL OR tax_inclusive IN (0,1)),
    is_comped            INTEGER NOT NULL DEFAULT 0 CHECK (is_comped IN (0,1)),

    shift_id             INTEGER NOT NULL REFERENCES shifts(id),
    created_at           INTEGER NOT NULL,
    printed_at           INTEGER,

    -- One or the other, never both.
    CHECK ((order_id IS NULL) <> (tab_id IS NULL)),

    -- An issue slip belongs to an order and carries a destination; a customer
    -- receipt belongs to a tab and carries a cashier name.
    CHECK (CASE receipt_type
             WHEN 'ISSUE'    THEN order_id IS NOT NULL AND destination IS NOT NULL
             WHEN 'CUSTOMER' THEN tab_id   IS NOT NULL AND destination IS NULL
           END),

    -- §6.7: THE ISSUE SLIP CARRIES NO MONEY. Structurally, not by convention.
    CHECK (receipt_type <> 'ISSUE' OR (
             subtotal_minor IS NULL AND service_charge_minor IS NULL
         AND tax_minor IS NULL AND total_minor IS NULL
         AND tax_rate_bp IS NULL AND service_rate_bp IS NULL
         AND tax_inclusive IS NULL AND cashier_name IS NULL)),

    CHECK (printed_at IS NULL OR printed_at >= created_at)
);

-- §11.3: no gaps and no duplicates within a type. INV-4 asserts the same thing
-- over generated histories; this stops it happening in the first place.
CREATE UNIQUE INDEX receipts_type_seq ON receipts(receipt_type, sequence_no);

-- At most ONE LIVE slip per order per destination. A voided number stays
-- behind so the sequence is accountable, but two live slips for one round
-- would mean the bartender could pour it twice.
CREATE UNIQUE INDEX receipts_one_live_issue
    ON receipts(order_id, destination)
    WHERE order_id IS NOT NULL AND status <> 'VOID';

-- One fiscal document per tab, ever (§6.9). A retry re-prepares the SAME
-- number; it never allocates a second.
CREATE UNIQUE INDEX receipts_one_customer_per_tab
    ON receipts(tab_id) WHERE tab_id IS NOT NULL;

CREATE INDEX receipts_by_shift  ON receipts(shift_id, receipt_type);
CREATE INDEX receipts_unsettled ON receipts(status) WHERE status = 'PENDING';

CREATE TRIGGER receipts_born_pending BEFORE INSERT ON receipts
WHEN NEW.status <> 'PENDING' OR NEW.printed_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'receipts: a receipt is created PENDING, before the printer is touched'); END;

-- §6.9: never for an open tab. The bill must be frozen before it can be
-- printed, or the document contradicts itself the moment the next round lands.
CREATE TRIGGER receipts_customer_needs_closed_tab BEFORE INSERT ON receipts
WHEN NEW.receipt_type = 'CUSTOMER'
 AND (SELECT status FROM tabs WHERE id = NEW.tab_id) NOT IN ('CLOSED','RECONCILED')
BEGIN SELECT RAISE(ABORT, 'receipts: a customer receipt needs a closed tab'); END;

-- §11.2: PENDING -> PRINTED|FAILED|VOID, and FAILED -> PRINTED.
--
-- FAILED -> PRINTED is the handwritten-chit case catching up: the printer came
-- back and the slip finally emerged. The failed original attempt is kept
-- forever in receipt_prints and stays on the exception report.
CREATE TRIGGER receipts_status_machine BEFORE UPDATE OF status ON receipts
WHEN NOT (   OLD.status = NEW.status
          OR (OLD.status = 'PENDING' AND NEW.status IN ('PRINTED','FAILED','VOID'))
          OR (OLD.status = 'FAILED'  AND NEW.status = 'PRINTED'))
BEGIN SELECT RAISE(ABORT, 'receipts: that status transition is not permitted'); END;

-- §6.3 TRANSACTION 1b: the text is frozen before device I/O and immutable
-- once set. A retry must never pick up changed settings, a renamed item or a
-- different waiter name — the paper already out there says what it says.
-- Re-storing byte-identical text is a safe no-op, which is what makes the
-- retry idempotent.
CREATE TRIGGER receipts_render_once BEFORE UPDATE OF rendered_text ON receipts
WHEN (OLD.rendered_text IS NOT NULL AND NEW.rendered_text IS NOT OLD.rendered_text)
  OR (OLD.rendered_text IS NULL AND NEW.rendered_text IS NOT NULL
      AND (OLD.status <> 'PENDING' OR TRIM(NEW.rendered_text) = ''))
BEGIN SELECT RAISE(ABORT, 'receipts: the rendered text is written once, while pending'); END;

CREATE TRIGGER receipts_printed_at_once BEFORE UPDATE ON receipts
WHEN NEW.printed_at IS NOT OLD.printed_at
 AND (OLD.printed_at IS NOT NULL OR NEW.printed_at IS NULL OR NEW.status <> 'PRINTED')
BEGIN SELECT RAISE(ABORT, 'receipts: printed_at is stamped once, when the receipt prints'); END;

-- §11.2: "everything else on those rows — receipt identity, money values,
-- rates, actors — is immutable".
CREATE TRIGGER receipts_frozen BEFORE UPDATE ON receipts
WHEN NEW.receipt_type         <> OLD.receipt_type
  OR NEW.sequence_no          <> OLD.sequence_no
  OR NEW.receipt_number       <> OLD.receipt_number
  OR NEW.order_id             IS NOT OLD.order_id
  OR NEW.tab_id               IS NOT OLD.tab_id
  OR NEW.destination          IS NOT OLD.destination
  OR NEW.waiter_name          <> OLD.waiter_name
  OR NEW.cashier_name         IS NOT OLD.cashier_name
  OR NEW.shift_id             <> OLD.shift_id
  OR NEW.created_at           <> OLD.created_at
  OR NEW.subtotal_minor       IS NOT OLD.subtotal_minor
  OR NEW.service_charge_minor IS NOT OLD.service_charge_minor
  OR NEW.tax_minor            IS NOT OLD.tax_minor
  OR NEW.total_minor          IS NOT OLD.total_minor
  OR NEW.tax_rate_bp          IS NOT OLD.tax_rate_bp
  OR NEW.service_rate_bp      IS NOT OLD.service_rate_bp
  OR NEW.tax_inclusive        IS NOT OLD.tax_inclusive
  OR NEW.is_comped            <> OLD.is_comped
BEGIN SELECT RAISE(ABORT, 'receipts: identity, actors and money are immutable'); END;

-- §6.8: an abandoned number is RETAINED as VOID so the sequence is gapless.
-- Deleting it would create the one thing the design promises does not exist.
CREATE TRIGGER receipts_no_delete BEFORE DELETE ON receipts
BEGIN SELECT RAISE(ABORT, 'receipt numbers are voided, never deleted'); END;

-- ---------------------------------------------------------------------------
-- Print attempts (§6.9, §6.10) — append-only
--
-- THE ATTEMPT IS RECORDED AS `UNKNOWN` BEFORE THE PRINTER IS TOUCHED.
--
-- A power cut after paper emerges therefore leaves both the exact bytes and an
-- explicit recovery question. It can never leave a bare fiscal number with no
-- reproducible document, and it can never silently look like a successful
-- print. UNKNOWN is non-terminal and blocks the shift close (§4.4) until a
-- human answers it.
--
-- A reprint is another row here: no new order, no new number, no new stock
-- movement.
-- ---------------------------------------------------------------------------

CREATE TABLE receipt_prints (
    id         INTEGER PRIMARY KEY,
    receipt_id INTEGER NOT NULL REFERENCES receipts(id),
    print_no   INTEGER NOT NULL CHECK (print_no >= 1),
    outcome    TEXT    NOT NULL DEFAULT 'UNKNOWN'
                       CHECK (outcome IN ('UNKNOWN','SUCCESS','FAILED')),
    reason     TEXT    NOT NULL DEFAULT '',

    -- The shift the ATTEMPT was made in, which for a reprint months later is
    -- not the shift the receipt belongs to. Both are needed: one for the
    -- report the reprint appears on, one for the document itself.
    shift_id   INTEGER NOT NULL REFERENCES shifts(id),
    created_by INTEGER NOT NULL REFERENCES staff(id),
    created_at INTEGER NOT NULL,

    UNIQUE (receipt_id, print_no)
);

CREATE INDEX receipt_prints_by_shift ON receipt_prints(shift_id, outcome);
CREATE INDEX receipt_prints_open     ON receipt_prints(outcome) WHERE outcome = 'UNKNOWN';

CREATE TRIGGER receipt_prints_sequential BEFORE INSERT ON receipt_prints
WHEN NEW.print_no <> 1 + (SELECT COUNT(*) FROM receipt_prints WHERE receipt_id = NEW.receipt_id)
BEGIN SELECT RAISE(ABORT, 'print attempts are numbered consecutively from 1'); END;

-- §6.10: a reprint is blocked while an earlier attempt is still UNKNOWN.
-- Stacking a second copy on top of an unresolved one is how a customer ends up
-- holding two fiscal documents for one payment.
CREATE TRIGGER receipt_prints_resolve_first BEFORE INSERT ON receipt_prints
WHEN EXISTS (SELECT 1 FROM receipt_prints
              WHERE receipt_id = NEW.receipt_id AND outcome = 'UNKNOWN')
BEGIN SELECT RAISE(ABORT, 'resolve the outstanding print attempt before reprinting'); END;

-- A reprint needs a reason and a receipt worth reprinting. A customer receipt
-- must already be PRINTED — reprinting a fiscal document that never printed is
-- a first print, and it goes through the normal path.
CREATE TRIGGER receipt_prints_reprint_rules BEFORE INSERT ON receipt_prints
WHEN NEW.print_no > 1
 AND (TRIM(NEW.reason) = ''
      OR (SELECT status FROM receipts WHERE id = NEW.receipt_id) NOT IN ('PRINTED','FAILED')
      OR ((SELECT receipt_type FROM receipts WHERE id = NEW.receipt_id) = 'CUSTOMER'
          AND (SELECT status FROM receipts WHERE id = NEW.receipt_id) <> 'PRINTED'))
BEGIN SELECT RAISE(ABORT, 'reprints need a reason and a printed or failed receipt'); END;

-- §11.2: UNKNOWN -> SUCCESS|FAILED, exactly once. An attempt that has been
-- answered is history; re-answering it would let a failed print be quietly
-- turned into a successful one.
CREATE TRIGGER receipt_prints_outcome_once BEFORE UPDATE OF outcome ON receipt_prints
WHEN NEW.outcome <> OLD.outcome
 AND NOT (OLD.outcome = 'UNKNOWN' AND NEW.outcome IN ('SUCCESS','FAILED'))
BEGIN SELECT RAISE(ABORT, 'a print attempt is answered once and stays answered'); END;

CREATE TRIGGER receipt_prints_frozen BEFORE UPDATE ON receipt_prints
WHEN NEW.receipt_id <> OLD.receipt_id
  OR NEW.print_no   <> OLD.print_no
  OR NEW.shift_id   <> OLD.shift_id
  OR NEW.created_by <> OLD.created_by
  OR NEW.created_at <> OLD.created_at
BEGIN SELECT RAISE(ABORT, 'receipt_prints: everything but the outcome is immutable'); END;

CREATE TRIGGER receipt_prints_no_delete BEFORE DELETE ON receipt_prints
BEGIN SELECT RAISE(ABORT, 'receipt_prints is append-only'); END;

-- ---------------------------------------------------------------------------
-- The deferred control from 0004 (§6.4)
--
-- A cashier may not void or correct without the printed issue receipt in hand.
-- The number is TYPED, never picked from a list, and it is validated here.
--
-- Merely storing whatever text was entered would make the control cosmetic: an
-- invented number would be accepted, and the spike of returned slips could no
-- longer be counted against the exception report. That check is the only thing
-- standing between "the bartender signed for what came back" and "the cashier
-- said so".
-- ---------------------------------------------------------------------------

CREATE TRIGGER order_corrections_typed_number BEFORE INSERT ON order_corrections
WHEN NOT EXISTS (SELECT 1 FROM receipts
                  WHERE receipt_number = NEW.issue_receipt_number
                    AND receipt_type   = 'ISSUE'
                    AND order_id       = NEW.original_order_id
                    AND status IN ('PRINTED','FAILED'))
BEGIN SELECT RAISE(ABORT, 'corrections: that receipt number is not a printed slip for this order'); END;

CREATE TRIGGER pending_order_corrections_typed_number
BEFORE INSERT ON pending_order_corrections
WHEN NOT EXISTS (SELECT 1 FROM receipts
                  WHERE receipt_number = NEW.issue_receipt_number
                    AND receipt_type   = 'ISSUE'
                    AND order_id       = NEW.original_order_id
                    AND status IN ('PRINTED','FAILED'))
BEGIN SELECT RAISE(ABORT, 'corrections: that receipt number is not a printed slip for this order'); END;
