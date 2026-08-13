//! Products, menu items, and the recipe that joins them (§2).
//!
//! WHAT YOU COUNT IS NOT WHAT YOU SELL. A shelf holds *Gin*; a menu offers a
//! shot of gin, a bottle of gin and a Gin & Tonic. The recipe is the only
//! bridge, and **every** menu item has one — a beer's recipe is a single line
//! consuming one bottle. That uniformity is why cocktails and shots were never
//! structural work: nothing below this line has ever heard of either.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};

use super::{guarded, RepoError, Result};
use crate::{Milli, Money};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Product {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub base_unit: String,
    pub base_units_per_pack: i64,
    pub units_per_purchase_pack: i64,
    pub low_stock_threshold: Milli,
    pub tracks_inventory: bool,
    pub destination: String,
    pub avg_cost: Money,
    pub active: bool,
    /// `NONE`, `ML` or `GRAM` — what one counted unit is made of, when it is
    /// poured or weighed out rather than handed over whole.
    pub content_measure: String,
    /// How much of that measure one counted unit holds: a 750ml bottle is
    /// `Milli::from_units(750)`. Zero when there is no measure.
    pub content_per_unit: Milli,
}

#[derive(Clone, Debug)]
pub struct NewProduct<'a> {
    pub code: &'a str,
    pub name: &'a str,
    pub category: &'a str,
    pub base_unit: &'a str,
    pub base_units_per_pack: Milli,
    pub units_per_purchase_pack: i64,
    pub low_stock_threshold: Milli,
    pub tracks_inventory: bool,
    pub destination: &'a str,
    /// `NONE`, `ML` or `GRAM`.
    pub content_measure: &'a str,
    /// How much of that measure one counted unit holds. Zero when there is none.
    pub content_per_unit: Milli,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleItem {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct NewSaleItem<'a> {
    pub code: &'a str,
    pub name: &'a str,
    pub category: &'a str,
}

/// A line on the menu: the item, what it costs tonight, and the recipe version
/// currently in force. All three are read together because an order line
/// snapshots all three, and reading them in separate queries would leave a
/// window where the price came from one moment and the recipe from another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub sale_item_id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub price: Money,
    pub recipe_id: i64,
}

/// One product and how much of it a quantity of some menu item consumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consumed {
    pub product_id: i64,
    pub name: String,
    pub quantity: Milli,
    pub tracks_inventory: bool,
}

const PRODUCT_COLUMNS: &str = "id, code, name, category, base_unit, base_units_per_pack,
     units_per_purchase_pack, low_stock_threshold_milli, tracks_inventory, destination,
     avg_cost_minor, active, content_measure, content_per_unit_milli";

fn read_product(row: &rusqlite::Row<'_>) -> rusqlite::Result<Product> {
    Ok(Product {
        id: row.get(0)?,
        code: row.get(1)?,
        name: row.get(2)?,
        category: row.get(3)?,
        base_unit: row.get(4)?,
        base_units_per_pack: row.get(5)?,
        units_per_purchase_pack: row.get(6)?,
        low_stock_threshold: Milli::from_thousandths(row.get(7)?),
        tracks_inventory: row.get::<_, i64>(8)? == 1,
        destination: row.get(9)?,
        avg_cost: Money::from_minor(row.get(10)?),
        active: row.get::<_, i64>(11)? == 1,
        content_measure: row.get(12)?,
        content_per_unit: Milli::from_thousandths(row.get(13)?),
    })
}

