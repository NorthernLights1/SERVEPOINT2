-- 0002_catalogue.sql — products, sale items, recipes, prices
--
-- The organising idea (§2.1): WHAT YOU COUNT IS NOT WHAT YOU SELL.
--
--   PRODUCT     the physical thing counted on the shelf, held in a base unit
--   SALE ITEM   what appears on the menu
--   RECIPE      sale item -> product(s) x quantity in base units   (the BOM)
--   PRICE       sale item -> money, effective-dated
--
-- One product `Gin` (base unit SHOT, 24 shots per bottle) carries three sale
-- items: "Gin, bottle" (24 shots), "Gin, shot" (1 shot), "Gin & Tonic"
-- (2 shots gin + 0.5 bottle tonic).
--
-- EVERY SALE ITEM HAS A RECIPE. No exceptions. A beer is a one-line recipe
-- consuming one bottle; a shot is a one-line recipe consuming one shot. One
-- code path, no special cases. This is why cocktails were never structural
-- work, and why the inventory layer never learns that shots or cocktails
-- exist. (11-port-decisions.md D27 records this so it is not "added" again.)

-- ---------------------------------------------------------------------------
-- Products — the countable thing (§2.2)
-- ---------------------------------------------------------------------------

CREATE TABLE products (
    id                        INTEGER PRIMARY KEY,
    code                      TEXT    NOT NULL UNIQUE,
    name                      TEXT    NOT NULL CHECK (TRIM(name) <> ''),
    category                  TEXT    NOT NULL DEFAULT '',
    base_unit                 TEXT    NOT NULL
                                      CHECK (base_unit IN ('BOTTLE','SHOT','UNIT')),

    -- Milli of base units in one pack. A beer bottle is 1000 (one base unit
    -- per pack). Gin at 24 shots per bottle is 24000.
    --
    -- §2.2: this conversion factor is an ASSUMPTION, NOT A FACT. 750ml / 30ml
    -- is 25 in theory; real yield is 23-24 after spillage and over-pour. Start
    -- theoretical, then use the yield variance report (§8.3) to find the club's
    -- real number. Shot-level stock will never reconcile exactly, and that is
    -- expected rather than a defect.
    base_units_per_pack       INTEGER NOT NULL CHECK (base_units_per_pack > 0),

    -- Crate provisioning. Invisible in the UI today (§2.2).
    units_per_purchase_pack   INTEGER NOT NULL DEFAULT 1
                                      CHECK (units_per_purchase_pack > 0),

    -- Per product, never global. One threshold across beer and premium spirits
    -- is meaningless (§2.2).
    low_stock_threshold_milli INTEGER NOT NULL DEFAULT 0
                                      CHECK (low_stock_threshold_milli >= 0),

    -- False -> appears on the order and the slip but produces NO stock
    -- movement. Food will land here (§2.2).
    tracks_inventory          INTEGER NOT NULL DEFAULT 1
                                      CHECK (tracks_inventory IN (0,1)),

    -- Drives receipt splitting (§6.7): one document per destination group.
    -- Everything routes to BAR today, so exactly one slip prints and nothing
    -- looks different — but the engine groups from day one, because
    -- retrofitting it would touch numbering, the print queue and crash
    -- recovery, which is the most delicate code in the system.
    destination               TEXT    NOT NULL DEFAULT 'BAR'
                                      CHECK (destination IN ('BAR','KITCHEN')),

    -- Weighted average cost. A DERIVED CACHE, recomputed on each purchase
    -- (§8.2). The authority is the purchase history; an invariant test
    -- recomputes it from the ledger.
    avg_cost_minor            INTEGER NOT NULL DEFAULT 0
                                      CHECK (avg_cost_minor >= 0),

    active                    INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
    created_at                INTEGER NOT NULL
);

CREATE TRIGGER products_no_delete BEFORE DELETE ON products
BEGIN SELECT RAISE(ABORT, 'products are deactivated, never deleted'); END;

CREATE INDEX products_active ON products(active, category);

-- ---------------------------------------------------------------------------
-- Sale items — what appears on the menu
-- ---------------------------------------------------------------------------

CREATE TABLE sale_items (
    id         INTEGER PRIMARY KEY,
    code       TEXT    NOT NULL UNIQUE,
    name       TEXT    NOT NULL CHECK (TRIM(name) <> ''),

    -- Drives the shots / bottles / cocktails breakdown on the reports page
    -- (11-port-decisions.md D13). Deliberately NOT constrained to a fixed
    -- enum: this ships to many venues (D8) and a bar that sells wine by the
    -- glass or shisha needs its own words. NOT NULL and non-blank so the
    -- bucket is always answerable — an unset category would silently drop the
    -- item out of every category total.
    category   TEXT    NOT NULL CHECK (TRIM(category) <> ''),

    active     INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
    created_at INTEGER NOT NULL
);

