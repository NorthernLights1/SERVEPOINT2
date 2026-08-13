//! The movement ledger (§3).
//!
//! **STOCK ON HAND IS NEVER STORED.** Every figure in this module is a `SUM()`
//! over `stock_movements`. There is no cached quantity to drift, because there
//! is no cached quantity — which is the single most important thing about this
//! file and the change most likely to be "optimised" away by someone later.
//!
//! # D9: insufficient stock always blocks the sale
//!
//! [`shortages`] is the check, and there is deliberately no setting that turns
//! it off. The old build let a sale drive stock negative and the count was
//! never trustworthy again. The relief valve is a `STOCK_CORRECTION` — moving
//! the count in the open, with a reason, on its own block of the shift report
//! — never waiving the check.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::{guarded, Result};
use crate::{money::MoneyError, Milli, Money};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sale,
    Return,
    Purchase,
    Adjustment,
    Damage,
    Loss,
    StockCorrection,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Sale => "SALE",
            Kind::Return => "RETURN",
            Kind::Purchase => "PURCHASE",
            Kind::Adjustment => "ADJUSTMENT",
            Kind::Damage => "DAMAGE",
            Kind::Loss => "LOSS",
            Kind::StockCorrection => "STOCK_CORRECTION",
        }
    }
}

/// One row of the ledger, before it is written.
///
/// The sign lives in the *quantity*, and the schema checks it against the
/// type: a SALE that increased stock, or a PURCHASE that decreased it, is
/// refused outright rather than becoming a silent inversion no report surfaces.
#[derive(Clone, Debug)]
pub struct Movement<'a> {
    pub product_id: i64,
    pub kind: Kind,
    pub quantity: Milli,
    pub unit_cost: Money,
    pub reason: &'a str,
    pub order_id: Option<i64>,
    pub purchase_id: Option<i64>,
    pub stock_count_id: Option<i64>,
    /// NULLABLE on purpose: deliveries and stock counts happen while the club
    /// is shut, which is exactly when stock actually arrives.
    pub shift_id: Option<i64>,
    pub at: i64,
    pub by: i64,
}

impl<'a> Movement<'a> {
    pub fn new(product_id: i64, kind: Kind, quantity: Milli, at: i64, by: i64) -> Self {
        Self {
            product_id,
            kind,
            quantity,
            unit_cost: Money::ZERO,
            reason: "",
            order_id: None,
            purchase_id: None,
            stock_count_id: None,
            shift_id: None,
            at,
            by,
        }
    }

    pub fn for_order(mut self, order_id: i64) -> Self {
        self.order_id = Some(order_id);
        self
    }

    pub fn for_purchase(mut self, purchase_id: i64) -> Self {
        self.purchase_id = Some(purchase_id);
        self
    }

    pub fn during(mut self, shift_id: i64) -> Self {
        self.shift_id = Some(shift_id);
        self
    }

    pub fn because(mut self, reason: &'a str) -> Self {
        self.reason = reason;
        self
    }

    pub fn costing(mut self, unit_cost: Money) -> Self {
        self.unit_cost = unit_cost;
        self
    }
}

/// Write one movement. Nothing corrects a movement afterwards — the table is
/// append-only, and a mistake is answered with another movement.
pub fn post(conn: &Connection, movement: &Movement<'_>) -> Result<i64> {
    guarded!(conn.execute(
        "INSERT INTO stock_movements
             (product_id, movement_type, quantity_milli, unit_cost_minor, reason,
              order_id, purchase_id, stock_count_id, shift_id, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            movement.product_id,
            movement.kind.as_str(),
            movement.quantity.thousandths(),
            movement.unit_cost.minor(),
            movement.reason,
            movement.order_id,
            movement.purchase_id,
            movement.stock_count_id,
            movement.shift_id,
            movement.at,
            movement.by,
        ],
    ))?;
    Ok(conn.last_insert_rowid())
}

