//! Ways out of a durable state that nothing else can clear (D10 / INV-13).
//!
//! Most non-terminal states in `resolution` are reachable through an ordinary
//! screen — an open tab is closed on the till, a closed one is settled at the
//! end of the night. One is not: an order left in `PRINTING` because the paper
//! never came out. Its BR numbers are already burned and its stock is already
//! reserved, so it blocks the night from closing until somebody says what
//! actually happened at the printer.
//!
//! Both answers here are statements of fact by the person who was standing
//! there, never a guess this code makes on their behalf.

use crate::commands::{require_session, CommandError};
use crate::floor::require_till;
use crate::repo::{orders, receipts, staff, tabs};
use crate::state::AppState;
use crate::{correction, trading};

type Result<T> = std::result::Result<T, CommandError>;

/// Whether this stranded order is a correction's replacement paper.
///
/// It looks identical in the list — PRINTING, numbers burned, nothing on the
/// tray — but it must not be answered as an ordinary round. Its original is
/// still ISSUED, and only the delta between the two belongs on the tab or on
/// the shelf. `correction` owns both answers; this just picks the right owner.
fn frozen_correction(
    conn: &rusqlite::Connection,
    order_id: i64,
) -> Result<Option<orders::PendingCorrection>> {
    Ok(orders::pending_corrections(conn)?
        .into_iter()
        .find(|frozen| frozen.replacement_order_id == order_id))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrandedPrint {
    pub order_id: i64,
    pub tab_label: String,
    pub waiter: String,
    /// The numbers already burned on this attempt. They are never reused.
    pub receipt_numbers: Vec<String>,
    pub rang_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryView {
    pub prints: Vec<StrandedPrint>,
}

fn view_of(conn: &rusqlite::Connection) -> Result<RecoveryView> {
    let mut prints = Vec::new();
    for order in orders::stranded_prints(conn)? {
        let tab = tabs::find(conn, order.tab_id)?;
        let waiter = staff::find(conn, order.waiter_id)?;
        prints.push(StrandedPrint {
            order_id: order.id,
            tab_label: tab.display_label,
            waiter: waiter.name,
            receipt_numbers: receipts::for_order(conn, order.id)?
                .into_iter()
                .filter(|receipt| receipt.status != receipts::Status::Void)
                .map(|receipt| receipt.receipt_number)
                .collect(),
            rang_at: order.created_at,
        });
    }
    Ok(RecoveryView { prints })
}

pub fn recovery_view(state: &AppState) -> Result<RecoveryView> {
    require_session(state)?;
    state.with_db(view_of)
}

/// The printer is dead and the cashier has written the chit out by hand.
///
/// Stock posts exactly as it would for a printed issue; the failed attempts
/// stay as append-only evidence that it happened.
pub fn resolve_handwritten(state: &AppState, order_id: i64) -> Result<RecoveryView> {
    let session = require_till(state)?;
    let now = now_ms();
    state.with_db_mut(|conn| -> Result<()> {
        match frozen_correction(conn, order_id)? {
            Some(frozen) => {
                correction::complete(
                    conn,
                    frozen.id,
                    session.staff_id,
                    now,
                    correction::PrintOutcome::Handwritten,
                )?;
            }
            None => {
                trading::authorize_handwritten(conn, order_id, session.staff_id, now)?;
            }
        }
        Ok(())
    })?;
    recovery_view(state)
}

/// Nothing came out, and nothing was written by hand.
///
/// Every number on this attempt is voided rather than reused, and the draft
/// returns to the till to be rung again.
pub fn resolve_non_print(state: &AppState, order_id: i64) -> Result<RecoveryView> {
    let session = require_till(state)?;
    let now = now_ms();
    state.with_db_mut(|conn| -> Result<()> {
        match frozen_correction(conn, order_id)? {
            // Abandoning gives the replacement's numbers up and leaves the
            // original standing, which is exactly right: nothing was corrected
            // because nothing came out to correct it with.
            Some(frozen) => correction::abandon(conn, frozen.id, session.staff_id, now)?,
            None => {
                trading::confirm_non_print(conn, order_id, session.staff_id, now)?;
            }
        }
        Ok(())
    })?;
    recovery_view(state)
}

#[tauri::command]
pub fn cmd_recovery_view(state: tauri::State<'_, AppState>) -> Result<RecoveryView> {
    recovery_view(&state)
}

#[tauri::command]
pub fn cmd_resolve_handwritten(
    state: tauri::State<'_, AppState>,
    order_id: i64,
) -> Result<RecoveryView> {
    resolve_handwritten(&state, order_id)
}

#[tauri::command]
pub fn cmd_resolve_non_print(
    state: tauri::State<'_, AppState>,
    order_id: i64,
) -> Result<RecoveryView> {
    resolve_non_print(&state, order_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{orders, shifts, stock};
    use crate::state::Session;
    use crate::{correction, floor, repo::fixture, Milli, Money};

    struct Wedged {
        state: AppState,
        directory: std::path::PathBuf,
        beer: i64,
        bottle: i64,
        tab: i64,
        cashier: i64,
    }

    impl Drop for Wedged {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// An order left in PRINTING — numbers burned, stock reserved, nothing on
    /// paper. Reached by running only the first half of the issue protocol,
    /// which is exactly what a dead printer leaves behind.
    fn wedged(name: &str) -> Wedged {
        let directory =
            std::env::temp_dir().join(format!("servepoint-recovery-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let conn = crate::db::open(directory.join("servepoint.db")).unwrap();

        let owner = fixture::staff(&conn, "OWN-V", "Selam", "OWNER");
        let cashier = fixture::staff(&conn, "CSH-V", "Abel", "CASHIER");
        let waiter = fixture::staff(&conn, "WTR-V", "Sara", "WAITER");
        let beer = fixture::product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);
        let bottle = fixture::sale_item(&conn, "S-BEER", "Beer", "Bottles", 5_000);
        fixture::recipe(&conn, bottle, &[(beer, 1_000)]);
        fixture::stock_up(&conn, beer, 2_000, owner);

        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: cashier,
            code: "CSH-V".into(),
            name: "Abel".into(),
            role: "CASHIER".into(),
        }));
        floor::open_shift(&state, "500").unwrap();
        let view = floor::open_tab(&state, waiter, "7", None).unwrap();
        let tab = view.tabs[0].id;

        state.with_db_mut(|conn| {
            let shift_id = shifts::active(conn).unwrap().unwrap().id;
            let order = orders::create(
                conn,
                orders::NewDraft {
                    tab_id: tab,
                    shift_id,
                    cashier_id: cashier,
                    created_at: 1,
                },
            )
            .unwrap();
            orders::add_line(conn, order.id, bottle, Milli::ONE).unwrap();
            trading::prepare_issue(conn, order.id, cashier, 2).unwrap();
        });

        Wedged {
            state,
            directory,
            beer,
            bottle,
            tab,
            cashier,
        }
    }

    #[test]
    fn a_stranded_print_is_listed_with_the_numbers_it_already_burned() {
        let wedged = wedged("list");
        let view = recovery_view(&wedged.state).unwrap();
        assert_eq!(view.prints.len(), 1);
        let stuck = &view.prints[0];
        assert_eq!(stuck.tab_label, "Table 7");
        assert_eq!(stuck.waiter, "Sara");
        assert_eq!(stuck.receipt_numbers.len(), 1);
        assert!(stuck.receipt_numbers[0].starts_with("BR-"));
    }

    #[test]
    fn a_handwritten_chit_posts_the_stock_and_clears_the_wedge() {
        let wedged = wedged("hand");
        let order_id = recovery_view(&wedged.state).unwrap().prints[0].order_id;

        let view = resolve_handwritten(&wedged.state, order_id).unwrap();
        assert!(view.prints.is_empty());
        // The drink left the shelf, because it was actually poured.
        let on_hand = wedged
            .state
            .with_db(|conn| stock::on_hand(conn, wedged.beer).unwrap());
        assert_eq!(on_hand, Milli::ONE);
    }

    #[test]
    fn a_confirmed_non_print_gives_the_numbers_up_and_returns_the_stock() {
        let wedged = wedged("void");
        let order_id = recovery_view(&wedged.state).unwrap().prints[0].order_id;

        let view = resolve_non_print(&wedged.state, order_id).unwrap();
        assert!(view.prints.is_empty());
        // Nothing was poured, so nothing left the shelf.
        let on_hand = wedged
            .state
            .with_db(|conn| stock::on_hand(conn, wedged.beer).unwrap());
        assert_eq!(on_hand, Milli::from_thousandths(2_000));

        // The burned number stays burned: the next issue takes a fresh one.
        let next: i64 = wedged.state.with_db(|conn| {
            conn.query_row(
                "SELECT next_value FROM sequences WHERE name = 'ISSUE_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        });
        assert_eq!(next, 2);
    }

    /// Freeze a correction against `original` whose paper never came out.
    fn strand_a_correction(wedged: &Wedged, original: i64) -> i64 {
        let (number, line) = wedged.state.with_db(|conn| {
            (
                receipts::for_order(conn, original).unwrap()[0]
                    .receipt_number
                    .clone(),
                orders::lines(conn, original).unwrap().remove(0),
            )
        });
        // One bottle became two: the shelf owes one more, the tab owes one more.
        wedged
            .state
            .with_db_mut(|conn| {
                correction::prepare(
                    conn,
                    correction::NewCorrection {
                        original_order_id: original,
                        issue_receipt_number: &number,
                        reason: "they asked for two",
                        cashier_id: wedged.cashier,
                        at: 3,
                    },
                    &[correction::ReplacementLine {
                        original_line_id: Some(line.id),
                        sale_item_id: wedged.bottle,
                        quantity: Milli::from_thousandths(2_000),
                    }],
                    &[],
                )
                .unwrap()
            })
            .replacement
            .id
    }

    /// The cashier who began a correction may have gone home before the
    /// printer was ever fixed. Somebody has to be able to answer for the paper,
    /// or a stranded correction holds the night open forever.
    #[test]
    fn another_cashier_can_answer_for_a_correction_that_was_left_stranded() {
        let wedged = wedged("relief");
        let first = recovery_view(&wedged.state).unwrap().prints[0].order_id;
        resolve_handwritten(&wedged.state, first).unwrap();
        let replacement = strand_a_correction(&wedged, first);

        let relief = wedged
            .state
            .with_db(|conn| fixture::staff(conn, "CSH-V2", "Meron", "CASHIER"));
        wedged.state.set_session(Some(Session {
            staff_id: relief,
            code: "CSH-V2".into(),
            name: "Meron".into(),
            role: "CASHIER".into(),
        }));

        let view = resolve_handwritten(&wedged.state, replacement).unwrap();
        assert!(view.prints.is_empty());
        assert!(wedged
            .state
            .with_db(|conn| shifts::recovery_complete(conn).unwrap()));
    }

    /// A correction's replacement paper strands in the same PRINTING state as
    /// an ordinary round and lands in the same list — but it must not be
    /// resolved as one. Its original is still ISSUED, so issuing the
    /// replacement as a fresh round bills the tab for both.
    #[test]
    fn a_stranded_correction_is_finished_as_a_correction_not_as_a_fresh_round() {
        let wedged = wedged("correction");
        let first = recovery_view(&wedged.state).unwrap().prints[0].order_id;
        resolve_handwritten(&wedged.state, first).unwrap();

        let replacement = strand_a_correction(&wedged, first);
        assert_eq!(
            recovery_view(&wedged.state).unwrap().prints[0].order_id,
            replacement
        );

        resolve_handwritten(&wedged.state, replacement).unwrap();

        // Two bottles at 50.00, not two bottles plus the original one.
        let total = wedged
            .state
            .with_db(|conn| tabs::running_total(conn, wedged.tab).unwrap());
        assert_eq!(total, Money::from_minor(10_000));
        assert_eq!(
            wedged
                .state
                .with_db(|conn| orders::find(conn, first).unwrap().status),
            orders::Status::Replaced
        );
        assert!(wedged
            .state
            .with_db(|conn| orders::pending_corrections(conn).unwrap())
            .is_empty());
        assert!(recovery_view(&wedged.state).unwrap().prints.is_empty());
        assert_eq!(
            wedged
                .state
                .with_db(|conn| stock::on_hand(conn, wedged.beer).unwrap()),
            Milli::ZERO
        );
    }
}