CREATE TRIGGER sale_items_no_delete BEFORE DELETE ON sale_items
BEGIN SELECT RAISE(ABORT, 'sale items are deactivated, never deleted'); END;

CREATE INDEX sale_items_active ON sale_items(active, category);

-- ---------------------------------------------------------------------------
-- Recipes — the bill of materials, versioned and never edited (§2.3)
--
-- Editing a recipe CLOSES the current version (effective_to = now) and OPENS a
-- new one. Order lines snapshot recipe_id, so a historical order always
-- expands through the recipe that was actually poured against, not today's.
-- ---------------------------------------------------------------------------

CREATE TABLE recipes (
    id             INTEGER PRIMARY KEY,
    sale_item_id   INTEGER NOT NULL REFERENCES sale_items(id),
    version        INTEGER NOT NULL CHECK (version >= 1),
    effective_from INTEGER NOT NULL,
    effective_to   INTEGER,          -- NULL = the current version
    created_by     INTEGER REFERENCES staff(id),
    UNIQUE (sale_item_id, version),
    CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

-- At most one open version per sale item. Two would make "the current recipe"
-- ambiguous, and expansion would silently pick whichever the query returned
-- first. Appendix A bug #5 was exactly this class of error: a null-check read
-- after a later column made every superseded recipe look current.
CREATE UNIQUE INDEX recipes_one_open
    ON recipes(sale_item_id) WHERE effective_to IS NULL;

CREATE INDEX recipes_lookup ON recipes(sale_item_id, effective_from);

-- A version is closed once and stays closed. Reopening one would resurrect a
-- recipe that historical orders were already expanded against.
CREATE TRIGGER recipes_close_once BEFORE UPDATE OF effective_to ON recipes
WHEN OLD.effective_to IS NOT NULL AND NEW.effective_to IS NOT OLD.effective_to
BEGIN SELECT RAISE(ABORT, 'a closed recipe version is never reopened or moved'); END;

CREATE TRIGGER recipes_no_delete BEFORE DELETE ON recipes
BEGIN SELECT RAISE(ABORT, 'recipe versions are closed, never deleted'); END;

CREATE TABLE recipe_lines (
    id             INTEGER PRIMARY KEY,
    recipe_id      INTEGER NOT NULL REFERENCES recipes(id),
    product_id     INTEGER NOT NULL REFERENCES products(id),
    quantity_milli INTEGER NOT NULL CHECK (quantity_milli > 0)
);

-- DELIBERATELY NO UNIQUE (recipe_id, product_id).
--
-- §2.5 requires expansion to SUM rather than overwrite, precisely because a
-- recipe may name the same product twice — a double measure written as two
-- lines. A unique constraint here would forbid the case the specification
-- calls out. This looks like a missing constraint; it is not.
CREATE INDEX recipe_lines_by_recipe ON recipe_lines(recipe_id);

-- Lines belong to the version they were written for. Appending to a closed
-- version would change what a historical order expanded to.
CREATE TRIGGER recipe_lines_only_open BEFORE INSERT ON recipe_lines
WHEN (SELECT effective_to FROM recipes WHERE id = NEW.recipe_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'cannot add lines to a closed recipe version'); END;

CREATE TRIGGER recipe_lines_frozen_upd BEFORE UPDATE ON recipe_lines
WHEN (SELECT effective_to FROM recipes WHERE id = OLD.recipe_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a closed recipe version is immutable'); END;

CREATE TRIGGER recipe_lines_frozen_del BEFORE DELETE ON recipe_lines
WHEN (SELECT effective_to FROM recipes WHERE id = OLD.recipe_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a closed recipe version is immutable'); END;

-- ---------------------------------------------------------------------------
-- Prices — effective-dated (§2.4)
--
-- A price change tonight cannot restate what a customer was charged last
-- night. This table is for lookup and audit; historical totals come from the
-- order line's own snapshotted unit_price_minor.
-- ---------------------------------------------------------------------------

CREATE TABLE prices (
    id             INTEGER PRIMARY KEY,
    sale_item_id   INTEGER NOT NULL REFERENCES sale_items(id),
    price_minor    INTEGER NOT NULL CHECK (price_minor >= 0),
    effective_from INTEGER NOT NULL,
    effective_to   INTEGER,          -- NULL = the current price
    created_by     INTEGER REFERENCES staff(id),
    CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

CREATE UNIQUE INDEX prices_one_open
    ON prices(sale_item_id) WHERE effective_to IS NULL;

CREATE INDEX prices_lookup ON prices(sale_item_id, effective_from);

CREATE TRIGGER prices_close_once BEFORE UPDATE OF effective_to ON prices
WHEN OLD.effective_to IS NOT NULL AND NEW.effective_to IS NOT OLD.effective_to
BEGIN SELECT RAISE(ABORT, 'a closed price is never reopened or moved'); END;

CREATE TRIGGER prices_no_delete BEFORE DELETE ON prices
BEGIN SELECT RAISE(ABORT, 'prices are closed, never deleted'); END;
