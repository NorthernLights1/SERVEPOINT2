//! Deliveries (§8.1) — what arrived, when, and what it cost.
//!
//! # Why there is no supplier in this module
//!
//! `purchases.supplier_id` is `NOT NULL REFERENCES suppliers(id)` and cannot
//! be dropped: SQLite would need the table rebuilt, and `stock_movements`
//! points at it, so the rebuild is exactly the kind these migrations cannot
//! do. Tracking *who sold* the stock is out of scope anyway — a bar wants to
//! know what a bottle cost, not to run a purchase-order system — so every
//! delivery is booked against one standing row that no screen ever shows.
//! The column stays honest, and if suppliers ever become in scope it is
//! already there.
//!
//! # The batch is the row id
//!
//! Nobody types a batch number. A delivery is one `purchases` row, and its id
//! *is* the batch: `received_at` says when, the lines say what and at what
//! price. `invoice_ref` stays NULL, and NULLs do not collide in a unique
//! index, so a venue that never keeps paperwork can receive stock forever.

use rusqlite::{Connection, OptionalExtension};

use super::{guarded, Result};
use crate::{Milli, Money};

/// The one supplier every delivery is booked against. Never shown to anybody.
const HOUSE: &str = "Deliveries";

/// Find the standing supplier row, creating it the first time stock arrives.
pub fn house(conn: &Connection, at: i64) -> Result<i64> {
    let normalized = HOUSE.to_lowercase();
    let found: Option<i64> = conn
        .query_row(
            "SELECT id FROM suppliers WHERE normalized_name = ?1",
            [&normalized],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    guarded!(conn.execute(
        "INSERT INTO suppliers (name, normalized_name, active, created_at)
         VALUES (?1, ?2, 1, ?3)",
        rusqlite::params![HOUSE, normalized, at],
    ))?;
    Ok(conn.last_insert_rowid())
}

/// Start a delivery. The caller adds lines, posts stock, then commits.
pub fn open(
    conn: &Connection,
    supplier_id: i64,
    total_cost: Money,
    shift_id: Option<i64>,
    at: i64,
    by: i64,
) -> Result<i64> {
    guarded!(conn.execute(
        "INSERT INTO purchases
             (supplier_id, invoice_ref, received_at, shift_id, total_cost_minor,
              created_by, created_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?2)",
        rusqlite::params![supplier_id, at, shift_id, total_cost.minor(), by],
    ))?;
    Ok(conn.last_insert_rowid())
}

/// The per-base-unit rate the schema expects for a line.
///
/// Derived with the same arithmetic as the `purchase_lines` CHECK, so a line
/// built from an exact invoice total can never be refused for disagreeing
/// with itself. The total stays authoritative — this rate only values the
/// movement, and §8.2 re-averages from the total, never from here.
fn unit_rate(line_cost: Money, quantity: Milli) -> Result<Money> {
    let thousandths = quantity.thousandths();
    if thousandths <= 0 {
        return super::refuse("a delivery line must be for more than nothing");
    }
    let scaled = i128::from(line_cost.minor())
        .checked_mul(1_000)
        .and_then(|scaled| scaled.checked_add(i128::from(thousandths) / 2))
        .ok_or_else(|| super::RepoError::Refused("that delivery costs too much".into()))?;
    i64::try_from(scaled / i128::from(thousandths))
        .map(Money::from_minor)
        .map_err(|_| super::RepoError::Refused("that delivery costs too much".into()))
}

/// Record what arrived, and what the whole line cost.
pub fn add_line(
    conn: &Connection,
    purchase_id: i64,
    product_id: i64,
    quantity: Milli,
    line_cost: Money,
) -> Result<Money> {
    let unit = unit_rate(line_cost, quantity)?;
    guarded!(conn.execute(
        "INSERT INTO purchase_lines
             (purchase_id, product_id, quantity_milli, unit_cost_minor, line_cost_minor)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            purchase_id,
            product_id,
            quantity.thousandths(),
            unit.minor(),
            line_cost.minor(),
        ],
    ))?;
    Ok(unit)
}

