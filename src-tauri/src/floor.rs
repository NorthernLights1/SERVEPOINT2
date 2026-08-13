//! The till, the floor and the shelf — what staff touch during service.
//!
//! Nothing here holds a rule of its own. `shifts`, `tabs` and `orders` own the
//! constraints, `trading` owns the all-or-nothing issue protocol, and
//! `printing` owns the paper. This module decides the order those happen in
//! and shapes the answer for the window.
//!
//! Every money figure leaves here as text. A running total the webview adds up
//! is a total that will one day disagree with the receipt.

use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone, Utc};
use rusqlite::Connection;

use crate::commands::{require_session, CommandError, ShiftView};
use crate::ledger::{self, Event};
use crate::printing::{self, Printer};
use crate::repo::{catalogue, orders, shifts, staff, stock, tabs};
use crate::settings::{Settings, TabReference};
use crate::state::{AppState, Session};
use crate::{trading, Milli, Money};

type Result<T> = std::result::Result<T, CommandError>;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// The till is a cashier's instrument — the repositories refuse anybody else
/// too, but the refusal is said here in words rather than as a constraint.
pub(crate) fn require_cashier(state: &AppState) -> Result<Session> {
    let session = require_session(state)?;
    if session.role != "CASHIER" {
        return Err(CommandError::of(
            "NOT_PERMITTED",
            "Only a cashier can work the till.",
        ));
    }
    Ok(session)
}

/// A local wall-clock instant as epoch milliseconds.
///
/// The business calendar deliberately works in local time — the night starts
/// at 18:00 *where the club is* — so the conversion back happens at this edge.
/// On the hour a clock goes back, the earlier of the two readings is taken.
fn local_ms(local: chrono::NaiveDateTime) -> Result<i64> {
    Local
        .from_local_datetime(&local)
        .earliest()
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| {
            CommandError::of(
                "DATABASE",
                "This venue's closing time does not exist on that date in this timezone.",
            )
        })
}

