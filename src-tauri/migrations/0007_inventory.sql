-- 0007_inventory.sql — the movement ledger and stock counts
--
-- §3.1: STOCK ON HAND IS NEVER STORED.
--
--   stock_on_hand(product) = SUM(stock_movements.quantity_milli)
--
-- There is no mutable stock column anywhere in this schema, and adding one
-- would be the single most damaging change anybody could make to it. A cached
-- quantity is a number that drifts away from the transactions that produced
-- it; that drift was the prior prototype's core failure. The same reasoning
-- governs waiter held balances in 0008.
--
-- §3.4 is the rule most likely to be "fixed" by mistake:
--
--   A CORRECTION DOES NOT REVERSE THE ORIGINAL SALE.
--
--   receipt says 5 beers, 2 come back:
--       SALE    -5    stands forever — the bottles genuinely left the shelf
--       RETURN  +2    what the bartender signed for on the back of the slip
--               ---
--       net     -3    matches physical reality
--       billed   3    what the customer owes
--
-- Stock records WHAT PHYSICALLY HAPPENED; the order records WHAT IS OWED.
-- Stock not returned gets NO MOVEMENT AT ALL — those bottles are already gone
-- via the original sale, and posting a LOSS as well would remove the same
-- drinks twice. The gap is a write-off on the correction line: revenue lost,
-- not stock lost.

-- ---------------------------------------------------------------------------
-- Stock counts (§3.5) — between shifts only
-- ---------------------------------------------------------------------------

CREATE TABLE stock_counts (
    id         INTEGER PRIMARY KEY,
    status     TEXT    NOT NULL DEFAULT 'DRAFT' CHECK (status IN ('DRAFT','APPLIED')),
    note       TEXT    NOT NULL DEFAULT '',
    counted_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES staff(id),
    applied_at INTEGER,
    applied_by INTEGER REFERENCES staff(id),

    CHECK (applied_at IS NULL OR applied_at >= counted_at)
);

CREATE TABLE stock_count_lines (
    id             INTEGER PRIMARY KEY,
    stock_count_id INTEGER NOT NULL REFERENCES stock_counts(id),
    product_id     INTEGER NOT NULL REFERENCES products(id),

    -- What the ledger said at the moment of counting, what the human found,
    -- and the difference. All three are kept: the variance alone cannot be
    -- audited, because nobody could tell afterwards which side moved.
    system_milli   INTEGER NOT NULL,
    counted_milli  INTEGER NOT NULL CHECK (counted_milli >= 0),
    variance_milli INTEGER NOT NULL,

    UNIQUE (stock_count_id, product_id),
    CHECK (variance_milli = counted_milli - system_milli)
);

CREATE INDEX stock_count_lines_by_count ON stock_count_lines(stock_count_id);

CREATE TRIGGER stock_counts_born_draft BEFORE INSERT ON stock_counts
WHEN NEW.status <> 'DRAFT'
BEGIN SELECT RAISE(ABORT, 'stock counts are created DRAFT and applied afterwards'); END;

-- §11.2: DRAFT -> APPLIED, then frozen.
CREATE TRIGGER stock_counts_status_machine BEFORE UPDATE OF status ON stock_counts
WHEN NOT (OLD.status = NEW.status OR (OLD.status = 'DRAFT' AND NEW.status = 'APPLIED'))
BEGIN SELECT RAISE(ABORT, 'stock counts: only DRAFT -> APPLIED is permitted'); END;

CREATE TRIGGER stock_counts_frozen BEFORE UPDATE ON stock_counts
WHEN OLD.status = 'APPLIED'
BEGIN SELECT RAISE(ABORT, 'stock counts: an applied count is frozen'); END;

-- §3.5: COUNTING WHILE SALES CONTINUE MEASURES NOTHING, because the shelf
-- moves under the counter's hands. The variance would be a mix of real
-- discrepancy and drinks poured during the count, and nobody could separate
-- them afterwards. So the system refuses to apply a count while a shift is
-- open — the refusal is the feature.
CREATE TRIGGER stock_counts_between_shifts BEFORE UPDATE OF status ON stock_counts
WHEN NEW.status = 'APPLIED'
 AND (EXISTS (SELECT 1 FROM shifts WHERE status IN ('OPEN','CLOSING'))
      OR NEW.applied_at IS NULL OR NEW.applied_by IS NULL)
BEGIN SELECT RAISE(ABORT, 'stock counts: apply between shifts, with a time and a person'); END;