/// **§8.2.** Re-average a product's cost once a delivery has landed.
///
/// Computed from the exact invoice total rather than the rounded per-unit
/// rate, so rounding cannot accumulate into the cost of goods. Stock already
/// on the shelf keeps what it cost; the new average is what the whole shelf
/// is now worth divided by how much of it there is.
///
/// A shelf that is somehow negative counts as empty — a delivery must not
/// inherit a cost from stock that is not there.
pub fn reaverage(
    conn: &Connection,
    product_id: i64,
    arriving: Milli,
    line_cost: Money,
) -> Result<Money> {
    let before = super::stock::on_hand(conn, product_id)?
        .thousandths()
        .max(0);
    let held: i64 = conn.query_row(
        "SELECT avg_cost_minor FROM products WHERE id = ?1",
        [product_id],
        |row| row.get(0),
    )?;

    let too_big = || super::RepoError::Refused("that shelf is worth too much to value".into());
    let worth = i128::from(before)
        .checked_mul(i128::from(held))
        .and_then(|standing| {
            i128::from(line_cost.minor())
                .checked_mul(1_000)
                .and_then(|arrived| standing.checked_add(arrived))
        })
        .ok_or_else(too_big)?;

    let total = i128::from(before) + i128::from(arriving.thousandths());
    if total <= 0 {
        return super::refuse("a delivery must leave something on the shelf");
    }
    let averaged = i64::try_from((worth + total / 2) / total).map_err(|_| too_big())?;

    guarded!(conn.execute(
        "UPDATE products SET avg_cost_minor = ?2 WHERE id = ?1",
        rusqlite::params![product_id, averaged],
    ))?;
    Ok(Money::from_minor(averaged))
}

/// One delivery line, for the history on the Inventory screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub purchase_id: i64,
    pub product_id: i64,
    pub name: String,
    pub base_unit: String,
    pub quantity: Milli,
    pub line_cost: Money,
    pub received_at: i64,
}