fn validate_product(input: &NewProduct<'_>) -> Result<()> {
    if input.code.trim().is_empty() {
        return super::refuse("a product needs a code");
    }
    if input.name.trim().is_empty() {
        return super::refuse("a product needs a name");
    }
    if input.base_units_per_pack.is_zero() || input.base_units_per_pack.is_negative() {
        return super::refuse("base units per pack must be greater than zero");
    }
    if input.units_per_purchase_pack <= 0 {
        return super::refuse("purchase-pack units must be greater than zero");
    }
    if input.low_stock_threshold.is_negative() {
        return super::refuse("a low-stock threshold cannot be negative");
    }
    match input.content_measure {
        "NONE" => {}
        "ML" | "GRAM" => {
            // Without a size, converting a poured amount would divide by zero.
            if input.content_per_unit.thousandths() <= 0 {
                return super::refuse("a measured item must say how much one holds");
            }
        }
        other => {
            return super::refuse(&format!("{other} is not a measure this till understands"));
        }
    }
    Ok(())
}

/// Convert an amount poured — 30ml — into the fraction of a counted unit a
/// recipe line stores.
///
/// 30ml out of a 750ml bottle is 0.040 of a bottle, so `Milli(40)`. Done here,
/// once, rather than on the screen: the webview computes nothing, and a recipe
/// written in the wrong unit draws the wrong stock every night thereafter.
pub fn measure_to_units(poured: Milli, per_unit: Milli) -> Result<Milli> {
    if per_unit.thousandths() <= 0 {
        return super::refuse("that item does not say how much one holds");
    }
    // Rounded rather than truncated, so a 25ml pour of a 700ml bottle stays the
    // nearest thousandth rather than always reading light.
    let numerator = i128::from(poured.thousandths()) * i128::from(Milli::ONE.thousandths());
    let denominator = i128::from(per_unit.thousandths());
    let units = (numerator + denominator / 2) / denominator;
    let units = i64::try_from(units)
        .map_err(|_| RepoError::Refused("that measure is too large to work with".into()))?;
    if units <= 0 {
        return super::refuse("that measure is too small to draw anything off the shelf");
    }
    Ok(Milli::from_thousandths(units))
}

/// The inverse, for showing a stored recipe line back in the unit it was
/// written in. 0.040 of a 750ml bottle reads as 30ml.
pub fn units_to_measure(units: Milli, per_unit: Milli) -> Milli {
    let poured = i128::from(units.thousandths()) * i128::from(per_unit.thousandths())
        / i128::from(Milli::ONE.thousandths());
    Milli::from_thousandths(i64::try_from(poured).unwrap_or(i64::MAX))
}

pub fn add_product(conn: &Connection, input: &NewProduct<'_>, at: i64) -> Result<i64> {
    validate_product(input)?;
    guarded!(conn.execute(
        "INSERT INTO products
             (code, name, category, base_unit, base_units_per_pack,
              units_per_purchase_pack, low_stock_threshold_milli,
              tracks_inventory, destination, created_at,
              content_measure, content_per_unit_milli)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            input.code.trim(),
            input.name.trim(),
            input.category.trim(),
            input.base_unit,
            input.base_units_per_pack.thousandths(),
            input.units_per_purchase_pack,
            input.low_stock_threshold.thousandths(),
            i64::from(input.tracks_inventory),
            input.destination,
            at,
            input.content_measure,
            input.content_per_unit.thousandths(),
        ],
    ))?;
    Ok(conn.last_insert_rowid())
}

pub fn update_product(
    conn: &Connection,
    id: i64,
    input: &NewProduct<'_>,
    active: bool,
) -> Result<()> {
    validate_product(input)?;
    let changed = guarded!(conn.execute(
        "UPDATE products
            SET code = ?2, name = ?3, category = ?4, base_unit = ?5,
                base_units_per_pack = ?6, units_per_purchase_pack = ?7,
                low_stock_threshold_milli = ?8, tracks_inventory = ?9,
                destination = ?10, active = ?11,
                content_measure = ?12, content_per_unit_milli = ?13
          WHERE id = ?1",
        rusqlite::params![
            id,
            input.code.trim(),
            input.name.trim(),
            input.category.trim(),
            input.base_unit,
            input.base_units_per_pack.thousandths(),
            input.units_per_purchase_pack,
            input.low_stock_threshold.thousandths(),
            i64::from(input.tracks_inventory),
            input.destination,
            i64::from(active),
            input.content_measure,
            input.content_per_unit.thousandths(),
        ],
    ))?;
    if changed == 0 {
        return Err(RepoError::Missing { what: "product" });
    }
    Ok(())
}