/// What is on the shelf, summed from the ledger.
pub fn on_hand(conn: &Connection, product_id: i64) -> Result<Milli> {
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(quantity_milli), 0) FROM stock_movements WHERE product_id = ?1",
        [product_id],
        |row| row.get(0),
    )?;
    Ok(Milli::from_thousandths(total))
}

/// Everything on the shelf, for the Inventory screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    pub product_id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub base_unit: String,
    pub on_hand: Milli,
    pub threshold: Milli,
    pub tracks_inventory: bool,
    pub avg_cost: Money,
}

impl Level {
    /// A product is low when it is at or under its own threshold. Per product,
    /// never global: one number across beer and premium spirits is meaningless.
    pub fn is_low(&self) -> bool {
        self.tracks_inventory && self.on_hand.thousandths() <= self.threshold.thousandths()
    }

    /// What the shelf is worth at the weighted average cost.
    pub fn value(&self) -> std::result::Result<Money, MoneyError> {
        let scaled = i128::from(self.on_hand.thousandths())
            .checked_mul(i128::from(self.avg_cost.minor()))
            .ok_or(MoneyError::Overflow)?;
        let rounded_magnitude = scaled
            .unsigned_abs()
            .checked_add(500)
            .ok_or(MoneyError::Overflow)?
            / 1_000;
        let rounded = i128::try_from(rounded_magnitude).map_err(|_| MoneyError::Overflow)?;
        let signed = if scaled.is_negative() {
            rounded.checked_neg().ok_or(MoneyError::Overflow)?
        } else {
            rounded
        };
        let minor = i64::try_from(signed).map_err(|_| MoneyError::Overflow)?;

        Ok(Money::from_minor(minor))
    }
}

/// One row per active product, with its balance. A single grouped query rather
/// than a sum per product — the Inventory screen would otherwise issue one
/// statement per line, which is the classic N+1 and is felt immediately on a
/// catalogue of any size.
pub fn levels(conn: &Connection) -> Result<Vec<Level>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.code, p.name, p.category, p.base_unit,
                COALESCE(m.total, 0), p.low_stock_threshold_milli,
                p.tracks_inventory, p.avg_cost_minor
           FROM products p
           LEFT JOIN (SELECT product_id, SUM(quantity_milli) AS total
                        FROM stock_movements GROUP BY product_id) m
             ON m.product_id = p.id
          WHERE p.active = 1
          ORDER BY p.category, p.name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Level {
            product_id: row.get(0)?,
            code: row.get(1)?,
            name: row.get(2)?,
            category: row.get(3)?,
            base_unit: row.get(4)?,
            on_hand: Milli::from_thousandths(row.get(5)?),
            threshold: Milli::from_thousandths(row.get(6)?),
            tracks_inventory: row.get::<_, i64>(7)? == 1,
            avg_cost: Money::from_minor(row.get(8)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// What is running out, worst first — the Overview's low-stock block.
pub fn low(conn: &Connection) -> Result<Vec<Level>> {
    let mut found: Vec<Level> = levels(conn)?.into_iter().filter(Level::is_low).collect();
    // Deepest into the threshold first: a product already at zero matters more
    // than one a bottle away from it.
    found.sort_by_key(|level| {
        i128::from(level.on_hand.thousandths()) - i128::from(level.threshold.thousandths())
    });
    Ok(found)
}

/// A product there is not enough of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortage {
    pub product_id: i64,
    pub name: String,
    pub wanted: Milli,
    pub available: Milli,
}

/// **D9.** Check a whole order's worth of consumption against the shelf.
///
/// Takes the expansion already summed per product, because a check per line
/// would pass twice for two lines that each want the last bottle. Returns
/// every shortage rather than the first: a cashier told "not enough gin" who
/// then discovers there is also no tonic has been made to fail twice for one
/// mistake.
///
/// Products that do not track inventory are skipped — nobody counts them, so
/// there is no number to be short of.
pub fn shortages(conn: &Connection, wanted: &[(i64, Milli)]) -> Result<Vec<Shortage>> {
    let mut totals = BTreeMap::<i64, Milli>::new();
    for (product_id, quantity) in wanted {
        if quantity.is_zero() || quantity.is_negative() {
            return super::refuse("stock requirements must be greater than zero");
        }
        let entry = totals.entry(*product_id).or_default();
        *entry = entry
            .checked_add(*quantity)
            .map_err(|_| super::RepoError::Refused("stock requirements are too large".into()))?;
    }

    let mut found = Vec::new();
    for (product_id, quantity) in totals {
        let (name, tracks): (String, bool) = conn.query_row(
            "SELECT name, tracks_inventory FROM products WHERE id = ?1",
            [product_id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? == 1)),
        )?;
        if !tracks {
            continue;
        }
        let available = on_hand(conn, product_id)?;
        if available.thousandths() < quantity.thousandths() {
            found.push(Shortage {
                product_id,
                name,
                wanted: quantity,
                available,
            });
        }
    }
    Ok(found)
}