/// The most recent deliveries, newest first.
///
/// Bounded by the caller because the shelf history grows without limit and
/// nobody scrolls a year of crates looking for last Tuesday.
pub fn recent(conn: &Connection, limit: i64) -> Result<Vec<Received>> {
    let mut statement = conn.prepare(
        "SELECT l.purchase_id, l.product_id, p.name, p.base_unit,
                l.quantity_milli, l.line_cost_minor, u.received_at
           FROM purchase_lines l
           JOIN purchases u ON u.id = l.purchase_id
           JOIN products  p ON p.id = l.product_id
          ORDER BY u.received_at DESC, l.id DESC
          LIMIT ?1",
    )?;
    let rows = statement.query_map([limit], |row| {
        Ok(Received {
            purchase_id: row.get(0)?,
            product_id: row.get(1)?,
            name: row.get(2)?,
            base_unit: row.get(3)?,
            quantity: Milli::from_thousandths(row.get(4)?),
            line_cost: Money::from_minor(row.get(5)?),
            received_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, NOW};
    use crate::repo::stock;

    /// Receive stock the way the command layer does, so these tests exercise
    /// the order the schema's triggers actually require.
    fn receive(bar: &fixture::Bar, product: i64, quantity: Milli, cost: Money) -> i64 {
        let supplier = house(&bar.conn, NOW).unwrap();
        let delivery = open(&bar.conn, supplier, cost, None, NOW, bar.owner).unwrap();
        let unit = add_line(&bar.conn, delivery, product, quantity, cost).unwrap();
        // Re-average BEFORE the movement lands: the old average belongs to the
        // shelf as it stood, and posting first would blend the crate into itself.
        reaverage(&bar.conn, product, quantity, cost).unwrap();
        stock::post(
            &bar.conn,
            &stock::Movement::new(product, stock::Kind::Purchase, quantity, NOW, bar.owner)
                .for_purchase(delivery)
                .costing(unit),
        )
        .unwrap();
        delivery
    }

    fn cost_of(bar: &fixture::Bar, product: i64) -> i64 {
        bar.conn
            .query_row(
                "SELECT avg_cost_minor FROM products WHERE id = ?1",
                [product],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn the_house_supplier_is_created_once_and_reused() {
        let bar = fixture::bar();
        let first = house(&bar.conn, NOW).unwrap();
        let second = house(&bar.conn, NOW + 86_400_000).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_delivery_line_always_agrees_with_its_own_invoice_total() {
        // 7 units for 100.00 does not divide evenly. The schema refuses a line
        // whose unit rate and total disagree, so the rate must be derived from
        // the total rather than typed alongside it.
        let bar = fixture::bar();
        receive(
            &bar,
            bar.beer,
            Milli::from_units(7),
            Money::from_minor(10_000),
        );
        assert_eq!(
            stock::on_hand(&bar.conn, bar.beer).unwrap(),
            Milli::from_units(7)
        );
    }

    #[test]
    fn the_average_cost_blends_the_old_shelf_with_the_new_crate() {
        // 4 on the shelf at 100.00, then 4 more for 800.00 — an average of 150.00.
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 4_000, bar.owner);
        receive(
            &bar,
            bar.beer,
            Milli::from_units(4),
            Money::from_minor(80_000),
        );
        assert_eq!(cost_of(&bar, bar.beer), 15_000);
    }

    #[test]
    fn the_first_delivery_sets_the_cost_outright() {
        // Nothing on the shelf to blend against, so the crate's own rate stands.
        let bar = fixture::bar();
        receive(
            &bar,
            bar.gin,
            Milli::from_units(10),
            Money::from_minor(50_000),
        );
        assert_eq!(cost_of(&bar, bar.gin), 5_000);
    }

    #[test]
    fn a_delivery_may_not_post_the_same_product_twice() {
        // §8.1: product-distinct within one delivery, so stock cannot double.
        let bar = fixture::bar();
        let supplier = house(&bar.conn, NOW).unwrap();
        let delivery = open(
            &bar.conn,
            supplier,
            Money::from_minor(10_000),
            None,
            NOW,
            bar.owner,
        )
        .unwrap();
        add_line(
            &bar.conn,
            delivery,
            bar.beer,
            Milli::from_units(5),
            Money::from_minor(10_000),
        )
        .unwrap();
        let twice = add_line(
            &bar.conn,
            delivery,
            bar.beer,
            Milli::from_units(5),
            Money::from_minor(10_000),
        );
        assert!(twice.is_err());
    }

    #[test]
    fn a_delivery_of_nothing_is_refused() {
        let bar = fixture::bar();
        assert!(unit_rate(Money::from_minor(10_000), Milli::ZERO).is_err());
        let supplier = house(&bar.conn, NOW).unwrap();
        let delivery = open(&bar.conn, supplier, Money::ZERO, None, NOW, bar.owner).unwrap();
        assert!(add_line(&bar.conn, delivery, bar.beer, Milli::ZERO, Money::ZERO).is_err());
    }

    #[test]
    fn a_delivery_is_never_edited_or_removed() {
        let bar = fixture::bar();
        receive(
            &bar,
            bar.beer,
            Milli::from_units(5),
            Money::from_minor(10_000),
        );
        let edit = bar
            .conn
            .execute("UPDATE purchases SET total_cost_minor = 1", []);
        let wipe = bar.conn.execute("DELETE FROM purchase_lines", []);
        assert!(edit.is_err() && wipe.is_err());
    }

    #[test]
    fn recent_deliveries_come_back_newest_first() {
        let bar = fixture::bar();
        receive(
            &bar,
            bar.beer,
            Milli::from_units(5),
            Money::from_minor(10_000),
        );
        let supplier = house(&bar.conn, NOW).unwrap();
        let later = open(
            &bar.conn,
            supplier,
            Money::from_minor(20_000),
            None,
            NOW + 3_600_000,
            bar.owner,
        )
        .unwrap();
        add_line(
            &bar.conn,
            later,
            bar.gin,
            Milli::from_units(24),
            Money::from_minor(20_000),
        )
        .unwrap();

        let history = recent(&bar.conn, 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].product_id, bar.gin);
        assert_eq!(history[1].product_id, bar.beer);
    }
}