fn read_sale_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<SaleItem> {
    Ok(SaleItem {
        id: row.get(0)?,
        code: row.get(1)?,
        name: row.get(2)?,
        category: row.get(3)?,
        active: row.get::<_, i64>(4)? == 1,
    })
}

pub fn sale_item(conn: &Connection, id: i64) -> Result<SaleItem> {
    conn.query_row(
        "SELECT id, code, name, category, active FROM sale_items WHERE id = ?1",
        [id],
        read_sale_item,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what: "sale item" },
        other => RepoError::Sqlite(other),
    })
}

fn validate_sale_item(input: &NewSaleItem<'_>) -> Result<()> {
    if input.code.trim().is_empty() {
        return super::refuse("a sale item needs a code");
    }
    if input.name.trim().is_empty() {
        return super::refuse("a sale item needs a name");
    }
    if input.category.trim().is_empty() {
        return super::refuse("a sale item needs a category");
    }
    Ok(())
}

pub fn add_sale_item(conn: &Connection, input: &NewSaleItem<'_>, at: i64) -> Result<i64> {
    validate_sale_item(input)?;
    guarded!(conn.execute(
        "INSERT INTO sale_items (code, name, category, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            input.code.trim(),
            input.name.trim(),
            input.category.trim(),
            at
        ],
    ))?;
    Ok(conn.last_insert_rowid())
}

/// Record that this menu entry is one shelf item sold one for one.
///
/// The recipe still does the drawing down; this only says the pair may be
/// shown as a single row and priced from it. A partial unique index keeps one
/// product from acquiring two twins.
pub fn link_twin(conn: &Connection, sale_item_id: i64, product_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE sale_items SET from_product_id = ?2 WHERE id = ?1",
        rusqlite::params![sale_item_id, product_id],
    ))?;
    if changed != 1 {
        return super::refuse("that menu entry is not there to be linked to a shelf item");
    }
    Ok(())
}

/// The menu entry that sells this product one for one, if there is one.
///
/// The pair share a name — [`link_twin`]'s caller copies it across — so a
/// rename of one that misses the other leaves the till still offering the old
/// name while the shelf shows the new one.
pub fn twin_of(conn: &Connection, product_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM sale_items WHERE from_product_id = ?1",
            [product_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn update_sale_item(
    conn: &Connection,
    id: i64,
    input: &NewSaleItem<'_>,
    active: bool,
) -> Result<()> {
    validate_sale_item(input)?;
    let changed = guarded!(conn.execute(
        "UPDATE sale_items SET code = ?2, name = ?3, category = ?4, active = ?5 WHERE id = ?1",
        rusqlite::params![
            id,
            input.code.trim(),
            input.name.trim(),
            input.category.trim(),
            i64::from(active),
        ],
    ))?;
    if changed == 0 {
        return Err(RepoError::Missing { what: "sale item" });
    }
    Ok(())
}

/// Take something off the list, or put it back.
///
/// **Nothing in the catalogue is ever deleted** — the schema refuses it
/// outright, because an order line from last year names a product, and a row
/// that vanished would take the meaning of that line with it. Removing is
/// setting `active = 0`: it leaves the till and the shelf, and everything
/// already printed still reads correctly.
pub fn set_product_active(conn: &Connection, id: i64, active: bool) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE products SET active = ?2 WHERE id = ?1",
        rusqlite::params![id, i64::from(active)],
    ))?;
    if changed == 0 {
        return Err(RepoError::Missing { what: "product" });
    }
    Ok(())
}

/// Take a drink off the menu, or put it back. See [`set_product_active`].
pub fn set_sale_item_active(conn: &Connection, id: i64, active: bool) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE sale_items SET active = ?2 WHERE id = ?1",
        rusqlite::params![id, i64::from(active)],
    ))?;
    if changed == 0 {
        return Err(RepoError::Missing { what: "sale item" });
    }
    Ok(())
}

