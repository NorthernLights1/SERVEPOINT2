//! Products, menu items, and the recipe that joins them (§2).
//!
//! WHAT YOU COUNT IS NOT WHAT YOU SELL. A shelf holds *Gin*; a menu offers a
//! shot of gin, a bottle of gin and a Gin & Tonic. The recipe is the only
//! bridge, and **every** menu item has one — a beer's recipe is a single line
//! consuming one bottle. That uniformity is why cocktails and shots were never
//! structural work: nothing below this line has ever heard of either.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::{guarded, RepoError, Result};
use crate::{Milli, Money};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Product {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub base_unit: String,
    pub base_units_per_pack: i64,
    pub low_stock_threshold: Milli,
    pub tracks_inventory: bool,
    pub avg_cost: Money,
    pub active: bool,
}

/// A line on the menu: the item, what it costs tonight, and the recipe version
/// currently in force. All three are read together because an order line
/// snapshots all three, and reading them in separate queries would leave a
/// window where the price came from one moment and the recipe from another.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
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
     low_stock_threshold_milli, tracks_inventory, avg_cost_minor, active";

fn read_product(row: &rusqlite::Row<'_>) -> rusqlite::Result<Product> {
    Ok(Product {
        id: row.get(0)?,
        code: row.get(1)?,
        name: row.get(2)?,
        category: row.get(3)?,
        base_unit: row.get(4)?,
        base_units_per_pack: row.get(5)?,
        low_stock_threshold: Milli::from_thousandths(row.get(6)?),
        tracks_inventory: row.get::<_, i64>(7)? == 1,
        avg_cost: Money::from_minor(row.get(8)?),
        active: row.get::<_, i64>(9)? == 1,
    })
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
    let mut totals: BTreeMap<i64, Consumed> = BTreeMap::new();
    for row in rows {
        let (product_id, name, tracks_inventory, per_item) = row?;
        // per_item is base units per ONE menu item, in thousandths; quantity is
        // menu items, in thousandths. Their product is in millionths, so it is
        // brought back to thousandths, rounded half up (§1.1).
        let scaled = quantity
            .thousandths()
            .checked_mul(per_item)
            .and_then(|product| product.checked_add(500))
            .map(|rounded| rounded / 1_000)
            .ok_or_else(|| RepoError::Refused("that quantity is too large to pour".into()))?;

        let entry = totals.entry(product_id).or_insert(Consumed {
            product_id,
            name,
            quantity: Milli::ZERO,
            tracks_inventory,
        });
        entry.quantity = entry
            .quantity
            .checked_add(Milli::from_thousandths(scaled))
            .map_err(|_| RepoError::Refused("that quantity is too large to pour".into()))?;
    }

    if totals.is_empty() {
        // A recipe with no lines would sell a drink that consumes nothing —
        // free stock forever, and invisible on every variance report.
        return super::refuse("that item has no recipe, so nothing could be taken off the shelf");
    }
    Ok(totals.into_values().collect())
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
        .ok();
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
        rusqlite::params![sale_item_id, previous.unwrap_or(0) + 1, at, by],
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
        let codes: Vec<String> = menu(&bar.conn).unwrap().into_iter().map(|m| m.code).collect();
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
        assert_eq!(gin.quantity, Milli::from_units(2), "the old version still pours a double");
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
    fn repricing_closes_the_old_price_instead_of_editing_it() {
        let bar = fixture::bar();
        reprice(&bar.conn, bar.beer_bottle, Money::from_minor(6_000), NOW + 1, bar.owner).unwrap();
        assert_eq!(menu_item(&bar.conn, bar.beer_bottle).unwrap().price.minor(), 6_000);

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
