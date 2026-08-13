//! Receiving stock (§8.1) — the delivery that keeps the shelf stocked.
//!
//! Restocking is not a one-off. This is the ordinary thing that happens every
//! time the beer runs low, and it may happen forever. It replaced a once-only
//! "count in" that recorded what was already on the shelf the day the system
//! arrived and refused any product with history — a first delivery against an
//! empty shelf does that job identically, so keeping both would have left two
//! ways to stock a shelf, on two screens.
//!
//! # No open shift is required
//!
//! §8.1, and the reason `stock_movements.shift_id` is nullable: deliveries
//! arrive during the day, while the club is shut. Refusing one because nobody
//! is trading pushes the venue into entering it wrong later. If a shift does
//! happen to be open the delivery is stamped with it, so the night's report
//! can account for stock that appeared mid-service.
//!
//! # Who may receive
//!
//! Anybody signed in, not the owner alone. Whoever opens the door at four in
//! the afternoon is who signs for the crate, and an owner-only rule here
//! produces exactly the late, wrong entry the nullable shift guards against.
//! Every delivery records who took it, which is the control that actually
//! holds.

use chrono::{Local, TimeZone, Utc};

use crate::commands::{require_session, CommandError};
use crate::floor::{self, InventoryView};
use crate::ledger::{self, Event};
use crate::repo::{catalogue, purchases, shifts, stock};
use crate::settings::Settings;
use crate::state::AppState;
use crate::{calendar, Milli, Money};

type Result<T> = std::result::Result<T, CommandError>;

/// How many deliveries the Inventory screen shows. Enough to answer "what did
/// we pay for the last few crates" without turning the shelf into a ledger.
const HISTORY: i64 = 20;

/// What the frontend sends when a crate lands.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryForm {
    pub product_id: i64,
    /// In the product's base units, as thousandths — the unit the ledger keeps.
    pub quantity_milli: i64,
    /// What the whole delivery cost, exactly as written on the receipt. The
    /// per-unit rate is derived from it, never typed beside it, so the two can
    /// never disagree.
    pub total_cost: String,
}

/// One past delivery, for the history on the Inventory screen.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryLine {
    /// The batch. Nobody types it — it is the row's own id.
    pub batch: i64,
    pub name: String,
    pub quantity: String,
    pub unit: String,
    pub cost: String,
    pub received: String,
}

/// The deliveries behind what is currently on the shelf.
pub fn history(conn: &rusqlite::Connection, settings: &Settings) -> Result<Vec<DeliveryLine>> {
    Ok(purchases::recent(conn, HISTORY)?
        .into_iter()
        .map(|line| DeliveryLine {
            batch: line.purchase_id,
            name: line.name,
            quantity: line.quantity.to_display(),
            unit: line.base_unit,
            cost: settings.format_money(line.line_cost),
            received: Local
                .timestamp_millis_opt(line.received_at)
                .single()
                .map(|local| calendar::describe(local.date_naive()))
                .unwrap_or_default(),
        })
        .collect())
}

