//! Settling a tab — what the customer is shown, and what they then pay.
//!
//! Two different figures live here and they must not be confused. Before the
//! tab closes, the bill is *calculated* by the same `Bill` code that prices
//! everything else. Once it closes, the figure is *frozen* into a payment row,
//! and from that moment this module reports the frozen one. A total that gets
//! recalculated after the customer has paid is a total that can quietly change
//! months later.
//!
//! `settlement::close_tab` owns the rules and the transaction. This is only
//! the window onto it.

use rusqlite::Connection;

use crate::bill::Bill;
use crate::commands::CommandError;
use crate::floor::require_till;
use crate::repo::{shifts, staff, tabs};
use crate::settings::{keys, Settings};
use crate::settlement::{self, CloseTab};
use crate::state::AppState;

type Result<T> = std::result::Result<T, CommandError>;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What the customer is about to be asked for, before anything is frozen.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillView {
    pub tab_id: i64,
    pub code: String,
    pub label: String,
    pub waiter: String,
    /// The menu total of everything issued to this tab.
    pub line_total: String,
    /// What prints as "Subtotal". Under inclusive tax this is *less* than the
    /// line total, which is the case that confuses people.
    pub net: String,
    pub service_charge: String,
    pub tax: String,
    pub total: String,
    pub service_label: String,
    pub tax_label: String,
    pub show_service: bool,
    pub show_tax: bool,
    pub tax_extracted: bool,
    /// Whether this venue permits a bill to be written off at all.
    pub comps_enabled: bool,
    /// Whether this venue asks for the customer's TIN as the tab closes.
    pub asks_customer_tin: bool,
}

/// The frozen bill, read back after the tab has closed.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledBill {
    pub tab_id: i64,
    pub code: String,
    pub label: String,
    pub waiter: String,
    pub subtotal: String,
    pub service_charge: String,
    pub tax: String,
    pub total: String,
    /// What the waiter now owes the till for this tab. Zero on a comped bill.
    pub liability: String,
    pub comped: bool,
    pub comp_reason: Option<String>,
}

fn bill_of(conn: &Connection, tab_id: i64) -> Result<BillView> {
    let tab = tabs::find(conn, tab_id)?;
    if tab.status != tabs::Status::Open {
        return Err(CommandError::refused("That tab is already closed."));
    }
    let waiter = staff::find(conn, tab.waiter_id)?;
    let settings = Settings::load(conn)?;
    let config = settings.charge_config();
    let bill = Bill::calculate(tabs::running_total(conn, tab_id)?, &config)?;

    Ok(BillView {
        tab_id,
        code: tab.code,
        label: tab.display_label,
        waiter: waiter.name,
        line_total: settings.format_money(bill.line_total),
        net: settings.format_money(bill.net),
        service_charge: settings.format_money(bill.service_charge),
        tax: settings.format_money(bill.tax),
        total: settings.format_money(bill.total),
        service_label: format!("Service charge {}", bill.service_rate),
        tax_label: format!("VAT {}", bill.tax_rate),
        show_service: config.service_enabled && !bill.service_rate.is_zero(),
        show_tax: config.tax_enabled && !bill.tax_rate.is_zero(),
        tax_extracted: bill.tax_inclusive,
        comps_enabled: settings.flag(keys::COMPS_ENABLED),
        asks_customer_tin: settings.flag(keys::TAX_ENABLED)
            && settings.flag(keys::TABS_ASK_CUSTOMER_TIN),
    })
}

/// What this tab owes right now. Readable by anyone signed in — showing a
/// customer their bill is not a privileged act.
pub fn tab_bill(state: &AppState, tab_id: i64) -> Result<BillView> {
    crate::commands::require_session(state)?;
    state.with_db(|conn| bill_of(conn, tab_id))
}

/// Close the tab and freeze what is owed.
///
/// `comp_reason` present means an authorised write-off, which the venue must
/// have enabled and which is never allowed to be blank. `customer_tin` is the
/// close-time capture, available only where tax and the prompt are both on.
pub fn settle_tab(
    state: &AppState,
    tab_id: i64,
    comp_reason: Option<&str>,
    customer_tin: Option<&str>,
) -> Result<SettledBill> {
    let session = require_till(state)?;
    let now = now_ms();
    // An empty box on screen is not a reason. Treat it as no comp at all,
    // rather than sending a blank string down to be refused.
    let comp_reason = comp_reason.map(str::trim).filter(|text| !text.is_empty());
    let customer_tin = customer_tin.map(str::trim).filter(|text| !text.is_empty());

    state.with_db_mut(|conn| {
        let shift = shifts::active(conn)?
            .ok_or_else(|| CommandError::refused("No trading night is open."))?;
        let closed = settlement::close_tab(
            conn,
            &CloseTab {
                tab_id,
                shift_id: shift.id,
                cashier_id: session.staff_id,
                comp_reason,
                customer_tin,
                closed_at: now,
            },
        )?;
        let settings = Settings::load(conn)?;
        let waiter = staff::find(conn, closed.payment.waiter_id)?;
        let payment = closed.payment;
        Ok(SettledBill {
            tab_id,
            code: closed.tab.code,
            label: closed.tab.display_label,
            waiter: waiter.name,
            subtotal: settings.format_money(payment.subtotal),
            service_charge: settings.format_money(payment.service_charge),
            tax: settings.format_money(payment.tax),
            total: settings.format_money(payment.total),
            liability: settings.format_money(payment.liability),
            comped: payment.is_comped,
            comp_reason: payment.comp_reason,
        })
    })
}