/// The live drink, if any, that still pours this product.
///
/// Deactivating an ingredient silently breaks every drink made from it — the
/// recipe still names it, the till still offers the drink, and the sale fails
/// at the shelf. Naming the drink lets the refusal say what to fix.
pub fn poured_into(conn: &Connection, product_id: i64) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT s.name
               FROM recipe_lines l
               JOIN recipes    r ON r.id = l.recipe_id
               JOIN sale_items s ON s.id = r.sale_item_id
              WHERE l.product_id = ?1
                AND r.effective_to IS NULL
                AND s.active = 1
              LIMIT 1",
            [product_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether a drink is sitting on a tab nobody has settled yet.
///
/// Removing it there would leave the floor holding a line for something the
/// till no longer sells, mid-service.
pub fn on_an_open_tab(conn: &Connection, sale_item_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM order_lines l
               JOIN orders o ON o.id = l.order_id
               JOIN tabs   t ON t.id = o.tab_id
              WHERE l.sale_item_id = ?1 AND t.status = 'OPEN')",
        [sale_item_id],
        |row| row.get(0),
    )?)
}

/// Everything ever on the shelf, removed items included.
///
/// The Catalogue needs this rather than [`products`]: something removed has to
/// stay visible there, or there is no way to bring it back and no way to see
/// that it was taken off. Everywhere else wants the active list.
pub fn every_product(conn: &Connection) -> Result<Vec<Product>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products ORDER BY category, name"
    ))?;
    let rows = stmt.query_map([], read_product)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn products(conn: &Connection) -> Result<Vec<Product>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products WHERE active = 1 ORDER BY category, name"
    ))?;
    let rows = stmt.query_map([], read_product)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn product(conn: &Connection, id: i64) -> Result<Product> {
    conn.query_row(
        &format!("SELECT {PRODUCT_COLUMNS} FROM products WHERE id = ?1"),
        [id],
        read_product,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what: "product" },
        other => RepoError::Sqlite(other),
    })
}

/// Everything sellable right now.
///
/// The joins are inner on purpose. An item with no open price, or no open
/// recipe, is not sellable — showing it on the till would let a cashier ring
/// up something that cannot be priced or cannot be taken off the shelf, and
/// the failure would land mid-round rather than here.
pub fn menu(conn: &Connection) -> Result<Vec<MenuItem>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.code, s.name, s.category, p.price_minor, r.id
           FROM sale_items s
           JOIN prices  p ON p.sale_item_id = s.id AND p.effective_to IS NULL
           JOIN recipes r ON r.sale_item_id = s.id AND r.effective_to IS NULL
          WHERE s.active = 1
          ORDER BY s.category, s.name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MenuItem {
            sale_item_id: row.get(0)?,
            code: row.get(1)?,
            name: row.get(2)?,
            category: row.get(3)?,
            price: Money::from_minor(row.get(4)?),
            recipe_id: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn menu_item(conn: &Connection, sale_item_id: i64) -> Result<MenuItem> {
    conn.query_row(
        "SELECT s.id, s.code, s.name, s.category, p.price_minor, r.id
           FROM sale_items s
           JOIN prices  p ON p.sale_item_id = s.id AND p.effective_to IS NULL
           JOIN recipes r ON r.sale_item_id = s.id AND r.effective_to IS NULL
          WHERE s.id = ?1 AND s.active = 1",
        [sale_item_id],
        |row| {
            Ok(MenuItem {
                sale_item_id: row.get(0)?,
                code: row.get(1)?,
                name: row.get(2)?,
                category: row.get(3)?,
                price: Money::from_minor(row.get(4)?),
                recipe_id: row.get(5)?,
            })
        },
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what: "menu item" },
        other => RepoError::Sqlite(other),
    })
}