/// Turn shortages into the sentence shown at the till.
pub fn shortage_message(shortages: &[Shortage]) -> String {
    let listed: Vec<String> = shortages
        .iter()
        .map(|short| {
            format!(
                "{} (need {}, have {})",
                short.name, short.wanted, short.available
            )
        })
        .collect();
    format!("Not enough stock: {}", listed.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, NOW};

    #[test]
    fn stock_on_hand_is_the_sum_of_the_ledger() {
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 24_000, bar.owner);
        post(
            &bar.conn,
            &Movement::new(
                bar.beer,
                Kind::Damage,
                Milli::from_units(-2),
                NOW,
                bar.owner,
            )
            .because("crate dropped"),
        )
        .unwrap();
        assert_eq!(on_hand(&bar.conn, bar.beer).unwrap(), Milli::from_units(22));
    }

    #[test]
    fn a_movement_of_nothing_is_not_an_event() {
        let bar = fixture::bar();
        let refused = post(
            &bar.conn,
            &Movement::new(bar.beer, Kind::StockCorrection, Milli::ZERO, NOW, bar.owner)
                .because("recount"),
        );
        assert!(refused.is_err());
    }

    #[test]
    fn a_sale_that_increased_stock_is_refused() {
        // The sign is part of the type, not a convention the caller remembers.
        let bar = fixture::bar();
        let refused = post(
            &bar.conn,
            &Movement::new(bar.beer, Kind::Sale, Milli::from_units(5), NOW, bar.owner).for_order(1),
        );
        assert!(refused.is_err());
    }

    #[test]
    fn damage_and_loss_must_say_why() {
        // Each is somebody asserting a fact the ledger cannot derive.
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 10_000, bar.owner);
        let refused = post(
            &bar.conn,
            &Movement::new(bar.beer, Kind::Loss, Milli::from_units(-1), NOW, bar.owner),
        );
        assert!(refused.is_err());
    }

    #[test]
    fn a_movement_is_never_edited_or_removed() {
        // §3.2: if a row here could change, nothing computed from it could be
        // trusted, and "never stored" would buy nothing.
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 10_000, bar.owner);
        let edit = bar
            .conn
            .execute("UPDATE stock_movements SET quantity_milli = 99", []);
        let wipe = bar.conn.execute("DELETE FROM stock_movements", []);
        assert!(edit.is_err() && wipe.is_err());
    }

    #[test]
    fn a_short_shelf_reports_every_shortage_not_just_the_first() {
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.gin, 1_000, bar.owner);
        // No tonic at all.
        let short = shortages(
            &bar.conn,
            &[
                (bar.gin, Milli::from_units(2)),
                (bar.tonic, Milli::from_thousandths(500)),
            ],
        )
        .unwrap();
        assert_eq!(short.len(), 2, "got: {short:?}");
    }

    #[test]
    fn exactly_enough_stock_is_not_a_shortage() {
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.gin, 2_000, bar.owner);
        assert!(shortages(&bar.conn, &[(bar.gin, Milli::from_units(2))])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn availability_aggregates_duplicate_product_requirements() {
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 1_000, bar.owner);

        let short = shortages(
            &bar.conn,
            &[
                (bar.beer, Milli::from_thousandths(750)),
                (bar.beer, Milli::from_thousandths(750)),
            ],
        )
        .unwrap();

        assert_eq!(short.len(), 1);
        assert_eq!(short[0].wanted, Milli::from_thousandths(1_500));
        assert_eq!(short[0].available, Milli::ONE);
    }

    #[test]
    fn availability_refuses_an_aggregate_that_overflows() {
        let bar = fixture::bar();
        let result = shortages(
            &bar.conn,
            &[
                (bar.beer, Milli::from_thousandths(i64::MAX)),
                (bar.beer, Milli::ONE),
            ],
        );

        assert!(result.is_err());
    }

    #[test]
    fn a_product_nobody_counts_can_never_be_short() {
        let bar = fixture::bar();
        bar.conn
            .execute(
                "UPDATE products SET tracks_inventory = 0 WHERE id = ?1",
                [bar.beer],
            )
            .unwrap();
        assert!(shortages(&bar.conn, &[(bar.beer, Milli::from_units(500))])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn low_stock_lists_the_worst_first() {
        let bar = fixture::bar();
        // Thresholds are 3 units each in the fixture.
        fixture::stock_up(&bar.conn, bar.beer, 3_000, bar.owner); // at the line
        fixture::stock_up(&bar.conn, bar.gin, 1_000, bar.owner); // two under
        fixture::stock_up(&bar.conn, bar.tonic, 50_000, bar.owner); // fine

        let running_out = low(&bar.conn).unwrap();
        let names: Vec<&str> = running_out.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Gin", "Beer"]);
    }

    #[test]
    fn low_stock_priority_does_not_overflow_at_extreme_ledger_values() {
        let bar = fixture::bar();
        bar.conn
            .execute(
                "UPDATE products SET low_stock_threshold_milli = ?2 WHERE id = ?1",
                rusqlite::params![bar.gin, i64::MAX],
            )
            .unwrap();
        post(
            &bar.conn,
            &Movement::new(
                bar.gin,
                Kind::Loss,
                Milli::from_thousandths(i64::MIN),
                NOW,
                bar.owner,
            )
            .because("overflow boundary fixture"),
        )
        .unwrap();

        let running_out = low(&bar.conn).unwrap();
        assert_eq!(running_out[0].product_id, bar.gin);
    }

    #[test]
    fn a_shelf_is_valued_at_the_weighted_average() {
        // 4 bottles at a 100.00 average is 400.00.
        let bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 4_000, bar.owner);
        let level = levels(&bar.conn)
            .unwrap()
            .into_iter()
            .find(|l| l.product_id == bar.beer)
            .unwrap();
        assert_eq!(level.value().unwrap().minor(), 40_000);
    }

    #[test]
    fn shelf_value_reports_overflow_instead_of_saturating() {
        let level = Level {
            product_id: 1,
            code: "HUGE".into(),
            name: "Impossible shelf".into(),
            category: "Test".into(),
            base_unit: "unit".into(),
            on_hand: Milli::from_thousandths(i64::MAX),
            threshold: Milli::ZERO,
            tracks_inventory: true,
            avg_cost: Money::from_minor(i64::MAX),
        };

        assert_eq!(level.value(), Err(crate::money::MoneyError::Overflow));
    }

    #[test]
    fn the_shortage_message_names_what_is_missing() {
        let short = vec![Shortage {
            product_id: 1,
            name: "Gin".into(),
            wanted: Milli::from_units(2),
            available: Milli::ONE,
        }];
        let message = shortage_message(&short);
        assert!(message.contains("Gin"), "got: {message}");
        assert!(message.contains("need 2"), "got: {message}");
    }
}
