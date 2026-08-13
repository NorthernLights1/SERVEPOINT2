//! End of the night — each waiter hands over what they took (§7.5).
//!
//! Closing a tab records what a waiter *owes*. It does not put money in the
//! drawer. Only a finalized reconciliation does that, and `repo::cash` keeps
//! those two paths apart deliberately. This module owns the transaction that
//! wraps the three steps `cash` leaves separate — create, allocate, finalize —
//! so a waiter is either fully settled or not settled at all.
//!
//! There are two shapes of settlement and the difference matters:
//!
//! * **Ordinary** — the waiter has closed tabs. Every one is allocated, and
//!   the total allocated must equal what was expected.
//! * **Old balance** — no closed tabs remain but the waiter still holds money
//!   from a shortfall on an earlier night. Nothing is allocated and the
//!   expected figure is the held balance itself.
//!
//! Getting these the wrong way round would clear a liability twice, so the
//! choice is made here from the data rather than asked of the cashier.

use rusqlite::Connection;

use crate::commands::{require_session, CommandError, ShiftView};
use crate::floor::require_cashier;
use crate::ledger::{self, Event};
use crate::repo::{cash, shifts, staff};
use crate::settings::Settings;
use crate::state::AppState;
use crate::Money;

type Result<T> = std::result::Result<T, CommandError>;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// How the waiter handed it over. One method per settlement — never split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Method {
    Cash,
    NonCash,
    WriteOff,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabDue {
    pub tab_id: i64,
    pub code: String,
    pub label: String,
    pub amount: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaiterSettlement {
    pub waiter_id: i64,
    pub name: String,
    /// Closed tabs waiting to be settled, each with what it brought in.
    pub tabs: Vec<TabDue>,
    /// What settling right now would clear.
    pub due: String,
    /// Everything this waiter still holds, including any earlier shortfall.
    pub held: String,
    /// False when there is nothing to settle and the row is informational.
    pub owes_anything: bool,
    /// True on the old-balance path — a shortfall carried in from a previous
    /// night, with no tabs left to allocate against it.
    pub old_balance: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationView {
    pub shift: Option<ShiftView>,
    pub waiters: Vec<WaiterSettlement>,
    /// What the drawer should hold if every movement is accounted for.
    pub expected_cash: String,
    /// True once nothing is outstanding and the night may start closing.
    pub can_begin_closing: bool,
    /// Why it cannot, in words. `None` once it can.
    pub blocker: Option<String>,
}

/// Every closed, unreconciled tab, with the waiter who holds it.
fn outstanding(conn: &Connection, settings: &Settings) -> Result<Vec<(i64, TabDue)>> {
    let mut statement = conn.prepare(
        "SELECT p.waiter_id, t.id, t.code, t.display_label, p.liability_minor
           FROM tabs t JOIN tab_payments p ON p.tab_id = t.id
          WHERE t.status = 'CLOSED'
          ORDER BY t.closed_at, t.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            TabDue {
                tab_id: row.get(1)?,
                code: row.get(2)?,
                label: row.get(3)?,
                amount: String::new(),
            },
            Money::from_minor(row.get::<_, i64>(4)?),
        ))
    })?;
    let found = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(found
        .into_iter()
        .map(|(waiter_id, tab, liability)| {
            (
                waiter_id,
                TabDue {
                    amount: settings.format_money(liability),
                    ..tab
                },
            )
        })
        .collect())
}

/// What settling this waiter would clear, and whether it is the old-balance
/// path. Derived, never asked for — see the module note.
fn expected_for(conn: &Connection, waiter_id: i64) -> Result<(Money, bool)> {
    let mut due = Money::ZERO;
    let mut any = false;
    let mut statement = conn.prepare(
        "SELECT p.liability_minor
           FROM tabs t JOIN tab_payments p ON p.tab_id = t.id
          WHERE t.status = 'CLOSED' AND p.waiter_id = ?1",
    )?;
    for row in statement.query_map([waiter_id], |row| row.get::<_, i64>(0))? {
        due = due.checked_add(Money::from_minor(row?))?;
        any = true;
    }
    if any {
        return Ok((due, false));
    }
    Ok((cash::held_balance(conn, waiter_id)?, true))
}