/// Expand a quantity of one menu item into the products it consumes.
///
/// Two details carry real weight:
///
/// * **The recipe is taken by id, never looked up from the sale item.** Order
///   lines snapshot `recipe_id`, so a historical order expands through the
///   recipe that was actually poured against, not tonight's.
/// * **Repeated products are summed, not overwritten** (§2.5). A double
///   measure may legitimately be written as two lines naming the same gin, and
///   `recipe_lines` deliberately carries no unique constraint to stop it.
pub fn expand(conn: &Connection, recipe_id: i64, quantity: Milli) -> Result<Vec<Consumed>> {
    if quantity.is_negative() || quantity.is_zero() {
        return super::refuse("nothing can be poured for a quantity of nothing");
    }

    let mut stmt = conn.prepare(
        "SELECT l.product_id, p.name, p.tracks_inventory, l.quantity_milli
           FROM recipe_lines l
           JOIN products p ON p.id = l.product_id
          WHERE l.recipe_id = ?1
          ORDER BY l.id",
    )?;
    let rows = stmt.query_map([recipe_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? == 1,
            row.get::<_, i64>(3)?,
        ))
    })?;

    // BTreeMap rather than HashMap: the order products come back in ends up in
    // the stock ledger and on the shortage message, and a stable order makes
    // both reproducible.
    let mut totals: BTreeMap<i64, (String, i128)> = BTreeMap::new();
    let mut saw_line = false;
    for row in rows {
        let (product_id, name, tracks_inventory, per_item) = row?;
        saw_line = true;
        if !tracks_inventory {
            continue;
        }
        // Keep millionths until every duplicate product line is summed, then
        // round once. Rounding each line independently lets two half-milli
        // contributions become two milli instead of one (§2.5).
        let scaled = i128::from(quantity.thousandths())
            .checked_mul(i128::from(per_item))
            .ok_or_else(|| RepoError::Refused("that quantity is too large to pour".into()))?;
        let entry = totals.entry(product_id).or_insert((name, 0));
        entry.1 = entry
            .1
            .checked_add(scaled)
            .ok_or_else(|| RepoError::Refused("that quantity is too large to pour".into()))?;
    }

    if !saw_line {
        // A recipe with no lines would sell a drink that consumes nothing —
        // free stock forever, and invisible on every variance report.
        return super::refuse("that item has no recipe, so nothing could be taken off the shelf");
    }
    totals
        .into_iter()
        .map(|(product_id, (name, millionths))| {
            let rounded = millionths
                .checked_add(500)
                .ok_or_else(|| RepoError::Refused("that quantity is too large to pour".into()))?
                / 1_000;
            let rounded = i64::try_from(rounded)
                .map_err(|_| RepoError::Refused("that quantity is too large to pour".into()))?;
            Ok(Consumed {
                product_id,
                name,
                quantity: Milli::from_thousandths(rounded),
                tracks_inventory: true,
            })
        })
        .collect()
}

/// Close the open recipe and open the next version (§2.3). Never an edit: an
/// edit would change what last week's orders expanded to.
pub fn revise_recipe(
    conn: &Connection,
    sale_item_id: i64,
    lines: &[(i64, Milli)],
    at: i64,
    by: i64,
) -> Result<i64> {
    if lines.is_empty() {
        return super::refuse("a recipe needs at least one product");
    }
    let previous: Option<i64> = conn
        .query_row(
            "SELECT version FROM recipes WHERE sale_item_id = ?1 AND effective_to IS NULL",
            [sale_item_id],
            |row| row.get(0),
        )
        .optional()?;
    let next_version = previous
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| RepoError::Refused("that recipe has too many versions".into()))?;
    if previous.is_some() {
        guarded!(conn.execute(
            "UPDATE recipes SET effective_to = ?2
              WHERE sale_item_id = ?1 AND effective_to IS NULL",
            rusqlite::params![sale_item_id, at],
        ))?;
    }

    guarded!(conn.execute(
        "INSERT INTO recipes (sale_item_id, version, effective_from, created_by)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![sale_item_id, next_version, at, by],
    ))?;
    let recipe_id = conn.last_insert_rowid();

    for (product_id, quantity) in lines {
        guarded!(conn.execute(
            "INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![recipe_id, product_id, quantity.thousandths()],
        ))?;
    }
    Ok(recipe_id)
}