CREATE TRIGGER stock_count_lines_draft_only BEFORE INSERT ON stock_count_lines
WHEN (SELECT status FROM stock_counts WHERE id = NEW.stock_count_id) <> 'DRAFT'
BEGIN SELECT RAISE(ABORT, 'stock count lines: only a draft count accepts lines'); END;

CREATE TRIGGER stock_count_lines_no_update BEFORE UPDATE ON stock_count_lines
BEGIN SELECT RAISE(ABORT, 'stock_count_lines is append-only'); END;

CREATE TRIGGER stock_count_lines_no_delete BEFORE DELETE ON stock_count_lines
BEGIN SELECT RAISE(ABORT, 'stock_count_lines is append-only'); END;

CREATE TRIGGER stock_counts_no_delete BEFORE DELETE ON stock_counts
BEGIN SELECT RAISE(ABORT, 'stock counts are never deleted'); END;

-- ---------------------------------------------------------------------------
-- The movement ledger (§3.2) — append-only, no exceptions
-- ---------------------------------------------------------------------------

CREATE TABLE stock_movements (
    id              INTEGER PRIMARY KEY,
    product_id      INTEGER NOT NULL REFERENCES products(id),

    movement_type   TEXT    NOT NULL
                            CHECK (movement_type IN ('SALE','RETURN','PURCHASE',
                                                     'ADJUSTMENT','DAMAGE','LOSS',
                                                     'STOCK_CORRECTION')),

    -- Signed, and never zero: a movement of nothing is not an event.
    quantity_milli  INTEGER NOT NULL CHECK (quantity_milli <> 0),

    -- Valued at the product's weighted average at the time (§8.2). Returns and
    -- adjustments use the CURRENT average, not the original cost — bar stock
    -- is fungible, so there is no original cost to return to.
    unit_cost_minor INTEGER NOT NULL DEFAULT 0 CHECK (unit_cost_minor >= 0),
    reason          TEXT    NOT NULL DEFAULT '',

    order_id        INTEGER REFERENCES orders(id),
    purchase_id     INTEGER REFERENCES purchases(id),
    stock_count_id  INTEGER REFERENCES stock_counts(id),

    -- NULLABLE, and this is the whole point of Appendix A bug #4: deliveries
    -- and stock counts happen while the club is shut. A NOT NULL here made it
    -- impossible to receive stock outside trading hours, which is when stock
    -- actually arrives.
    shift_id        INTEGER REFERENCES shifts(id),

    created_at      INTEGER NOT NULL,
    created_by      INTEGER NOT NULL REFERENCES staff(id),

    -- §3.2, and §11.3's "new operational movements must carry the correct
    -- source id; only explicit manual entries may be source-less".
    --
    -- The sign is part of the type, not a convention the caller remembers. A
    -- SALE that increased stock, or a PURCHASE that decreased it, would be a
    -- silent inversion that no report would ever surface.
    CHECK (CASE movement_type
             WHEN 'SALE'   THEN quantity_milli < 0 AND order_id IS NOT NULL
                                AND purchase_id IS NULL AND stock_count_id IS NULL
             WHEN 'RETURN' THEN quantity_milli > 0 AND order_id IS NOT NULL
                                AND purchase_id IS NULL AND stock_count_id IS NULL
             WHEN 'PURCHASE' THEN quantity_milli > 0 AND purchase_id IS NOT NULL
                                AND order_id IS NULL AND stock_count_id IS NULL
             WHEN 'ADJUSTMENT' THEN stock_count_id IS NOT NULL
                                AND order_id IS NULL AND purchase_id IS NULL
             -- The source-less trio. Each is somebody asserting a fact the
             -- ledger cannot derive, so each must say why.
             WHEN 'DAMAGE' THEN quantity_milli < 0 AND TRIM(reason) <> ''
                                AND order_id IS NULL AND purchase_id IS NULL
                                AND stock_count_id IS NULL
             WHEN 'LOSS'   THEN quantity_milli < 0 AND TRIM(reason) <> ''
                                AND order_id IS NULL AND purchase_id IS NULL
                                AND stock_count_id IS NULL
             -- D9's relief valve. Insufficient stock always blocks the sale
             -- and there is no setting to turn that off, so the way out of a
             -- wrong count is to fix the count — in the open, with a reason,
             -- on its own block of the shift report.
             WHEN 'STOCK_CORRECTION' THEN TRIM(reason) <> ''
                                AND order_id IS NULL AND purchase_id IS NULL
                                AND stock_count_id IS NULL
           END)
);