// ---------------------------------------------------------------------------
// What the frontend receives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabLine {
    pub id: i64,
    pub code: String,
    /// How the venue refers to this tab — "Table 7", a name, a reference.
    pub label: String,
    pub waiter: String,
    pub running_total: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaiterLine {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuLine {
    pub sale_item_id: i64,
    pub name: String,
    pub category: String,
    pub price: String,
}

/// Everything the till screen needs, in one round trip.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorView {
    pub shift: Option<ShiftView>,
    pub tabs: Vec<TabLine>,
    pub waiters: Vec<WaiterLine>,
    pub menu: Vec<MenuLine>,
    /// What this venue calls a tab: "Table number", "Customer name", ...
    pub tab_prompt: String,
    /// True when this venue's reference mode also wants a phone beside the
    /// name before a tab can be opened.
    pub wants_contact: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StockLine {
    pub product_id: i64,
    pub name: String,
    pub category: String,
    pub unit: String,
    pub on_hand: String,
    pub value: String,
    pub low: bool,
    pub tracked: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryView {
    pub lines: Vec<StockLine>,
    pub total_value: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlipLine {
    pub receipt_number: String,
    pub destination: String,
    pub text: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacedOrder {
    pub order_id: i64,
    pub slips: Vec<SlipLine>,
    /// The tab's total after this order, so the screen does not add it up.
    pub tab_total: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderLine {
    pub sale_item_id: i64,
    pub quantity_milli: i64,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Open tabs with what each one currently owes.
///
/// The total per tab is asked of `tabs::running_total` rather than summed by a
/// query written here, because a second definition of what a tab owes is a
/// second answer waiting to disagree with the bill. That costs one small query
/// per open tab; a venue with enough tabs open for that to matter would want
/// the total pushed into the listing query instead.
fn open_tabs(conn: &Connection, settings: &Settings) -> Result<Vec<TabLine>> {
    let mut statement = conn.prepare(
        "SELECT t.id, t.code, t.display_label, s.full_name
           FROM tabs t JOIN staff s ON s.id = t.waiter_id
          WHERE t.status = 'OPEN'
          ORDER BY t.opened_at",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let found = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    found
        .into_iter()
        .map(|(id, code, label, waiter)| {
            Ok(TabLine {
                id,
                code,
                label,
                waiter,
                running_total: settings.format_money(tabs::running_total(conn, id)?),
            })
        })
        .collect()
}

pub(crate) fn view_of(conn: &Connection, now: i64) -> Result<FloorView> {
    let settings = Settings::load(conn)?;
    Ok(FloorView {
        shift: crate::commands::open_shift_of(conn, now)?,
        tabs: open_tabs(conn, &settings)?,
        waiters: staff::waiters(conn)?
            .into_iter()
            .map(|person| WaiterLine {
                id: person.id,
                name: person.name,
            })
            .collect(),
        menu: catalogue::menu(conn)?
            .into_iter()
            .map(|item| MenuLine {
                sale_item_id: item.sale_item_id,
                name: item.name,
                category: item.category,
                price: settings.format_money(item.price),
            })
            .collect(),
        tab_prompt: settings.reference_mode().prompt().to_owned(),
        wants_contact: settings.reference_mode() == TabReference::CustomerPhone,
    })
}

pub fn floor_view(state: &AppState) -> Result<FloorView> {
    require_session(state)?;
    let now = now_ms();
    state.with_db(|conn| view_of(conn, now))
}

pub fn inventory_view(state: &AppState) -> Result<InventoryView> {
    require_session(state)?;
    state.with_db(|conn| {
        let settings = Settings::load(conn)?;
        let levels = stock::levels(conn)?;
        let mut total = Money::ZERO;
        let mut lines = Vec::with_capacity(levels.len());
        for level in levels {
            let value = level.value()?;
            total = total.checked_add(value)?;
            lines.push(StockLine {
                product_id: level.product_id,
                on_hand: level.on_hand.to_display(),
                value: settings.format_money(value),
                low: level.is_low(),
                tracked: level.tracks_inventory,
                name: level.name,
                category: level.category,
                unit: level.base_unit,
            });
        }
        Ok(InventoryView {
            lines,
            total_value: settings.format_money(total),
        })
    })
}

// ---------------------------------------------------------------------------
// Trading
// ---------------------------------------------------------------------------

pub fn open_shift(state: &AppState, opening_float: &str) -> Result<FloorView> {
    let session = require_cashier(state)?;
    let now = now_ms();
    let float = Money::parse(opening_float)
        .map_err(|error| CommandError::refused(format!("Opening float: {error}")))?;

    state.with_db_mut(|conn| -> Result<()> {
        let settings = Settings::load(conn)?;
        let calendar = settings.calendar()?;
        let date = calendar
            .business_date_for(now)
            .map_err(|error| CommandError::of("DATABASE", error.to_string()))?;
        let business_date = date.format("%Y-%m-%d").to_string();
        let expected_end_at = local_ms(calendar.expected_end(date))?;

        let transaction = conn.transaction()?;
        let shift = shifts::open(
            &transaction,
            &shifts::NewShift {
                business_date: &business_date,
                opened_at: now,
                opened_by: session.staff_id,
                opening_float: float,
                expected_end_at,
            },
        )?;
        // The float is cash entering the drawer, so it belongs in the chain
        // beside every other movement of money.
        ledger::append(
            &transaction,
            &Event::new("SHIFT_OPENED", "shift", now)
                .about(shift.id)
                .by(session.staff_id)
                .during(Some(shift.id))
                .recording(&settings.format_money(float)),
        )?;
        transaction.commit()?;
        Ok(())
    })?;
    floor_view(state)
}

pub fn open_tab(
    state: &AppState,
    waiter_id: i64,
    reference: &str,
    contact: Option<&str>,
) -> Result<FloorView> {
    let session = require_cashier(state)?;
    let now = now_ms();
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(CommandError::refused(
            "A tab needs a reference before it can be opened.",
        ));
    }
    let contact = contact.map(str::trim).filter(|text| !text.is_empty());

    state.with_db(|conn| -> Result<()> {
        let settings = Settings::load(conn)?;
        let shift = shifts::active(conn)?
            .ok_or_else(|| CommandError::refused("Open the trading night before opening a tab."))?;
        let reference = match settings.reference_mode() {
            TabReference::Table => tabs::Reference::table(reference),
            TabReference::CustomerName => tabs::Reference::customer_name(reference),
            TabReference::CustomerPhone => tabs::Reference::customer_phone(reference, contact),
            TabReference::Custom => tabs::Reference::custom(reference),
        };
        tabs::open(
            conn,
            &tabs::NewTab {
                opened_shift_id: shift.id,
                waiter_id,
                reference,
                opened_at: now,
                opened_by: session.staff_id,
            },
        )?;
        Ok(())
    })?;
    floor_view(state)
}

/// Ring an order, reserve its numbers, print, and post the stock.
///
/// Three steps in a deliberate order: the slips are numbered and durably
/// reserved, *then* paper is attempted with no lock and no transaction held,
/// *then* stock and audit commit. A failure at the paper step leaves the order
/// in `PRINTING` on purpose — that is the state the recovery commands below
/// exist to resolve, and inventing an outcome for it here would be forging an
/// authorisation nobody gave.
pub fn place_order(state: &AppState, tab_id: i64, lines: &[OrderLine]) -> Result<PlacedOrder> {
    let session = require_cashier(state)?;
    let now = now_ms();
    if lines.is_empty() {
        return Err(CommandError::refused(
            "An order needs at least one drink on it.",
        ));
    }
    if lines.iter().any(|line| line.quantity_milli <= 0) {
        return Err(CommandError::refused(
            "Every line on an order needs a quantity above zero.",
        ));
    }

    let (order_id, slips) = state.with_db_mut(|conn| -> Result<(i64, Vec<SlipLine>)> {
        let tab = tabs::find(conn, tab_id)?;
        if tab.status != tabs::Status::Open {
            return Err(CommandError::refused("That tab is closed."));
        }
        let shift = shifts::active(conn)?
            .ok_or_else(|| CommandError::refused("No trading night is open."))?;
        if shift.status != shifts::Status::Open {
            return Err(CommandError::refused(
                "This night is being closed, so it cannot take another order.",
            ));
        }

        let order = {
            let transaction = conn.transaction()?;
            let order = orders::create(
                &transaction,
                orders::NewDraft {
                    tab_id,
                    shift_id: shift.id,
                    cashier_id: session.staff_id,
                    created_at: now,
                },
            )?;
            for line in lines {
                orders::add_line(
                    &transaction,
                    order.id,
                    line.sale_item_id,
                    Milli::from_thousandths(line.quantity_milli),
                )?;
            }
            transaction.commit()?;
            order
        };

        let prepared = match trading::prepare_issue(conn, order.id, session.staff_id, now) {
            Ok(prepared) => prepared,
            Err(error) => {
                // Usually not enough stock. The refused draft must not be left
                // in the table for somebody to find later and wonder about.
                let _ = orders::abandon(conn, order.id);
                return Err(error.into());
            }
        };

        let mut slips = Vec::with_capacity(prepared.receipts.len());
        for receipt in &prepared.receipts {
            let text = receipt.rendered_text.clone().ok_or_else(|| {
                CommandError::of("DATABASE", "a reserved slip has no frozen text")
            })?;
            slips.push(SlipLine {
                receipt_number: receipt.receipt_number.clone(),
                destination: receipt
                    .destination
                    .map(|destination| destination.as_str().to_owned())
                    .unwrap_or_default(),
                text,
            });
        }
        Ok((order.id, slips))
    })?;

    // Device I/O, with no database lock held: the durable PRINTING reservation
    // committed above is what holds the stock across this gap.
    let directory = state.with_db(slip_directory)?;
    if let Err(error) = spool(&directory, &slips) {
        return Err(CommandError::of(
            "PRINT_PENDING",
            format!(
                "The slips did not print: {error}. Those numbers are still reserved — write the \
                 chit by hand and confirm it, or abandon the numbers and ring it again."
            ),
        ));
    }

    state.with_db_mut(|conn| -> Result<PlacedOrder> {
        trading::confirm_issued(conn, order_id, session.staff_id, now)?;
        let settings = Settings::load(conn)?;
        Ok(PlacedOrder {
            order_id,
            slips,
            tab_total: settings.format_money(tabs::running_total(conn, tab_id)?),
        })
    })
}

// ---------------------------------------------------------------------------
// Paper
// ---------------------------------------------------------------------------

/// Where slips spool: beside the venue's own database file.
///
/// That is the one location this venue has already chosen and already backs
/// up, so it needs no setting of its own and nothing to configure on site.
pub(crate) fn slip_directory(conn: &Connection) -> Result<PathBuf> {
    let file: String = conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    let file = file.trim();
    if file.is_empty() {
        return Err(CommandError::of(
            "NO_PRINTER",
            "This till is running on a temporary database, so there is nowhere to send a slip.",
        ));
    }
    Ok(Path::new(file)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("slips"))
}

pub(crate) fn spool(directory: &Path, slips: &[SlipLine]) -> printing::Result<()> {
    let mut printer = printing::FilePrinter::new(directory);
    printer.status()?;
    for slip in slips {
        printer.print(&printing::escpos_text(&slip.text)?)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The Tauri surface. Thin on purpose — the logic above is what is tested.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn cmd_floor_view(state: tauri::State<'_, AppState>) -> Result<FloorView> {
    floor_view(&state)
}

#[tauri::command]
pub fn cmd_inventory_view(state: tauri::State<'_, AppState>) -> Result<InventoryView> {
    inventory_view(&state)
}

#[tauri::command]
pub fn cmd_open_shift(
    state: tauri::State<'_, AppState>,
    opening_float: String,
) -> Result<FloorView> {
    open_shift(&state, &opening_float)
}

#[tauri::command]
pub fn cmd_open_tab(
    state: tauri::State<'_, AppState>,
    waiter_id: i64,
    reference: String,
    contact: Option<String>,
) -> Result<FloorView> {
    open_tab(&state, waiter_id, &reference, contact.as_deref())
}

#[tauri::command]
pub fn cmd_place_order(
    state: tauri::State<'_, AppState>,
    tab_id: i64,
    lines: Vec<OrderLine>,
) -> Result<PlacedOrder> {
    place_order(&state, tab_id, &lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture;

    struct Till {
        state: AppState,
        directory: PathBuf,
        waiter: i64,
        beer: i64,
        beer_bottle: i64,
    }

    impl Drop for Till {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// A till on a real file, because slips spool beside the database and an
    /// in-memory database has nowhere to put them.
    fn till(name: &str) -> Till {
        let directory =
            std::env::temp_dir().join(format!("servepoint-floor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let conn = crate::db::open(directory.join("servepoint.db")).unwrap();

        let owner = fixture::staff(&conn, "OWN-F", "Selam", "OWNER");
        let cashier = fixture::staff(&conn, "CSH-F", "Abel", "CASHIER");
        let waiter = fixture::staff(&conn, "WTR-F", "Sara", "WAITER");
        let beer = fixture::product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);
        let beer_bottle = fixture::sale_item(&conn, "S-BEER", "Beer", "Bottles", 5_000);
        fixture::recipe(&conn, beer_bottle, &[(beer, 1_000)]);
        fixture::stock_up(&conn, beer, 2_000, owner);

        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: cashier,
            code: "CSH-F".into(),
            name: "Abel".into(),
            role: "CASHIER".into(),
        }));
        Till {
            state,
            directory,
            waiter,
            beer,
            beer_bottle,
        }
    }

    fn one_beer(till: &Till) -> Vec<OrderLine> {
        vec![OrderLine {
            sale_item_id: till.beer_bottle,
            quantity_milli: 1_000,
        }]
    }

    fn trading_tab(till: &Till) -> i64 {
        open_shift(&till.state, "500").unwrap();
        let view = open_tab(&till.state, till.waiter, "7", None).unwrap();
        view.tabs[0].id
    }

    #[test]
    fn a_night_opens_once_and_its_float_reaches_the_chain() {
        let till = till("shift");
        let view = open_shift(&till.state, "500.50").unwrap();
        assert!(view.shift.is_some(), "the night is open");

        // A second night, and an unreadable float, are both refused.
        assert_eq!(open_shift(&till.state, "100").unwrap_err().kind, "REFUSED");
        assert_eq!(
            open_shift(&till.state, "1,000").unwrap_err().kind,
            "REFUSED"
        );

        let logged: i64 = till.state.with_db(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'SHIFT_OPENED'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        });
        assert_eq!(logged, 1);
    }

    #[test]
    fn an_order_prints_a_numbered_slip_and_draws_the_stock_down() {
        let till = till("order");
        let tab = trading_tab(&till);

        let placed = place_order(&till.state, tab, &one_beer(&till)).unwrap();
        assert_eq!(placed.slips.len(), 1);
        let slip = &placed.slips[0];
        assert_eq!(slip.destination, "BAR");
        // The defect this whole layer was blocked on: the number is on the paper.
        assert!(slip.receipt_number.starts_with("BR-"));
        assert!(slip.text.contains(&slip.receipt_number), "{}", slip.text);
        assert!(slip.text.contains("Table 7") && slip.text.contains("Sara"));
        assert_eq!(placed.tab_total, "50.00");

        let spooled = std::fs::read(till.directory.join("slips").join("print-000001.bin")).unwrap();
        assert!(String::from_utf8_lossy(&spooled).contains(&slip.receipt_number));

        let on_hand = till
            .state
            .with_db(|conn| stock::on_hand(conn, till.beer).unwrap());
        assert_eq!(on_hand, Milli::ONE);

        // A second order spools beside the first rather than colliding with it.
        let second = place_order(&till.state, tab, &one_beer(&till)).unwrap();
        assert_ne!(second.slips[0].receipt_number, slip.receipt_number);
        assert!(till
            .directory
            .join("slips")
            .join("print-000002.bin")
            .exists());
    }

    #[test]
    fn an_order_short_of_stock_is_refused_and_leaves_no_draft_behind() {
        let till = till("shortage");
        let tab = trading_tab(&till);
        let too_many = vec![OrderLine {
            sale_item_id: till.beer_bottle,
            quantity_milli: 9_000,
        }];

        let refused = place_order(&till.state, tab, &too_many).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(
            refused.message.contains("Not enough stock"),
            "{}",
            refused.message
        );

        let (left, on_hand) = till.state.with_db(|conn| {
            let left: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM orders WHERE status IN ('DRAFT','PRINTING')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (left, stock::on_hand(conn, till.beer).unwrap())
        });
        assert_eq!(left, 0, "a refused order left a draft behind");
        assert_eq!(on_hand, Milli::from_thousandths(2_000));
    }

    #[test]
    fn only_a_signed_in_cashier_can_trade() {
        let till = till("permission");
        till.state.set_session(None);
        assert_eq!(floor_view(&till.state).unwrap_err().kind, "SIGNED_OUT");

        till.state.set_session(Some(Session {
            staff_id: 1,
            code: "OWN-F".into(),
            name: "Selam".into(),
            role: "OWNER".into(),
        }));
        // An owner may look at the floor, but may not trade on it.
        floor_view(&till.state).unwrap();
        assert_eq!(
            open_shift(&till.state, "500").unwrap_err().kind,
            "NOT_PERMITTED"
        );
    }
}