/// How many waiters are still holding money.
///
/// `create_reconciliation` and `finalize_reconciliation` both require an OPEN
/// shift, so everybody has to be settled *before* the night begins closing.
/// The view and the command below share this one count so the screen cannot
/// say one thing while the command does another.
fn still_holding(conn: &Connection) -> Result<usize> {
    let mut count = 0;
    for person in staff::waiters(conn)? {
        if !expected_for(conn, person.id)?.0.is_zero() {
            count += 1;
        }
    }
    Ok(count)
}

fn view_of(conn: &Connection, now: i64) -> Result<ReconciliationView> {
    let settings = Settings::load(conn)?;
    let shift = shifts::active(conn)?;
    let due_tabs = outstanding(conn, &settings)?;

    let mut waiters = Vec::new();
    for person in staff::waiters(conn)? {
        let tabs: Vec<TabDue> = due_tabs
            .iter()
            .filter(|(waiter_id, _)| *waiter_id == person.id)
            .map(|(_, tab)| tab.clone())
            .collect();
        let held = cash::held_balance(conn, person.id)?;
        let (due, old_balance) = expected_for(conn, person.id)?;
        waiters.push(WaiterSettlement {
            waiter_id: person.id,
            name: person.name,
            owes_anything: !due.is_zero(),
            tabs,
            due: settings.format_money(due),
            held: settings.format_money(held),
            old_balance,
        });
    }

    // The night may not begin closing while money is still in somebody's
    // pocket or a print is still unresolved.
    let unsettled = still_holding(conn)?;
    let recovered = shifts::recovery_complete(conn)?;
    let blocker = match shift.as_ref() {
        None => Some("No trading night is open.".to_owned()),
        Some(open) if open.status != shifts::Status::Open => {
            Some("This night has already begun closing.".to_owned())
        }
        Some(_) if !recovered => {
            Some("An issue slip is still unresolved. Settle it on the till first.".to_owned())
        }
        Some(_) if unsettled > 0 => Some(format!(
            "{unsettled} waiter{} still holding money.",
            if unsettled == 1 { " is" } else { "s are" }
        )),
        Some(_) => None,
    };

    Ok(ReconciliationView {
        expected_cash: match shift.as_ref() {
            Some(open) => settings.format_money(cash::expected_cash(conn, open.id)?),
            None => settings.format_money(Money::ZERO),
        },
        shift: crate::commands::open_shift_of(conn, now)?,
        waiters,
        can_begin_closing: blocker.is_none(),
        blocker,
    })
}

pub fn reconciliation_view(state: &AppState) -> Result<ReconciliationView> {
    require_session(state)?;
    let now = now_ms();
    state.with_db(|conn| view_of(conn, now))
}