CREATE INDEX stock_movements_by_product ON stock_movements(product_id);
CREATE INDEX stock_movements_by_shift   ON stock_movements(shift_id, movement_type);
CREATE INDEX stock_movements_by_order   ON stock_movements(order_id);
CREATE INDEX stock_movements_by_count   ON stock_movements(stock_count_id, product_id);

-- §11.3: a delivery may post stock at most once per product.
CREATE UNIQUE INDEX stock_movements_one_per_purchase_line
    ON stock_movements(purchase_id, product_id) WHERE purchase_id IS NOT NULL;

-- §3.3: inventory decreases when the ISSUE RECEIPT PRINTS, not when payment
-- occurs. The print is the moment the system can observe, and it stands as the
-- proxy for the drinks physically leaving. So a sale movement requires an
-- order that has actually been issued.
CREATE TRIGGER stock_movements_sale_needs_issue BEFORE INSERT ON stock_movements
WHEN NEW.movement_type IN ('SALE','RETURN')
 AND (SELECT issued_at FROM orders WHERE id = NEW.order_id) IS NULL
BEGIN SELECT RAISE(ABORT, 'stock: sales and returns belong to an issued order'); END;

-- §11.3: a purchase movement must match its immutable invoice line, exactly.
-- Anything else means the ledger and the invoice disagree about what arrived,
-- and the weighted average cost silently becomes fiction.
CREATE TRIGGER stock_movements_purchase_matches_line BEFORE INSERT ON stock_movements
WHEN NEW.movement_type = 'PURCHASE'
 AND NOT EXISTS (SELECT 1 FROM purchase_lines
                  WHERE purchase_id = NEW.purchase_id
                    AND product_id  = NEW.product_id
                    AND quantity_milli = NEW.quantity_milli)
BEGIN SELECT RAISE(ABORT, 'stock: a purchase movement must match its invoice line'); END;

-- §11.3: an adjustment must match a count line whose variance is exactly
-- counted - system, and may post at most once per product per count.
CREATE TRIGGER stock_movements_adjustment_matches_count BEFORE INSERT ON stock_movements
WHEN NEW.movement_type = 'ADJUSTMENT'
 AND (NOT EXISTS (SELECT 1 FROM stock_count_lines
                   WHERE stock_count_id = NEW.stock_count_id
                     AND product_id     = NEW.product_id
                     AND variance_milli = NEW.quantity_milli)
      OR EXISTS (SELECT 1 FROM stock_movements
                  WHERE stock_count_id = NEW.stock_count_id
                    AND product_id     = NEW.product_id))
BEGIN SELECT RAISE(ABORT, 'stock: an adjustment must match an unposted count line'); END;

-- A product that does not track inventory produces no movements at all. A row
-- here for one would put something nobody ever counts into every stock figure.
CREATE TRIGGER stock_movements_tracked_only BEFORE INSERT ON stock_movements
WHEN (SELECT tracks_inventory FROM products WHERE id = NEW.product_id) = 0
BEGIN SELECT RAISE(ABORT, 'stock: this product does not track inventory'); END;

-- §3.2, §11.1. The ledger is the authority for every stock figure in the
-- system. If a row here could be edited, nothing computed from it could be
-- trusted, and "stock on hand is never stored" would buy nothing.
CREATE TRIGGER stock_movements_no_update BEFORE UPDATE ON stock_movements
BEGIN SELECT RAISE(ABORT, 'stock_movements is append-only — correct it with another movement'); END;

CREATE TRIGGER stock_movements_no_delete BEFORE DELETE ON stock_movements
BEGIN SELECT RAISE(ABORT, 'stock_movements is append-only — correct it with another movement'); END;

-- ---------------------------------------------------------------------------
-- The deferred control from 0006 (§8.1)
--
-- Lines are inserted BEFORE any movement posts, so the whole invoice is frozen
-- first. Appending a line afterwards would mean stock was posted against an
-- invoice that has since changed, and the two could never be reconciled.
-- ---------------------------------------------------------------------------

CREATE TRIGGER purchase_lines_frozen_once_posted BEFORE INSERT ON purchase_lines
WHEN EXISTS (SELECT 1 FROM stock_movements WHERE purchase_id = NEW.purchase_id)
BEGIN SELECT RAISE(ABORT, 'purchases: no line may be added after stock has posted'); END;
