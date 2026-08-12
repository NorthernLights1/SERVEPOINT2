-- 0006_purchasing.sql — suppliers, deliveries, invoice lines
--
-- §8.1: PURCHASES DO NOT REQUIRE AN OPEN SHIFT. Deliveries arrive during the
-- day while the club is shut. Refusing to record one because nobody is trading
-- pushes the club into entering it wrong later — which is exactly the schema
-- defect that shipped in the prior build (Appendix A #4: stock_movements
-- .shift_id was NOT NULL, so a delivery could not be received at all).
--
-- shift_id here is therefore NULLABLE, deliberately.

CREATE TABLE suppliers (
    id              INTEGER PRIMARY KEY,
    name            TEXT    NOT NULL CHECK (TRIM(name) <> ''),

    -- §11.3: normalized identity. Without it "Dashen Brewery" and
    -- "dashen brewery " are two suppliers, and the same invoice can be
    -- received twice — once against each — silently doubling stock and cost.
    normalized_name TEXT    NOT NULL UNIQUE
                            CHECK (normalized_name = LOWER(TRIM(name))),

    phone           TEXT,
    active          INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
    created_at      INTEGER NOT NULL
);

CREATE TRIGGER suppliers_no_delete BEFORE DELETE ON suppliers
BEGIN SELECT RAISE(ABORT, 'suppliers are deactivated, never deleted'); END;

-- ---------------------------------------------------------------------------
-- Purchases (§8.1) — append-only
-- ---------------------------------------------------------------------------

CREATE TABLE purchases (
    id               INTEGER PRIMARY KEY,
    supplier_id      INTEGER NOT NULL REFERENCES suppliers(id),

    -- NULL where the delivery genuinely arrived without paperwork. NULLs do
    -- not collide in a unique index, so several no-invoice deliveries from one
    -- supplier are fine while a real reference can never be used twice.
    invoice_ref      TEXT,

    received_at      INTEGER NOT NULL,
    shift_id         INTEGER REFERENCES shifts(id),      -- NULL: the club was shut
    total_cost_minor INTEGER NOT NULL CHECK (total_cost_minor >= 0),
    created_by       INTEGER NOT NULL REFERENCES staff(id),
    created_at       INTEGER NOT NULL,

    -- §8.1: the same delivery must not be postable twice.
    UNIQUE (supplier_id, invoice_ref)
);

CREATE INDEX purchases_by_supplier ON purchases(supplier_id, received_at);
CREATE INDEX purchases_by_shift    ON purchases(shift_id);

CREATE TRIGGER purchases_no_update BEFORE UPDATE ON purchases
BEGIN SELECT RAISE(ABORT, 'purchases is append-only'); END;

CREATE TRIGGER purchases_no_delete BEFORE DELETE ON purchases
BEGIN SELECT RAISE(ABORT, 'purchases is append-only'); END;

-- ---------------------------------------------------------------------------
-- Purchase lines (§8.2) — append-only, and frozen once stock has posted
--
-- Two costs per line, on purpose:
--
--   line_cost_minor   THE EXACT AMOUNT ON THE SUPPLIER'S INVOICE. Authoritative.
--   unit_cost_minor   a rounded per-base-unit rate, for valuing movements.
--
-- The weighted average is computed from the exact line total (§8.2), never
-- from the rounded rate, so rounding cannot accumulate into the cost of goods.
-- The two are required to agree within one santim, which catches a unit cost
-- typed against the wrong pack size.
-- ---------------------------------------------------------------------------

CREATE TABLE purchase_lines (
    id              INTEGER PRIMARY KEY,
    purchase_id     INTEGER NOT NULL REFERENCES purchases(id),
    product_id      INTEGER NOT NULL REFERENCES products(id),

    -- In the product's BASE units (§8.1): shots for spirits, bottles for beer.
    -- Five bottles of gin at 24 shots each arrives here as 120000 milli-shots.
    quantity_milli  INTEGER NOT NULL CHECK (quantity_milli > 0),

    unit_cost_minor INTEGER NOT NULL CHECK (unit_cost_minor >= 0),
    line_cost_minor INTEGER NOT NULL CHECK (line_cost_minor >= 0),

    -- §8.1: product-distinct within one invoice, so a delivery cannot post
    -- stock for the same product twice.
    UNIQUE (purchase_id, product_id),

    -- §8.2 line cost consistency, within one minor unit.
    CHECK (ABS(unit_cost_minor
               - ((line_cost_minor * 1000 + quantity_milli / 2) / quantity_milli)) <= 1)
);

CREATE INDEX purchase_lines_by_purchase ON purchase_lines(purchase_id);
CREATE INDEX purchase_lines_by_product  ON purchase_lines(product_id);

CREATE TRIGGER purchase_lines_no_update BEFORE UPDATE ON purchase_lines
BEGIN SELECT RAISE(ABORT, 'purchase_lines is append-only'); END;

CREATE TRIGGER purchase_lines_no_delete BEFORE DELETE ON purchase_lines
BEGIN SELECT RAISE(ABORT, 'purchase_lines is append-only'); END;

-- The trigger that stops a line being appended to an already-posted purchase
-- needs stock_movements, so it is created in 0007 alongside them.
