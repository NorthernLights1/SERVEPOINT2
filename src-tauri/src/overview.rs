//! Tonight, as it stands. Read-only.
//!
//! Two figures, never added together: what has been **billed** on tabs that
//! are settled, and what is **still running** on tabs that are open. Summing
//! them would present money nobody has been asked for yet as though it were
//! taken, which is exactly the kind of number that makes an owner stop
//! trusting a screen.
//!
//! Nothing here is stored. Every figure is a sum over the ledgers that
//! produced it, so it cannot drift away from them.

use rusqlite::Connection;

use crate::commands::{require_session, CommandError, ShiftView};
use crate::repo::{shifts, stock, tabs};
use crate::settings::Settings;
use crate::state::AppState;
use crate::{Milli, Money};

type Result<T> = std::result::Result<T, CommandError>;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedAmount {
    pub name: String,
    pub amount: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedQuantity {
    pub name: String,
    pub quantity: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LowStockLine {
    pub name: String,
    pub on_hand: String,
    pub unit: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewView {
    pub shift: Option<ShiftView>,
    /// Billed on tabs that have been settled tonight.
    pub settled: String,
    /// Still running on tabs nobody has asked to pay yet.
    pub open_tabs: String,
    pub tabs_settled: usize,
    pub tabs_open: usize,
    /// Who has taken the most tonight. `None` until somebody has.
    pub top_waiter: Option<NamedAmount>,
    /// What is moving fastest, by how much of it has gone out.
    pub top_sellers: Vec<NamedQuantity>,
    pub low_stock: Vec<LowStockLine>,
    /// True when nothing has happened yet, so the screen can say so plainly
    /// rather than showing a row of zeroes.
    pub quiet: bool,
}

fn view_of(conn: &Connection, now: i64) -> Result<OverviewView> {
    let settings = Settings::load(conn)?;
    let shift_id = shifts::active(conn)?.map(|open| open.id);

    let settled_minor: i64 = match shift_id {
        Some(id) => conn.query_row(
            "SELECT COALESCE(SUM(total_minor), 0) FROM tab_payments WHERE shift_id = ?1",
            [id],
            |row| row.get(0),
        )?,
        None => 0,
    };
    let tabs_settled: i64 = match shift_id {
        Some(id) => conn.query_row(
            "SELECT COUNT(*) FROM tab_payments WHERE shift_id = ?1",
            [id],
            |row| row.get(0),
        )?,
        None => 0,
    };

    // Open tabs are totalled through the same function the bill uses, so the
    // running figure here and the one on the till cannot disagree.
    let open_ids = {
        let mut statement = conn.prepare("SELECT id FROM tabs WHERE status = 'OPEN'")?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut open_total = Money::ZERO;
    for tab_id in &open_ids {
        open_total = open_total.checked_add(tabs::running_total(conn, *tab_id)?)?;
    }

    let top_waiter = match shift_id {
        Some(id) => conn
            .query_row(
                "SELECT s.full_name, SUM(p.total_minor)
                   FROM tab_payments p JOIN staff s ON s.id = p.waiter_id
                  WHERE p.shift_id = ?1
                  GROUP BY p.waiter_id
                  ORDER BY 2 DESC, s.full_name
                  LIMIT 1",
                [id],
                |row| {
                    Ok(NamedAmount {
                        name: row.get(0)?,
                        amount: settings.format_money(Money::from_minor(row.get(1)?)),
                    })
                },
            )
            .ok(),
        None => None,
    };

    let top_sellers = match shift_id {
        Some(id) => {
            let mut statement = conn.prepare(
                "SELECT l.sale_item_name, SUM(l.quantity_milli)
                   FROM order_lines l JOIN orders o ON o.id = l.order_id
                  WHERE o.shift_id = ?1 AND o.status = 'ISSUED'
                  GROUP BY l.sale_item_name
                  ORDER BY 2 DESC, l.sale_item_name
                  LIMIT 5",
            )?;
            let rows = statement.query_map([id], |row| {
                Ok(NamedQuantity {
                    name: row.get(0)?,
                    quantity: Milli::from_thousandths(row.get(1)?).to_display(),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        }
        None => Vec::new(),
    };

    let low_stock: Vec<LowStockLine> = stock::levels(conn)?
        .into_iter()
        .filter(stock::Level::is_low)
        .map(|level| LowStockLine {
            name: level.name,
            on_hand: level.on_hand.to_display(),
            unit: level.base_unit,
        })
        .collect();

    Ok(OverviewView {
        shift: crate::commands::open_shift_of(conn, now)?,
        settled: settings.format_money(Money::from_minor(settled_minor)),
        open_tabs: settings.format_money(open_total),
        tabs_settled: tabs_settled as usize,
        tabs_open: open_ids.len(),
        quiet: tabs_settled == 0 && open_ids.is_empty(),
        top_waiter,
        top_sellers,
        low_stock,
    })
}

pub fn overview_view(state: &AppState) -> Result<OverviewView> {
    require_session(state)?;
    let now = chrono::Utc::now().timestamp_millis();
    state.with_db(|conn| view_of(conn, now))
}

#[tauri::command]
pub fn cmd_overview_view(state: tauri::State<'_, AppState>) -> Result<OverviewView> {
    overview_view(&state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Session;
    use crate::{bills, floor, repo::fixture};

    struct Bar {
        state: AppState,
        directory: std::path::PathBuf,
        waiter: i64,
        bottle: i64,
    }

    impl Drop for Bar {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn bar(name: &str) -> Bar {
        let directory =
            std::env::temp_dir().join(format!("servepoint-overview-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let conn = crate::db::open(directory.join("servepoint.db")).unwrap();

        let owner = fixture::staff(&conn, "OWN-O", "Selam", "OWNER");
        let cashier = fixture::staff(&conn, "CSH-O", "Abel", "CASHIER");
        let waiter = fixture::staff(&conn, "WTR-O", "Sara", "WAITER");
        let beer = fixture::product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);
        let bottle = fixture::sale_item(&conn, "S-BEER", "Beer", "Bottles", 5_000);
        fixture::recipe(&conn, bottle, &[(beer, 1_000)]);
        // Two bottles in, against a low-stock threshold of three: the shelf is
        // low from the moment it is counted.
        fixture::stock_up(&conn, beer, 2_000, owner);

        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: cashier,
            code: "CSH-O".into(),
            name: "Abel".into(),
            role: "CASHIER".into(),
        }));
        Bar {
            state,
            directory,
            waiter,
            bottle,
        }
    }

    fn one_beer(bar: &Bar) -> [floor::OrderLine; 1] {
        [floor::OrderLine {
            sale_item_id: bar.bottle,
            quantity_milli: 1_000,
        }]
    }

    #[test]
    fn a_quiet_night_says_nothing_rather_than_showing_zeroes() {
        let bar = bar("quiet");
        floor::open_shift(&bar.state, "500").unwrap();
        let view = overview_view(&bar.state).unwrap();
        assert!(view.quiet);
        assert!(view.top_waiter.is_none());
        assert!(view.top_sellers.is_empty());
        assert_eq!(view.tabs_open, 0);
    }

    #[test]
    fn what_is_billed_and_what_is_still_running_are_kept_apart() {
        let bar = bar("trading");
        floor::open_shift(&bar.state, "500").unwrap();

        // One tab settled, one still open.
        let first = floor::open_tab(&bar.state, bar.waiter, "7", None)
            .unwrap()
            .tabs[0]
            .id;
        floor::place_order(&bar.state, first, &one_beer(&bar)).unwrap();
        bills::settle_tab(&bar.state, first, None, None).unwrap();

        let second = floor::open_tab(&bar.state, bar.waiter, "8", None)
            .unwrap()
            .tabs
            .iter()
            .find(|tab| tab.label == "Table 8")
            .unwrap()
            .id;
        floor::place_order(&bar.state, second, &one_beer(&bar)).unwrap();

        let view = overview_view(&bar.state).unwrap();
        assert!(!view.quiet);
        // Billed, with the venue's default 10% service charge on top.
        assert_eq!(view.settled, "55.00");
        assert_eq!(view.tabs_settled, 1);
        // Still running, at menu price — nobody has been asked for it yet.
        assert_eq!(view.open_tabs, "50.00");
        assert_eq!(view.tabs_open, 1);

        assert_eq!(view.top_waiter.unwrap().name, "Sara");
        assert_eq!(view.top_sellers[0].name, "Beer");
        assert_eq!(view.top_sellers[0].quantity, "2");
        // Both bottles have gone; the shelf is below its threshold.
        assert_eq!(view.low_stock[0].name, "Beer");
        assert_eq!(view.low_stock[0].on_hand, "0");
    }
}
