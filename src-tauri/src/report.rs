//! What a night came to, frozen at the moment it closed (§9.3).
//!
//! Every other view in this system is a live sum over the ledgers, because
//! `lib.rs` rule 1 says a cached figure drifts away from the transactions that
//! produced it. **A shift report is the deliberate exception**, and the schema
//! is explicit about why: it is the night's sole fraud-control document, and a
//! document that recalculates itself is not evidence.
//!
//! The exception costs nothing in accuracy, because a closed night cannot
//! move. `order_corrections` is refused outside its own OPEN shift by trigger,
//! so there are no prior-period restatements to miss — which is exactly what
//! `0004_orders.sql` means when it says that one constraint "makes every shift
//! report self-contained".
//!
//! So the figures are computed once, here, from the ledgers, and stored twice:
//! as this struct's JSON and as the exact text that goes on paper. **Money in
//! the stored JSON is already formatted**, for the same reason it is formatted
//! at every other boundary — a report re-rendered next year, after somebody
//! sets a currency code, would no longer match the paper that was signed.
//! Counts, quantities and names stay as themselves, so the row is still worth
//! querying.

use rusqlite::Connection;

use crate::commands::{require_owner, CommandError};
use crate::repo::{cash, reports, shifts, staff};
use crate::settings::{keys, Settings};
use crate::state::AppState;
use crate::{calendar, Milli, Money};

type Result<T> = std::result::Result<T, CommandError>;

/// The divider the rest of the paper in this system already uses.
const RULE: &str = "--------------------------------";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaiterLine {
    pub name: String,
    pub expected: String,
    pub cash: String,
    pub non_cash: String,
    pub written_off: String,
    pub shortfall: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemLine {
    pub name: String,
    pub quantity: String,
    pub value: String,
}

/// One night, whole. Serialized straight into `shift_reports.report_json` and
/// handed to the screen unchanged when it is read back.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftReport {
    pub venue_name: String,
    pub shift_code: String,
    pub business_date: String,
    pub business_date_label: String,
    pub opened_at_label: String,
    pub opened_by: String,
    pub closed_at_label: String,
    pub closed_by: String,

    // What was billed. Never added to the drawer figures below — they answer
    // different questions and summing them double-counts the night.
    pub tabs_settled: i64,
    pub gross_sales: String,
    pub service_charge: String,
    pub tax: String,
    pub total_billed: String,

    // What the drawer should hold, and what was actually in it.
    pub opening_float: String,
    pub cash_from_waiters: String,
    pub other_movements: String,
    pub expected_cash: String,
    pub counted_cash: String,
    pub variance: String,
    pub over: bool,
    pub balanced: bool,

    /// Money that never reached the drawer: settled by card or transfer, or
    /// forgiven. Both belong on the report precisely because they are not in
    /// it to be counted.
    pub non_cash: String,
    pub written_off: String,

    pub waiters: Vec<WaiterLine>,
    pub items: Vec<ItemLine>,

    // The exception block. Every figure here is something a person has to be
    // able to answer for.
    pub comped_tabs: i64,
    pub comped_value: String,
    pub corrections: i64,
    pub voids: i64,
}

