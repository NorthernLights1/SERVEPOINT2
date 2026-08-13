-- 0014_measures.sql
-- What one counted unit physically contains.
--
-- A club pours by measure, not by bottle: a single is 30ml, a double is 60, a
-- cocktail might want 45, and wine is a 5oz glass. Writing those as fractions
-- of a bottle — 0.04, 0.08, 0.06 — is arithmetic done by the person least able
-- to check it, at the moment they are least able to check it.
--
-- Every bar system solves this the same way (Restaurant365 fixes a measure
-- type per item; Backbar and the pour-cost tools cost recipes in ml or oz):
-- the item declares what it physically holds, and recipes are written in that
-- measure.
--
-- ---------------------------------------------------------------------------
-- What is deliberately NOT changing
-- ---------------------------------------------------------------------------
--
-- Stock stays counted in the unit written on the shelf. `stock_movements` and
-- `recipe_lines` keep meaning thousandths of a counted unit, so every row
-- already written keeps its meaning exactly and nothing has to be restated.
--
-- The measure is a data-entry conversion applied once, when a recipe line is
-- written: 30ml of a 750ml bottle is stored as 40, being 0.040 of a bottle.
--
-- ponytail: recipe lines therefore round to a thousandth of a counted unit —
-- 0.75ml on a 750ml bottle. The rounding happens once, at definition, so every
-- pour of that drink draws an identical amount rather than drifting. That is
-- an order of magnitude below real pour variance, which 0002_catalogue.sql
-- already notes is 23-24 shots from a theoretical 25. If a venue ever needs
-- exactness, the upgrade is to hold stock in the measure itself and restate
-- on-hand — a migration of ledger rows, which is why it is not done for free
-- here.

ALTER TABLE products ADD COLUMN content_measure TEXT NOT NULL DEFAULT 'NONE'
    CHECK (content_measure IN ('NONE', 'ML', 'GRAM'));

-- Thousandths of the measure in one counted unit: a 750ml bottle is 750000.
-- Zero when there is no measure, which is the case for anything sold whole.
ALTER TABLE products ADD COLUMN content_per_unit_milli INTEGER NOT NULL DEFAULT 0
    CHECK (content_per_unit_milli >= 0);

-- A measure with no size would silently divide by nothing.
CREATE TRIGGER products_measure_needs_a_size_ins BEFORE INSERT ON products
WHEN NEW.content_measure <> 'NONE' AND NEW.content_per_unit_milli <= 0
BEGIN SELECT RAISE(ABORT, 'products: a measured item must say how much one holds'); END;

CREATE TRIGGER products_measure_needs_a_size_upd BEFORE UPDATE ON products
WHEN NEW.content_measure <> 'NONE' AND NEW.content_per_unit_milli <= 0
BEGIN SELECT RAISE(ABORT, 'products: a measured item must say how much one holds'); END;
