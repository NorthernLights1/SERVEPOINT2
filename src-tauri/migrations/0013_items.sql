-- 0013_items.sql
-- A menu entry may be the twin of one shelf item.
--
-- The split between a thing on a shelf and a thing on a menu is what lets one
-- gin bottle pour 24 shots, and it is why stock reconciles. It stays.
--
-- But a club sells bottles. For most of a club's list the two are the same
-- thing under two names, and making somebody create a product, then a menu
-- item, then a recipe joining them, then a price, is four steps to say "a
-- bottle of Harar, 120". It is also four chances to mistype, and a
-- half-finished one is invisible until the till refuses to sell it.
--
-- So a menu entry can now record that it IS a shelf item, sold one for one.
-- The recipe is still written and still does the work — nothing downstream
-- knows the difference — but the screen can present the pair as a single row
-- and create both together.
--
-- NULL means what it has always meant: a composed drink, whose recipe is the
-- only thing that says what it draws.

ALTER TABLE sale_items ADD COLUMN from_product_id INTEGER REFERENCES products(id);

-- Link what is already a one-for-one pairing: a live recipe of exactly one
-- line drawing exactly one base unit. Anything else is composed by definition
-- and is left alone.
UPDATE sale_items
   SET from_product_id = (
       SELECT l.product_id
         FROM recipes r
         JOIN recipe_lines l ON l.recipe_id = r.id
        WHERE r.sale_item_id = sale_items.id
          AND r.effective_to IS NULL
        GROUP BY r.id
       HAVING COUNT(*) = 1 AND MIN(l.quantity_milli) = 1000
   );

-- Two menu entries may both have been a one-for-one of the same product. That
-- is legitimate history and is not rewritten; only the older of the pair keeps
-- the twin relationship, because the screen shows one price per shelf row.
UPDATE sale_items
   SET from_product_id = NULL
 WHERE from_product_id IS NOT NULL
   AND id > (
       SELECT MIN(other.id) FROM sale_items AS other
        WHERE other.from_product_id = sale_items.from_product_id
   );

CREATE UNIQUE INDEX sale_items_one_twin_per_product
    ON sale_items (from_product_id)
 WHERE from_product_id IS NOT NULL;