/// Close the open price and open a new one (§2.4). Tonight's price change
/// cannot restate what a customer was charged last night.
pub fn reprice(conn: &Connection, sale_item_id: i64, price: Money, at: i64, by: i64) -> Result<()> {
    guarded!(conn.execute(
        "UPDATE prices SET effective_to = ?2 WHERE sale_item_id = ?1 AND effective_to IS NULL",
        rusqlite::params![sale_item_id, at],
    ))?;
    guarded!(conn.execute(
        "INSERT INTO prices (sale_item_id, price_minor, effective_from, created_by)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![sale_item_id, price.minor(), at, by],
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, NOW};

    #[test]
    fn a_beer_expands_to_one_bottle() {
        let bar = fixture::bar();
        let item = menu_item(&bar.conn, bar.beer_bottle).unwrap();
        let consumed = expand(&bar.conn, item.recipe_id, Milli::from_units(3)).unwrap();
        assert_eq!(consumed.len(), 1);
        assert_eq!(consumed[0].quantity, Milli::from_units(3));
    }

    #[test]
    fn a_recipe_naming_the_same_product_twice_sums_it() {
        // §2.5. The Gin & Tonic carries gin on two separate lines — a double
        // measure. Overwriting instead of summing would pour half the gin and
        // the variance report would blame the bartender.
        let bar = fixture::bar();
        let item = menu_item(&bar.conn, bar.gin_tonic).unwrap();
        let consumed = expand(&bar.conn, item.recipe_id, Milli::ONE).unwrap();

        let gin = consumed.iter().find(|c| c.product_id == bar.gin).unwrap();
        assert_eq!(gin.quantity, Milli::from_units(2), "two measures, not one");
        let tonic = consumed.iter().find(|c| c.product_id == bar.tonic).unwrap();
        assert_eq!(tonic.quantity, Milli::from_thousandths(500));
    }

    #[test]
    fn expansion_scales_with_the_order_quantity() {
        let bar = fixture::bar();
        let item = menu_item(&bar.conn, bar.gin_tonic).unwrap();
        let consumed = expand(&bar.conn, item.recipe_id, Milli::from_units(4)).unwrap();
        let gin = consumed.iter().find(|c| c.product_id == bar.gin).unwrap();
        assert_eq!(gin.quantity, Milli::from_units(8));
    }

    #[test]
    fn a_fractional_quantity_rounds_half_up() {
        // Half a Gin & Tonic takes half a bottle of tonic: 500 * 500 / 1000
        // is 250 exactly, and the gin is 500 * 2000 / 1000 = 1000.
        let bar = fixture::bar();
        let item = menu_item(&bar.conn, bar.gin_tonic).unwrap();
        let consumed = expand(&bar.conn, item.recipe_id, Milli::from_thousandths(500)).unwrap();
        let tonic = consumed.iter().find(|c| c.product_id == bar.tonic).unwrap();
        assert_eq!(tonic.quantity, Milli::from_thousandths(250));
    }

    #[test]
    fn duplicate_recipe_lines_round_once_after_they_are_aggregated() {
        let bar = fixture::bar();
        let item = fixture::sale_item(&bar.conn, "S-TINY", "Tiny pour", "Test", 100);
        let recipe = fixture::recipe(&bar.conn, item, &[(bar.gin, 1), (bar.gin, 1)]);

        let consumed = expand(&bar.conn, recipe, Milli::from_thousandths(500)).unwrap();

        assert_eq!(consumed.len(), 1);
        assert_eq!(consumed[0].quantity, Milli::from_thousandths(1));
    }

    #[test]
    fn an_item_with_no_open_price_is_not_on_the_menu() {
        // Better to be missing from the till than to be ringable at a price
        // nobody set.
        let bar = fixture::bar();
        bar.conn
            .execute(
                "UPDATE prices SET effective_to = ?1 WHERE sale_item_id = ?2",
                rusqlite::params![NOW, bar.gin_shot],
            )
            .unwrap();
        let codes: Vec<String> = menu(&bar.conn)
            .unwrap()
            .into_iter()
            .map(|m| m.code)
            .collect();
        assert!(!codes.contains(&"S-GIN".to_string()), "got: {codes:?}");
    }

    #[test]
    fn revising_a_recipe_leaves_the_old_version_readable() {
        // §2.3: a historical order must still expand through what was poured
        // at the time, so the old version is closed rather than edited.
        let bar = fixture::bar();
        let before = menu_item(&bar.conn, bar.gin_tonic).unwrap().recipe_id;
        let after = revise_recipe(
            &bar.conn,
            bar.gin_tonic,
            &[(bar.gin, Milli::ONE), (bar.tonic, Milli::ONE)],
            NOW + 1,
            bar.owner,
        )
        .unwrap();

        assert_ne!(before, after);
        let old = expand(&bar.conn, before, Milli::ONE).unwrap();
        let gin = old.iter().find(|c| c.product_id == bar.gin).unwrap();
        assert_eq!(
            gin.quantity,
            Milli::from_units(2),
            "the old version still pours a double"
        );
    }

    #[test]
    fn a_second_open_recipe_version_is_impossible() {
        // Two open versions would make "the current recipe" ambiguous, and
        // expansion would silently pick whichever the query returned first.
        let bar = fixture::bar();
        let clash = bar.conn.execute(
            "INSERT INTO recipes (sale_item_id, version, effective_from) VALUES (?1, 9, ?2)",
            rusqlite::params![bar.beer_bottle, NOW],
        );
        assert!(clash.is_err());
    }

    #[test]
    fn zero_quantity_is_refused_rather_than_silently_pouring_nothing() {
        let bar = fixture::bar();
        let item = menu_item(&bar.conn, bar.beer_bottle).unwrap();
        assert!(expand(&bar.conn, item.recipe_id, Milli::ZERO).is_err());
    }

    #[test]
    fn expansion_drops_products_that_do_not_track_inventory() {
        let bar = fixture::bar();
        bar.conn
            .execute(
                "UPDATE products SET tracks_inventory = 0 WHERE id = ?1",
                [bar.beer],
            )
            .unwrap();
        let item = menu_item(&bar.conn, bar.beer_bottle).unwrap();

        assert!(expand(&bar.conn, item.recipe_id, Milli::ONE)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recipe_version_overflow_is_refused_without_panicking() {
        let bar = fixture::bar();
        bar.conn
            .execute(
                "UPDATE recipes SET effective_to = ?2
                  WHERE sale_item_id = ?1 AND effective_to IS NULL",
                rusqlite::params![bar.beer_bottle, NOW + 1],
            )
            .unwrap();
        bar.conn
            .execute(
                "INSERT INTO recipes (sale_item_id, version, effective_from, created_by)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![bar.beer_bottle, i64::MAX, NOW + 1, bar.owner],
            )
            .unwrap();

        let result = revise_recipe(
            &bar.conn,
            bar.beer_bottle,
            &[(bar.beer, Milli::ONE)],
            NOW + 2,
            bar.owner,
        );
        assert!(result.is_err());
    }

    #[test]
    fn repricing_closes_the_old_price_instead_of_editing_it() {
        let bar = fixture::bar();
        reprice(
            &bar.conn,
            bar.beer_bottle,
            Money::from_minor(6_000),
            NOW + 1,
            bar.owner,
        )
        .unwrap();
        assert_eq!(
            menu_item(&bar.conn, bar.beer_bottle).unwrap().price.minor(),
            6_000
        );

        let history: i64 = bar
            .conn
            .query_row(
                "SELECT COUNT(*) FROM prices WHERE sale_item_id = ?1",
                [bar.beer_bottle],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history, 2, "the old price is kept, not overwritten");
    }
}