/// Book a delivery: one batch, one line, one movement, one new average.
///
/// All of it or none of it. A batch recorded without the stock arriving would
/// show a cost against a shelf that never grew, and stock arriving without its
/// batch is refused outright by the schema — which is the check working.
pub fn receive(state: &AppState, form: &DeliveryForm) -> Result<InventoryView> {
    let session = require_session(state)?;
    let now = Utc::now().timestamp_millis();

    if form.quantity_milli <= 0 {
        return Err(CommandError::refused(
            "A delivery has to be for more than nothing.",
        ));
    }
    let quantity = Milli::from_thousandths(form.quantity_milli);
    let cost = Money::parse(&form.total_cost)
        .map_err(|error| CommandError::refused(format!("What it cost: {error}")))?;
    if cost.is_negative() {
        return Err(CommandError::refused(
            "A delivery cannot cost less than nothing.",
        ));
    }

    state.with_db_mut(|conn| -> Result<()> {
        let transaction = conn.transaction()?;

        let product = catalogue::product(&transaction, form.product_id)?;
        if !product.tracks_inventory {
            return Err(CommandError::refused(format!(
                "Nobody counts {}, so there is no shelf for it to land on.",
                product.name
            )));
        }

        let shift = shifts::active(&transaction)?.map(|shift| shift.id);
        let supplier = purchases::house(&transaction, now)?;
        let batch = purchases::open(&transaction, supplier, cost, shift, now, session.staff_id)?;
        let unit = purchases::add_line(&transaction, batch, form.product_id, quantity, cost)?;

        // Before the movement lands: the standing average belongs to the shelf
        // as it stood, and posting first would blend the crate into itself.
        purchases::reaverage(&transaction, form.product_id, quantity, cost)?;
        stock::post(
            &transaction,
            &stock::Movement::new(
                form.product_id,
                stock::Kind::Purchase,
                quantity,
                now,
                session.staff_id,
            )
            .for_purchase(batch)
            .costing(unit),
        )?;

        let settings = Settings::load(&transaction)?;
        ledger::append(
            &transaction,
            &Event::new("STOCK_RECEIVED", "purchase", now)
                .about(batch)
                .by(session.staff_id)
                .during(shift)
                .recording(&format!(
                    "{} {} of {} for {}",
                    quantity,
                    product.base_unit,
                    product.name,
                    settings.format_money(cost)
                )),
        )?;

        transaction.commit()?;
        Ok(())
    })?;

    floor::inventory_view(state)
}

