-- 0012_generated_codes.sql
-- Catalogue and staff codes become machine-allocated (§1.4).
--
-- A code was previously typed by whoever added the row. That is a small piece
-- of bookkeeping handed to a person standing at a bar, and it drifts: W-001
-- and W001 and w-1 all end up meaning the same bottle in different hands, and
-- a duplicate is only discovered when the insert is refused.
--
-- These now come from the same allocator that hands out BR- and TAB- numbers,
-- so they are unique by construction and nobody has to invent one.
--
-- Codes are identity, not description: they are never derived from a category
-- or a name, because a product that is recategorised would otherwise carry a
-- code that lies about it, and reissuing it would break anything already
-- counted or printed against the old one.

-- ---------------------------------------------------------------------------
-- Three more counters. The CHECK lists them exhaustively, so a typo in the
-- Rust `Counter::row()` fails at the first allocation rather than silently
-- creating an eighth counter — which is also why the table has to be rebuilt
-- to widen it.
-- ---------------------------------------------------------------------------

CREATE TABLE sequences_new (
    name        TEXT    PRIMARY KEY
                        CHECK (name IN ('TAB','SHIFT','ISSUE_RECEIPT','CUSTOMER_RECEIPT',
                                        'PRODUCT','SALE_ITEM','STAFF')),
    next_value  INTEGER NOT NULL CHECK (next_value >= 1)
);

INSERT INTO sequences_new (name, next_value)
SELECT name, next_value FROM sequences;

DROP TABLE sequences;
ALTER TABLE sequences_new RENAME TO sequences;

INSERT INTO sequences (name, next_value) VALUES ('PRODUCT', 1), ('SALE_ITEM', 1), ('STAFF', 1);

-- ---------------------------------------------------------------------------
-- Renumber what is already here.
--
-- Two passes per table. Every code is first moved to a temporary form that is
-- unique by construction, because assigning the final codes in one pass could
-- collide with a hand-typed code that already happens to look like a generated
-- one, and `code` is UNIQUE.
--
-- Ordering is by `id`, so the oldest row keeps the lowest number and the
-- sequence continues from the end rather than reusing anything.
-- ---------------------------------------------------------------------------

UPDATE products SET code = 'migrating:' || id;
UPDATE products
   SET code = printf('PRD-%06d',
                     (SELECT COUNT(*) FROM products AS earlier WHERE earlier.id <= products.id));

UPDATE sale_items SET code = 'migrating:' || id;
UPDATE sale_items
   SET code = printf('ITM-%06d',
                     (SELECT COUNT(*) FROM sale_items AS earlier WHERE earlier.id <= sale_items.id));

UPDATE staff SET code = 'migrating:' || id;
UPDATE staff
   SET code = printf('STF-%06d',
                     (SELECT COUNT(*) FROM staff AS earlier WHERE earlier.id <= staff.id));

-- Continue past what was just assigned, so the next allocation cannot hand out
-- a code a row is already holding.
UPDATE sequences SET next_value = 1 + (SELECT COUNT(*) FROM products)   WHERE name = 'PRODUCT';
UPDATE sequences SET next_value = 1 + (SELECT COUNT(*) FROM sale_items) WHERE name = 'SALE_ITEM';
UPDATE sequences SET next_value = 1 + (SELECT COUNT(*) FROM staff)      WHERE name = 'STAFF';