/// Read every figure for a night out of the ledgers.
///
/// Called once, inside the transaction that closes the shift, and never again
/// for that night.
pub fn compile(conn: &Connection, shift_id: i64) -> Result<ShiftReport> {
    let settings = Settings::load(conn)?;
    let shift = shifts::find(conn, shift_id)?;
    let money = |minor: i64| settings.format_money(Money::from_minor(minor));

    let (tabs_settled, gross, service, tax, billed, comped_tabs, comped_value) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(subtotal_minor), 0),
                COALESCE(SUM(service_charge_minor), 0),
                COALESCE(SUM(tax_minor), 0),
                COALESCE(SUM(total_minor), 0),
                COALESCE(SUM(is_comped), 0),
                COALESCE(SUM(CASE WHEN is_comped = 1 THEN total_minor ELSE 0 END), 0)
           FROM tab_payments WHERE shift_id = ?1",
        [shift_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;

    let (float_minor, from_waiters, other_movements) = conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN movement_type = 'OPENING_FLOAT'
                                  THEN amount_minor ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN movement_type = 'RECONCILIATION'
                                  THEN amount_minor ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN movement_type IN ('PAYOUT','ADJUSTMENT')
                                  THEN amount_minor ELSE 0 END), 0)
           FROM cash_movements WHERE shift_id = ?1",
        [shift_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;

    // The one figure the close screen also showed. Read through the same
    // function so the report and the count the cashier worked against cannot
    // disagree.
    let expected = cash::expected_cash(conn, shift_id)?;
    let counted = shift.counted_cash.unwrap_or(Money::ZERO);
    let over = counted.minor() >= expected.minor();
    let variance = if over {
        counted.checked_sub(expected)?
    } else {
        expected.checked_sub(counted)?
    };

    let settled = {
        let mut statement = conn.prepare(
            "SELECT s.full_name,
                    COALESCE(SUM(r.expected_minor), 0),
                    COALESCE(SUM(r.cash_minor), 0),
                    COALESCE(SUM(r.non_cash_minor), 0),
                    COALESCE(SUM(r.written_off_minor), 0),
                    COALESCE(SUM(r.shortfall_minor), 0)
               FROM reconciliations r JOIN staff s ON s.id = r.waiter_id
              WHERE r.shift_id = ?1 AND r.finalized_at IS NOT NULL
              GROUP BY r.waiter_id
              ORDER BY s.full_name",
        )?;
        let rows = statement.query_map([shift_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let non_cash: i64 = settled.iter().map(|line| line.3).sum();
    let written_off: i64 = settled.iter().map(|line| line.4).sum();
    let waiters = settled
        .into_iter()
        .map(
            |(name, expected, cash, non_cash, written_off, shortfall)| WaiterLine {
                name,
                expected: money(expected),
                cash: money(cash),
                non_cash: money(non_cash),
                written_off: money(written_off),
                shortfall: money(shortfall),
            },
        )
        .collect();

    // ISSUED only, so a corrected round is counted as its replacement and a
    // voided one is not counted at all — which is what actually left the bar.
    let items = {
        let mut statement = conn.prepare(
            "SELECT l.sale_item_name, SUM(l.quantity_milli), SUM(l.line_total_minor)
               FROM order_lines l JOIN orders o ON o.id = l.order_id
              WHERE o.shift_id = ?1 AND o.status = 'ISSUED'
              GROUP BY l.sale_item_name
              ORDER BY 3 DESC, l.sale_item_name",
        )?;
        let rows = statement.query_map([shift_id], |row| {
            Ok(ItemLine {
                name: row.get(0)?,
                quantity: Milli::from_thousandths(row.get::<_, i64>(1)?).to_display(),
                value: money(row.get::<_, i64>(2)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let (corrections, voids) = conn.query_row(
        "SELECT COALESCE(SUM(correction_type = 'CORRECTION'), 0),
                COALESCE(SUM(correction_type = 'VOID'), 0)
           FROM order_corrections WHERE shift_id = ?1",
        [shift_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;

    let named = |id: Option<i64>| -> Result<String> {
        match id {
            Some(id) => Ok(staff::find(conn, id)?.name),
            None => Ok(String::new()),
        }
    };
    let business_date_label = calendar::describe(
        chrono::NaiveDate::parse_from_str(&shift.business_date, "%Y-%m-%d")
            .map_err(|_| CommandError::of("DATABASE", "that night has no readable date"))?,
    );

    Ok(ShiftReport {
        venue_name: settings.text(keys::BUSINESS_NAME).to_owned(),
        shift_code: shift.code,
        business_date: shift.business_date,
        business_date_label,
        opened_at_label: calendar::clock_label(shift.opened_at),
        opened_by: named(Some(shift.opened_by))?,
        closed_at_label: shift
            .closed_at
            .map(calendar::clock_label)
            .unwrap_or_default(),
        closed_by: named(shift.closed_by)?,

        tabs_settled,
        gross_sales: money(gross),
        service_charge: money(service),
        tax: money(tax),
        total_billed: money(billed),

        opening_float: money(float_minor),
        cash_from_waiters: money(from_waiters),
        other_movements: money(other_movements),
        expected_cash: settings.format_money(expected),
        counted_cash: settings.format_money(counted),
        variance: settings.format_money(variance),
        balanced: variance.is_zero(),
        over,

        non_cash: money(non_cash),
        written_off: money(written_off),

        waiters,
        items,

        comped_tabs,
        comped_value: money(comped_value),
        corrections,
        voids,
    })
}

/// The paper. Plain text in the same shape as the receipts, so a venue that
/// switches report printing on gets something that looks like the rest of the
/// night's paper rather than a screen dump.
pub fn render(report: &ShiftReport) -> String {
    let mut out = String::new();
    if !report.venue_name.trim().is_empty() {
        out.push_str(report.venue_name.trim());
        out.push('\n');
    }
    out.push_str(&format!(
        "SHIFT REPORT\n{}  {}\n",
        report.shift_code, report.business_date_label
    ));
    out.push_str(&format!(
        "Opened {} by {}\nClosed {} by {}\n",
        report.opened_at_label, report.opened_by, report.closed_at_label, report.closed_by
    ));

    out.push_str(&format!("{RULE}\nSALES\n"));
    out.push_str(&format!("Tabs settled: {}\n", report.tabs_settled));
    for (label, value) in [
        ("Gross sales", &report.gross_sales),
        ("Service", &report.service_charge),
        ("Tax", &report.tax),
        ("Total billed", &report.total_billed),
    ] {
        out.push_str(&format!("{label}: {value}\n"));
    }

    out.push_str(&format!("{RULE}\nDRAWER\n"));
    for (label, value) in [
        ("Opening float", &report.opening_float),
        ("Cash from waiters", &report.cash_from_waiters),
        ("Other movements", &report.other_movements),
        ("Expected", &report.expected_cash),
        ("Counted", &report.counted_cash),
    ] {
        out.push_str(&format!("{label}: {value}\n"));
    }
    // Named rather than signed, because "-10.00" on a drawer line has been
    // read both ways by different people and only one of them is right.
    out.push_str(&match (report.balanced, report.over) {
        (true, _) => "Balanced\n".to_owned(),
        (false, true) => format!("OVER: {}\n", report.variance),
        (false, false) => format!("SHORT: {}\n", report.variance),
    });

    if !report.waiters.is_empty() {
        out.push_str(&format!("{RULE}\nWAITERS\n"));
        for waiter in &report.waiters {
            out.push_str(&format!(
                "{}: expected {}, cash {}\n",
                waiter.name, waiter.expected, waiter.cash
            ));
        }
    }

    if !report.items.is_empty() {
        out.push_str(&format!("{RULE}\nSOLD\n"));
        for item in &report.items {
            out.push_str(&format!(
                "{} x{}  {}\n",
                item.name, item.quantity, item.value
            ));
        }
    }

    out.push_str(&format!("{RULE}\nEXCEPTIONS\n"));
    out.push_str(&format!(
        "Corrections: {}\nVoids: {}\nComped tabs: {} ({})\nWritten off: {}\nNon-cash: {}\n",
        report.corrections,
        report.voids,
        report.comped_tabs,
        report.comped_value,
        report.written_off,
        report.non_cash
    ));
    out
}

/// Compile and freeze a night's report, inside the caller's transaction.
/// See `reconcile::close_night` — §4.3 makes this and the close one commit.
pub fn freeze(conn: &Connection, shift_id: i64, by: i64, at: i64) -> Result<reports::Stored> {
    let report = compile(conn, shift_id)?;
    let rendered = render(&report);
    let json = serde_json::to_string(&report).map_err(|error| {
        CommandError::of("DATABASE", format!("the report would not store: {error}"))
    })?;
    Ok(reports::store(
        conn,
        &reports::NewReport {
            shift_id,
            is_provisional: false,
            report_json: &json,
            rendered_text: &rendered,
            generated_at: at,
            generated_by: by,
        },
    )?)
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NightLine {
    pub shift_id: i64,
    pub code: String,
    pub business_date: String,
    pub business_date_label: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportsView {
    pub nights: Vec<NightLine>,
    /// The night being read. `None` only when no night has ever closed.
    pub showing: Option<ShiftReport>,
    pub showing_shift_id: Option<i64>,
    /// The exact stored paper, for whoever wants to read what was signed
    /// rather than the laid-out screen version of it.
    pub rendered_text: Option<String>,
}

fn view_of(conn: &Connection, wanted: Option<i64>) -> Result<ReportsView> {
    let closed = shifts::closed(conn)?;
    let mut nights = Vec::with_capacity(closed.len());
    for night in &closed {
        nights.push(NightLine {
            shift_id: night.id,
            code: night.code.clone(),
            business_date: night.business_date.clone(),
            business_date_label: calendar::describe(
                chrono::NaiveDate::parse_from_str(&night.business_date, "%Y-%m-%d")
                    .map_err(|_| CommandError::of("DATABASE", "that night has no readable date"))?,
            ),
        });
    }

    // Default to the most recent night, which is what somebody opening this
    // screen almost always wants.
    let showing_shift_id = match wanted {
        Some(id) if closed.iter().any(|night| night.id == id) => Some(id),
        Some(_) => return Err(CommandError::refused("That night has not been closed.")),
        None => closed.first().map(|night| night.id),
    };

    let stored = match showing_shift_id {
        Some(id) => reports::final_for(conn, id)?,
        None => None,
    };
    // Read back, never recompiled — that promise is the whole reason the row
    // exists.
    let showing = match stored.as_ref() {
        Some(row) => Some(serde_json::from_str(&row.report_json).map_err(|error| {
            CommandError::of("DATABASE", format!("that report cannot be read: {error}"))
        })?),
        None => None,
    };

    Ok(ReportsView {
        nights,
        showing,
        showing_shift_id,
        rendered_text: stored.map(|row| row.rendered_text),
    })
}

/// Owner only — the navigation gives Reports to the owner alone, and a view
/// the screen hides but the command serves is exactly the disagreement that
/// strands people.
pub fn reports_view(state: &AppState, shift_id: Option<i64>) -> Result<ReportsView> {
    require_owner(state)?;
    state.with_db(|conn| view_of(conn, shift_id))
}

#[tauri::command]
pub fn cmd_reports_view(
    state: tauri::State<'_, AppState>,
    shift_id: Option<i64>,
) -> Result<ReportsView> {
    reports_view(&state, shift_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Session;
    use crate::{bills, floor, reconcile, repo::fixture, Milli};

    struct Night {
        state: AppState,
        directory: std::path::PathBuf,
        owner: i64,
        waiter: i64,
    }

    impl Drop for Night {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn as_owner(night: &Night) {
        night.state.set_session(Some(Session {
            staff_id: night.owner,
            code: "OWN-R".into(),
            name: "Selam".into(),
            role: "OWNER".into(),
        }));
    }

    /// One beer sold, settled, the waiter squared up and the night sealed —
    /// the smallest complete night there is.
    fn night(name: &str) -> Night {
        let directory =
            std::env::temp_dir().join(format!("servepoint-report-{}-{name}", std::process::id()));
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
        let tab = floor::open_tab(&state, waiter, "7", None).unwrap().tabs[0].id;
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
            owner,
            waiter,
        }
    }

    fn seal(night: &Night, counted: &str) {
        reconcile::settle_waiter(
            &night.state,
            night.waiter,
            reconcile::Method::Cash,
            "55",
            None,
        )
        .unwrap();
        reconcile::begin_closing(&night.state).unwrap();
        reconcile::close_night(&night.state, counted).unwrap();
    }

    #[test]
    fn closing_a_night_freezes_its_report_and_the_screen_reads_it_back() {
        let night = night("frozen");
        seal(&night, "555");
        as_owner(&night);

        let view = reports_view(&night.state, None).unwrap();
        assert_eq!(view.nights.len(), 1);
        let report = view.showing.expect("a closed night has a report");

        // 50.00 of beer, plus this venue's default 10% service charge.
        assert_eq!(report.gross_sales, "50.00");
        assert_eq!(report.service_charge, "5.00");
        assert_eq!(report.total_billed, "55.00");
        assert_eq!(report.tabs_settled, 1);

        // The float, plus what Sara handed over.
        assert_eq!(report.opening_float, "500.00");
        assert_eq!(report.cash_from_waiters, "55.00");
        assert_eq!(report.expected_cash, "555.00");
        assert_eq!(report.counted_cash, "555.00");
        assert!(report.balanced);

        assert_eq!(report.waiters[0].name, "Sara");
        assert_eq!(report.waiters[0].cash, "55.00");
        assert_eq!(report.items[0].name, "Beer");
        assert_eq!(report.items[0].quantity, "1");
        assert_eq!(report.items[0].value, "50.00");

        assert!(view.rendered_text.unwrap().contains("SHIFT REPORT"));
    }

    #[test]
    fn a_short_drawer_is_named_on_the_paper_rather_than_signed() {
        let night = night("short");
        seal(&night, "540");
        as_owner(&night);

        let view = reports_view(&night.state, None).unwrap();
        let report = view.showing.unwrap();
        assert!(!report.balanced && !report.over);
        assert_eq!(report.variance, "15.00");
        assert!(view.rendered_text.unwrap().contains("SHORT: 15.00"));
    }

    #[test]
    fn a_report_is_read_back_and_never_recompiled() {
        // The proof: change what the figures would compile to, then read the
        // night again. A recalculating report would follow the setting.
        let night = night("stable");
        seal(&night, "555");
        as_owner(&night);
        let before = reports_view(&night.state, None).unwrap();

        night
            .state
            .with_db(|conn| -> Result<()> {
                crate::settings::put(conn, keys::CURRENCY_CODE, "ETB", None, 0)?;
                Ok(())
            })
            .unwrap();

        let after = reports_view(&night.state, None).unwrap();
        assert_eq!(
            before.showing.unwrap().total_billed,
            after.showing.unwrap().total_billed,
            "a stored report must not follow a setting changed afterwards"
        );
    }

    #[test]
    fn only_the_owner_may_read_a_report() {
        // The navigation hands Reports to the owner alone. The command has to
        // agree, or the screen is the only thing stopping anybody.
        let night = night("owner-only");
        seal(&night, "555");
        let refused = reports_view(&night.state, None).unwrap_err();
        assert_eq!(refused.kind, "NOT_PERMITTED");
    }

    #[test]
    fn a_night_that_never_closed_cannot_be_asked_for() {
        let night = night("unknown");
        seal(&night, "555");
        as_owner(&night);
        let refused = reports_view(&night.state, Some(9_999)).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
    }

    #[test]
    fn a_venue_that_has_never_closed_a_night_gets_an_empty_screen_not_an_error() {
        let night = night("quiet");
        as_owner(&night);
        let view = reports_view(&night.state, None).unwrap();
        assert!(view.nights.is_empty());
        assert!(view.showing.is_none());
        assert!(view.rendered_text.is_none());
    }
}