#[tauri::command]
pub fn cmd_tab_bill(state: tauri::State<'_, AppState>, tab_id: i64) -> Result<BillView> {
    tab_bill(&state, tab_id)
}

#[tauri::command]
pub fn cmd_settle_tab(
    state: tauri::State<'_, AppState>,
    tab_id: i64,
    comp_reason: Option<String>,
    customer_tin: Option<String>,
) -> Result<SettledBill> {
    settle_tab(
        &state,
        tab_id,
        comp_reason.as_deref(),
        customer_tin.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floor;
    use crate::repo::fixture;
    use crate::state::Session;
    use crate::Milli;

    struct Till {
        state: AppState,
        directory: std::path::PathBuf,
        tab: i64,
    }

    impl Drop for Till {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// A till with one beer already issued to one tab — the only state from
    /// which a tab is allowed to close.
    fn traded(name: &str) -> Till {
        let directory =
            std::env::temp_dir().join(format!("servepoint-bills-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let conn = crate::db::open(directory.join("servepoint.db")).unwrap();

        let owner = fixture::staff(&conn, "OWN-B", "Selam", "OWNER");
        let cashier = fixture::staff(&conn, "CSH-B", "Abel", "CASHIER");
        let waiter = fixture::staff(&conn, "WTR-B", "Sara", "WAITER");
        let beer = fixture::product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);
        let bottle = fixture::sale_item(&conn, "S-BEER", "Beer", "Bottles", 5_000);
        fixture::recipe(&conn, bottle, &[(beer, 1_000)]);
        fixture::stock_up(&conn, beer, 5_000, owner);

        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: cashier,
            code: "CSH-B".into(),
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

        Till {
            state,
            directory,
            tab,
        }
    }

    #[test]
    fn a_bill_is_shown_then_frozen_and_the_tab_closes_once() {
        let till = traded("close");
        let bill = tab_bill(&till.state, till.tab).unwrap();
        assert_eq!(bill.line_total, "50.00");
        assert_eq!(bill.label, "Table 7");
        assert_eq!(bill.waiter, "Sara");

        let settled = settle_tab(&till.state, till.tab, None, None).unwrap();
        assert!(!settled.comped);
        assert_eq!(settled.total, bill.total);
        // What the waiter now owes the till is the money actually taken.
        assert_eq!(settled.liability, settled.total);

        // Closed once and only once; the bill is no longer offered.
        assert_eq!(
            settle_tab(&till.state, till.tab, None, None)
                .unwrap_err()
                .kind,
            "REFUSED"
        );
        assert_eq!(tab_bill(&till.state, till.tab).unwrap_err().kind, "REFUSED");
    }

    #[test]
    fn a_write_off_needs_the_venue_to_allow_it() {
        let till = traded("comp");
        // Comps are off by default, so an authorised-looking reason is still
        // refused rather than quietly written off.
        let refused = settle_tab(&till.state, till.tab, Some("Owner's table"), None).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(
            refused.message.contains("not enabled"),
            "{}",
            refused.message
        );

        till.state.with_db(|conn| {
            crate::settings::put(conn, keys::COMPS_ENABLED, "1", None, 0).unwrap();
        });
        let settled = settle_tab(&till.state, till.tab, Some("Owner's table"), None).unwrap();
        assert!(settled.comped);
        assert_eq!(settled.comp_reason.as_deref(), Some("Owner's table"));
        // Nothing is owed on a bill nobody paid.
        assert_eq!(settled.liability, "0.00");
        assert_ne!(settled.total, "0.00");
    }

    #[test]
    fn a_blank_comp_reason_settles_normally_rather_than_being_refused() {
        let till = traded("blank");
        let settled = settle_tab(&till.state, till.tab, Some("   "), None).unwrap();
        assert!(!settled.comped);
    }
}