/// Settle one waiter in a single commit: create, allocate every closed tab,
/// finalize, and extend the audit chain.
///
/// `amount` may be less than what is owed — the difference is recorded as a
/// shortfall the waiter still carries, rather than being quietly forgiven.
pub fn settle_waiter(
    state: &AppState,
    waiter_id: i64,
    method: Method,
    amount: &str,
    reason: Option<&str>,
) -> Result<ReconciliationView> {
    let session = require_cashier(state)?;
    let now = now_ms();
    let amount = Money::parse(amount)
        .map_err(|error| CommandError::refused(format!("Handed over: {error}")))?;
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let taken = |wanted: Method| {
        if method == wanted {
            amount
        } else {
            Money::ZERO
        }
    };

    state.with_db_mut(|conn| -> Result<()> {
        let shift = shifts::active(conn)?
            .ok_or_else(|| CommandError::refused("No trading night is open."))?;
        let (expected, old_balance) = expected_for(conn, waiter_id)?;
        if expected.is_zero() {
            return Err(CommandError::refused(
                "That waiter has nothing outstanding to settle.",
            ));
        }

        let transaction = conn.transaction()?;
        let reconciliation = cash::create_reconciliation(
            &transaction,
            &cash::NewReconciliation {
                waiter_id,
                cashier_id: session.staff_id,
                expected,
                cash: taken(Method::Cash),
                non_cash: taken(Method::NonCash),
                written_off: taken(Method::WriteOff),
                write_off_reason: (method == Method::WriteOff).then_some(reason).flatten(),
                shift_id: shift.id,
                created_at: now,
            },
        )?;

        if !old_balance {
            let tab_ids = {
                let mut statement = transaction.prepare(
                    "SELECT t.id FROM tabs t JOIN tab_payments p ON p.tab_id = t.id
                      WHERE t.status = 'CLOSED' AND p.waiter_id = ?1
                      ORDER BY t.id",
                )?;
                let rows = statement.query_map([waiter_id], |row| row.get::<_, i64>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for tab_id in tab_ids {
                cash::allocate_tab(&transaction, reconciliation.id, tab_id, session.staff_id)?;
            }
        }

        let sealed =
            cash::finalize_reconciliation(&transaction, reconciliation.id, session.staff_id, now)?;
        ledger::append(
            &transaction,
            &Event::new("WAITER_RECONCILED", "reconciliation", now)
                .about(sealed.id)
                .by(session.staff_id)
                .during(Some(shift.id))
                .recording(&format!(
                    "waiter={};expected_minor={};shortfall_minor={}",
                    waiter_id,
                    sealed.expected.minor(),
                    sealed.shortfall.minor()
                )),
        )?;
        transaction.commit()?;
        Ok(())
    })?;
    reconciliation_view(state)
}

/// Stop the night taking new trade. Counting the drawer and sealing the night
/// happen after this.
pub fn begin_closing(state: &AppState) -> Result<ReconciliationView> {
    let session = require_cashier(state)?;
    let now = now_ms();
    state.with_db_mut(|conn| -> Result<()> {
        let shift = shifts::active(conn)?
            .ok_or_else(|| CommandError::refused("No trading night is open."))?;
        // Settling requires an open shift, so anybody still holding money
        // would be stranded on the other side of this transition.
        let holding = still_holding(conn)?;
        if holding > 0 {
            return Err(CommandError::refused(format!(
                "{holding} waiter{} still holding money. Settle them before closing.",
                if holding == 1 { " is" } else { "s are" }
            )));
        }
        let transaction = conn.transaction()?;
        shifts::begin_closing(&transaction, shift.id, session.staff_id)?;
        ledger::append(
            &transaction,
            &Event::new("SHIFT_CLOSING", "shift", now)
                .about(shift.id)
                .changed("OPEN", "CLOSING")
                .by(session.staff_id)
                .during(Some(shift.id)),
        )?;
        transaction.commit()?;
        Ok(())
    })?;
    reconciliation_view(state)
}

/// What the night came to, once the drawer has been counted.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedNight {
    pub code: String,
    pub business_date_label: String,
    /// What the ledger says should be in the drawer.
    pub expected: String,
    /// What was physically counted out of it.
    pub counted: String,
    /// The difference, as a positive figure — `over` says which way.
    pub variance: String,
    pub over: bool,
    pub balanced: bool,
}

/// Seal the night against a physical count of the drawer.
///
/// The count is recorded as given. Any difference from what the ledger expects
/// is reported, never reconciled away — a drawer that is short is a fact the
/// owner needs, not a number to be corrected.
///
/// §4.3: closing the shift and storing its report are ONE all-or-nothing
/// commit. The report is compiled after `shifts::close` and inside the same
/// transaction, so it reads the sealed counted figure — and if it cannot be
/// stored, the night does not close. A closed night missing its only
/// fraud-control document is not a state this system has.
pub fn close_night(state: &AppState, counted_cash: &str) -> Result<ClosedNight> {
    let session = require_cashier(state)?;
    let now = now_ms();
    let counted = Money::parse(counted_cash)
        .map_err(|error| CommandError::refused(format!("Counted cash: {error}")))?;

    state.with_db_mut(|conn| {
        let shift = shifts::active(conn)?
            .ok_or_else(|| CommandError::refused("There is no night to close."))?;
        let expected = cash::expected_cash(conn, shift.id)?;

        let transaction = conn.transaction()?;
        let closed = shifts::close(&transaction, shift.id, counted, session.staff_id, now)?;
        ledger::append(
            &transaction,
            &Event::new("SHIFT_CLOSED", "shift", now)
                .about(shift.id)
                .changed("CLOSING", "CLOSED")
                .by(session.staff_id)
                .during(Some(shift.id))
                .recording(&format!(
                    "expected_minor={};counted_minor={}",
                    expected.minor(),
                    counted.minor()
                )),
        )?;
        let report = crate::report::freeze(&transaction, shift.id, session.staff_id, now)?;
        ledger::append(
            &transaction,
            &Event::new("SHIFT_REPORT_STORED", "shift_report", now)
                .about(report.id)
                .by(session.staff_id)
                .during(Some(shift.id)),
        )?;
        let settings = Settings::load(&transaction)?;
        transaction.commit()?;

        let over = counted.minor() >= expected.minor();
        let variance = if over {
            counted.checked_sub(expected)?
        } else {
            expected.checked_sub(counted)?
        };
        Ok(ClosedNight {
            code: closed.code,
            business_date_label: crate::calendar::describe(
                chrono::NaiveDate::parse_from_str(&closed.business_date, "%Y-%m-%d")
                    .map_err(|_| CommandError::of("DATABASE", "that night has no readable date"))?,
            ),
            expected: settings.format_money(expected),
            counted: settings.format_money(counted),
            variance: settings.format_money(variance),
            balanced: variance.is_zero(),
            over,
        })
    })
}

#[tauri::command]
pub fn cmd_reconciliation_view(state: tauri::State<'_, AppState>) -> Result<ReconciliationView> {
    reconciliation_view(&state)
}

#[tauri::command]
pub fn cmd_close_night(
    state: tauri::State<'_, AppState>,
    counted_cash: String,
) -> Result<ClosedNight> {
    close_night(&state, &counted_cash)
}

#[tauri::command]
pub fn cmd_settle_waiter(
    state: tauri::State<'_, AppState>,
    waiter_id: i64,
    method: Method,
    amount: String,
    reason: Option<String>,
) -> Result<ReconciliationView> {
    settle_waiter(&state, waiter_id, method, &amount, reason.as_deref())
}

#[tauri::command]
pub fn cmd_begin_closing(state: tauri::State<'_, AppState>) -> Result<ReconciliationView> {
    begin_closing(&state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Session;
    use crate::{bills, floor, repo::fixture, Milli};

    struct Night {
        state: AppState,
        directory: std::path::PathBuf,
        waiter: i64,
    }

    impl Drop for Night {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// One beer sold to one tab, and that tab already closed — the state a
    /// waiter is reconciled from.
    fn night(name: &str) -> Night {
        let directory = std::env::temp_dir().join(format!(
            "servepoint-reconcile-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let conn = crate::db::open(directory.join("servepoint.db")).unwrap();

        let owner = fixture::staff(&conn, "OWN-R", "Selam", "OWNER");
        let cashier = fixture::staff(&conn, "CSH-R", "Abel", "CASHIER");
        let waiter = fixture::staff(&conn, "WTR-R", "Sara", "WAITER");
        let beer = fixture::product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);
        let bottle = fixture::sale_item(&conn, "S-BEER", "Beer", "Bottles", 5_000);
        fixture::recipe(&conn, bottle, &[(beer, 1_000)]);
        fixture::stock_up(&conn, beer, 5_000, owner);

        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: cashier,
            code: "CSH-R".into(),
            name: "Abel".into(),
            role: "CASHIER".into(),
        }));
        floor::open_shift(&state, "500").unwrap();
        let view = floor::open_tab(&state, waiter, "7", None).unwrap();
        let tab = view.tabs[0].id;
        floor::place_order(
            &state,
            tab,
            &[floor::OrderLine {
                sale_item_id: bottle,
                quantity_milli: Milli::ONE.thousandths(),
            }],
        )
        .unwrap();
        bills::settle_tab(&state, tab, None, None).unwrap();

        Night {
            state,
            directory,
            waiter,
        }
    }

    #[test]
    fn a_waiter_hands_over_what_the_tabs_took_and_the_night_can_then_close() {
        let night = night("full");
        let view = reconciliation_view(&night.state).unwrap();
        let sara = &view.waiters[0];
        assert_eq!(sara.name, "Sara");
        assert_eq!(sara.tabs.len(), 1);
        // 50.00 of drinks plus this venue's default 10% service charge.
        assert_eq!(sara.due, "55.00");
        assert_eq!(sara.tabs[0].amount, "55.00");
        assert!(sara.owes_anything && !sara.old_balance);
        // The night cannot close while she is still holding it.
        assert!(!view.can_begin_closing);
        assert!(view.blocker.unwrap().contains("still holding"));

        let view = settle_waiter(&night.state, night.waiter, Method::Cash, "55", None).unwrap();
        assert_eq!(view.waiters[0].held, "0.00");
        assert!(!view.waiters[0].owes_anything);
        assert!(view.waiters[0].tabs.is_empty());
        // The float, plus what she handed over.
        assert_eq!(view.expected_cash, "555.00");
        assert!(view.can_begin_closing, "{:?}", view.blocker);

        let view = begin_closing(&night.state).unwrap();
        assert!(!view.can_begin_closing);
        assert!(view.blocker.unwrap().contains("already begun closing"));

        // Counting the drawer seals it, and the count is reported as given.
        let closed = close_night(&night.state, "555").unwrap();
        assert!(closed.balanced);
        assert_eq!(closed.expected, "555.00");
        assert_eq!(closed.counted, "555.00");
        assert_eq!(closed.variance, "0.00");
        assert!(reconciliation_view(&night.state)
            .unwrap()
            .blocker
            .unwrap()
            .contains("No trading night"));
    }

    #[test]
    fn a_drawer_that_is_short_says_so_rather_than_being_adjusted() {
        let night = night("short-drawer");
        settle_waiter(&night.state, night.waiter, Method::Cash, "55", None).unwrap();
        begin_closing(&night.state).unwrap();

        let closed = close_night(&night.state, "540").unwrap();
        assert!(!closed.balanced && !closed.over);
        assert_eq!(closed.expected, "555.00");
        assert_eq!(closed.counted, "540.00");
        assert_eq!(closed.variance, "15.00");
    }

    #[test]
    fn a_night_cannot_close_while_a_waiter_still_holds_money() {
        let night = night("unsettled");
        // begin_closing is refused first, so CLOSING is never reached.
        let refused = begin_closing(&night.state).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert_eq!(
            close_night(&night.state, "555").unwrap_err().kind,
            "REFUSED"
        );
    }

    #[test]
    fn handing_over_less_than_is_owed_leaves_the_difference_on_the_waiter() {
        let night = night("short");
        let view = settle_waiter(&night.state, night.waiter, Method::Cash, "30", None).unwrap();
        let sara = &view.waiters[0];
        // The tabs are cleared, but 20.00 of shortfall is still carried.
        assert!(sara.tabs.is_empty());
        assert_eq!(sara.held, "25.00");
        assert_eq!(sara.due, "25.00");
        assert!(sara.old_balance, "a carried shortfall has no tabs left");
        assert_eq!(view.expected_cash, "530.00");

        // Settling the remainder later clears her completely.
        let view = settle_waiter(&night.state, night.waiter, Method::Cash, "25", None).unwrap();
        assert_eq!(view.waiters[0].held, "0.00");
        assert!(view.can_begin_closing, "{:?}", view.blocker);
    }

    #[test]
    fn a_waiter_with_nothing_outstanding_cannot_be_settled_twice() {
        let night = night("twice");
        settle_waiter(&night.state, night.waiter, Method::Cash, "55", None).unwrap();
        let refused =
            settle_waiter(&night.state, night.waiter, Method::Cash, "55", None).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(
            refused.message.contains("nothing outstanding"),
            "{}",
            refused.message
        );
    }
}