// ---------------------------------------------------------------------------
// The Tauri surface. Thin on purpose — the logic above is what is tested.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn cmd_receive_delivery(
    state: tauri::State<'_, AppState>,
    form: DeliveryForm,
) -> Result<InventoryView> {
    receive(&state, &form)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::repo::fixture;
    use crate::state::Session;

    struct Store {
        state: AppState,
        directory: PathBuf,
        beer: i64,
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// A stockroom with one product on the shelf, signed in as a cashier —
    /// deliberately not the owner, because receiving is not an owner's job.
    fn store(name: &str) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "servepoint-receiving-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let conn = crate::db::open(directory.join("servepoint.db")).unwrap();

        let cashier = fixture::staff(&conn, "CSH-R", "Abel", "CASHIER");
        let beer = fixture::product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);

        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: cashier,
            code: "CSH-R".into(),
            name: "Abel".into(),
            role: "CASHIER".into(),
        }));
        Store {
            state,
            directory,
            beer,
        }
    }

    fn crate_of(product_id: i64, quantity_milli: i64, total_cost: &str) -> DeliveryForm {
        DeliveryForm {
            product_id,
            quantity_milli,
            total_cost: total_cost.to_owned(),
        }
    }

    fn on_hand(state: &AppState, product_id: i64) -> String {
        floor::inventory_view(state)
            .unwrap()
            .lines
            .into_iter()
            .find(|line| line.product_id == product_id)
            .unwrap()
            .on_hand
    }

    #[test]
    fn a_delivery_lands_on_the_shelf() {
        let store = store("lands");
        assert_eq!(on_hand(&store.state, store.beer), "0");
        receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap();
        assert_eq!(on_hand(&store.state, store.beer), "24");
    }

    #[test]
    fn restocking_may_happen_again_and_again() {
        // The gap this module fills: counting in is once-only, so a shelf that
        // ran low could not be refilled at all.
        let store = store("again");
        for _ in 0..3 {
            receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap();
        }
        assert_eq!(on_hand(&store.state, store.beer), "72");
    }

    #[test]
    fn a_delivery_does_not_need_an_open_shift() {
        // §8.1: stock arrives while the club is shut, which is the whole point.
        let store = store("shut");
        assert!(receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).is_ok());
    }

    #[test]
    fn every_delivery_gets_its_own_batch_without_anybody_typing_one() {
        let store = store("batch");
        receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap();
        let view = receive(&store.state, &crate_of(store.beer, 24_000, "500.00")).unwrap();

        let batches: Vec<i64> = view.deliveries.iter().map(|line| line.batch).collect();
        assert_eq!(batches.len(), 2, "got: {:?}", view.deliveries);
        assert_ne!(batches[0], batches[1]);
    }

    #[test]
    fn the_history_says_what_each_crate_cost() {
        let store = store("history");
        let view = receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap();
        let latest = &view.deliveries[0];
        assert_eq!(latest.name, "Beer");
        assert_eq!(latest.quantity, "24");
        assert!(latest.cost.contains("480"), "got: {}", latest.cost);
    }

    #[test]
    fn the_shelf_is_worth_what_the_deliveries_cost() {
        // 24 bottles for 480.00 is 20.00 each, and the shelf is worth the lot.
        let store = store("value");
        let view = receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap();
        assert!(
            view.total_value.contains("480"),
            "got: {}",
            view.total_value
        );
    }

    #[test]
    fn a_second_crate_at_a_new_price_blends_the_average() {
        // 24 at 20.00 then 24 at 30.00 leaves 48 bottles worth 1200.00.
        let store = store("blend");
        receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap();
        let view = receive(&store.state, &crate_of(store.beer, 24_000, "720.00")).unwrap();
        assert!(
            view.total_value.contains("1,200") || view.total_value.contains("1200"),
            "got: {}",
            view.total_value
        );
    }

    #[test]
    fn a_delivery_of_nothing_is_refused() {
        let store = store("nothing");
        assert_eq!(
            receive(&store.state, &crate_of(store.beer, 0, "480.00"))
                .unwrap_err()
                .kind,
            "REFUSED"
        );
    }

    #[test]
    fn a_price_that_is_not_a_number_is_refused() {
        let store = store("words");
        assert_eq!(
            receive(&store.state, &crate_of(store.beer, 24_000, "a lot"))
                .unwrap_err()
                .kind,
            "REFUSED"
        );
    }

    #[test]
    fn nothing_arrives_for_a_product_nobody_counts() {
        let store = store("untracked");
        store
            .state
            .with_db(|conn| {
                conn.execute(
                    "UPDATE products SET tracks_inventory = 0 WHERE id = ?1",
                    [store.beer],
                )
                .map_err(crate::repo::RepoError::from)
            })
            .unwrap();
        assert_eq!(
            receive(&store.state, &crate_of(store.beer, 24_000, "480.00"))
                .unwrap_err()
                .kind,
            "REFUSED"
        );
    }

    #[test]
    fn an_audit_failure_rolls_back_the_batch_the_cost_and_the_movement() {
        // Ported from the opening-stock suite when counting in was removed: the
        // property it guarded is the same one deliveries need. A cost updated
        // without the stock arriving would value a shelf that never grew.
        let store = store("rollback");
        let cost_of = || {
            store
                .state
                .with_db(|conn| {
                    conn.query_row(
                        "SELECT avg_cost_minor FROM products WHERE id = ?1",
                        [store.beer],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(crate::repo::RepoError::from)
                })
                .unwrap()
        };

        // The standing cost is whatever the shelf already carries, not zero —
        // the point is that a failed delivery leaves it exactly as it was.
        let standing = cost_of();
        store
            .state
            .with_db(|conn| {
                conn.execute_batch(
                    "CREATE TEMP TRIGGER fail_delivery_audit BEFORE INSERT ON audit_log
                     WHEN NEW.action = 'STOCK_RECEIVED'
                     BEGIN SELECT RAISE(ABORT, 'injected delivery audit failure'); END;",
                )
                .map_err(crate::repo::RepoError::from)
            })
            .unwrap();

        receive(&store.state, &crate_of(store.beer, 24_000, "480.00")).unwrap_err();

        assert_eq!(on_hand(&store.state, store.beer), "0");
        assert_eq!(cost_of(), standing, "cost survived a rolled-back delivery");
        let batches = store
            .state
            .with_db(|conn| {
                conn.query_row("SELECT COUNT(*) FROM purchases", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(crate::repo::RepoError::from)
            })
            .unwrap();
        assert_eq!(batches, 0, "a batch survived a rolled-back delivery");
    }

    #[test]
    fn a_signed_out_window_receives_nothing() {
        let store = store("signed-out");
        store.state.set_session(None);
        assert_eq!(
            receive(&store.state, &crate_of(store.beer, 24_000, "480.00"))
                .unwrap_err()
                .kind,
            "SIGNED_OUT"
        );
    }
}
