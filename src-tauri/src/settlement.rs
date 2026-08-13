//! Atomic tab close and complimentary-bill protocol (§7.3, §7.8).

use rusqlite::{Connection, TransactionBehavior};

use crate::ledger::{self, Event};
use crate::printing::{self};
use crate::repo::{self, cash, shifts, staff, tabs};
use crate::settings::{self, keys};
use crate::Settings;

#[derive(Debug, Clone)]
pub struct CloseTab<'a> {
    pub tab_id: i64,
    pub shift_id: i64,
    pub cashier_id: i64,
    /// `Some` means an authorised complimentary bill; a blank reason is never
    /// meaningful and is refused.
    pub comp_reason: Option<&'a str>,
    /// Optional close-time D25 capture. An existing value is preserved when
    /// this is `None`.
    pub customer_tin: Option<&'a str>,
    pub closed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedTab {
    pub tab: tabs::Tab,
    pub payment: cash::TabPayment,
}

#[derive(Debug, thiserror::Error)]
pub enum SettlementError {
    #[error("{0}")]
    Repo(#[from] repo::RepoError),

    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("settings: {0}")]
    Settings(#[from] settings::SettingsError),

    #[error("audit: {0}")]
    Audit(#[from] ledger::LedgerError),

    #[error("printing: {0}")]
    Printing(#[from] printing::PrintError),
}

pub type Result<T> = std::result::Result<T, SettlementError>;

/// Freeze one tab's identity and bill with its audit row in a single commit.
pub fn close_tab(conn: &mut Connection, request: &CloseTab<'_>) -> Result<ClosedTab> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let actor = staff::find(&tx, request.cashier_id)?;
    if actor.role != staff::Role::Cashier || !actor.active {
        return Err(
            repo::RepoError::Refused("only an active cashier may close a tab".into()).into(),
        );
    }
    let shift = shifts::find(&tx, request.shift_id)?;
    if shift.status != shifts::Status::Open {
        return Err(
            repo::RepoError::Refused("a tab can close only during an open shift".into()).into(),
        );
    }
    let tab = tabs::find(&tx, request.tab_id)?;
    if tab.status != tabs::Status::Open {
        return Err(repo::RepoError::Refused("only an open tab can be closed".into()).into());
    }

    let unfinished: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM orders
              WHERE tab_id = ?1 AND status IN ('DRAFT','PRINTING')
             UNION ALL
             SELECT 1 FROM pending_order_corrections pending
              JOIN orders original ON original.id = pending.original_order_id
              WHERE original.tab_id = ?1
             UNION ALL
             SELECT 1 FROM receipt_prints attempt
              JOIN receipts receipt ON receipt.id = attempt.receipt_id
              LEFT JOIN orders issue_order ON issue_order.id = receipt.order_id
              WHERE attempt.outcome = 'UNKNOWN'
                AND (issue_order.tab_id = ?1 OR receipt.tab_id = ?1)
         )",
        [request.tab_id],
        |row| row.get(0),
    )?;
    if unfinished {
        return Err(repo::RepoError::Refused(
            "resolve every draft, print attempt, and correction before closing the tab".into(),
        )
        .into());
    }
    let has_issued: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM orders WHERE tab_id = ?1 AND status = 'ISSUED')",
        [request.tab_id],
        |row| row.get(0),
    )?;
    if !has_issued {
        return Err(repo::RepoError::Refused(
            "a tab needs at least one issued order before it can close".into(),
        )
        .into());
    }

    let settings = Settings::load(&tx)?;
    let comp_reason = request.comp_reason.map(str::trim);
    let comped = comp_reason.is_some();
    if comp_reason.is_some_and(str::is_empty) {
        return Err(repo::RepoError::Refused("a complimentary tab needs a reason".into()).into());
    }
    if comped && !settings.flag(keys::COMPS_ENABLED) {
        return Err(repo::RepoError::Refused(
            "complimentary tabs are not enabled for this venue".into(),
        )
        .into());
    }

    let customer_tin = request
        .customer_tin
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if customer_tin.is_some()
        && (!settings.flag(keys::TAX_ENABLED) || !settings.flag(keys::TABS_ASK_CUSTOMER_TIN))
    {
        return Err(repo::RepoError::Refused(
            "customer TIN capture is available only when tax and the TIN prompt are enabled".into(),
        )
        .into());
    }

    let changed = tx.execute(
        "UPDATE tabs
            SET status = 'CLOSED', closed_shift_id = ?2, closed_at = ?3,
                closed_by = ?4, is_comped = ?5,
                customer_tin = COALESCE(?6, customer_tin)
          WHERE id = ?1 AND status = 'OPEN'",
        rusqlite::params![
            request.tab_id,
            request.shift_id,
            request.closed_at,
            request.cashier_id,
            i64::from(comped),
            customer_tin,
        ],
    )?;
    if changed != 1 {
        return Err(
            repo::RepoError::Refused("the tab changed before it could close".into()).into(),
        );
    }

    let payment = cash::freeze_payment(
        &tx,
        &cash::NewTabPayment {
            tab_id: request.tab_id,
            comp_reason,
            shift_id: request.shift_id,
            created_by: request.cashier_id,
            created_at: request.closed_at,
        },
    )?;
    let facts = format!(
        "total_minor={};liability_minor={};comped={}",
        payment.total.minor(),
        payment.liability.minor(),
        i64::from(comped)
    );
    let action = if comped { "TAB_COMPED" } else { "TAB_CLOSED" };
    ledger::append(
        &tx,
        &Event::new(action, "tab", request.closed_at)
            .about(request.tab_id)
            .recording(&facts)
            .by(request.cashier_id)
            .during(Some(request.shift_id)),
    )?;

    let tab = tabs::find(&tx, request.tab_id)?;
    tx.commit()?;
    Ok(ClosedTab { tab, payment })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{fixture, orders, shifts, tabs};
    use crate::settings::{self, keys};
    use crate::{Milli, Money};

    const CLOSED_AT: i64 = fixture::NOW + 10_000;

    struct Trading {
        bar: fixture::Bar,
        shift_id: i64,
        tab_id: i64,
    }

    fn trading() -> Trading {
        let bar = fixture::bar();
        let shift = shifts::open(
            &bar.conn,
            &shifts::NewShift {
                business_date: "2025-08-01",
                opened_at: fixture::NOW,
                opened_by: bar.cashier,
                opening_float: Money::ZERO,
                expected_end_at: fixture::NOW + 43_200_000,
            },
        )
        .unwrap();
        let tab = tabs::open(
            &bar.conn,
            &tabs::NewTab {
                opened_shift_id: shift.id,
                waiter_id: bar.sara,
                reference: tabs::Reference::table("9"),
                opened_at: fixture::NOW,
                opened_by: bar.cashier,
            },
        )
        .unwrap();
        Trading {
            bar,
            shift_id: shift.id,
            tab_id: tab.id,
        }
    }

    fn issued_order(t: &Trading) {
        let order = orders::create(
            &t.bar.conn,
            orders::NewDraft {
                tab_id: t.tab_id,
                shift_id: t.shift_id,
                cashier_id: t.bar.cashier,
                created_at: fixture::NOW,
            },
        )
        .unwrap();
        orders::add_line(&t.bar.conn, order.id, t.bar.beer_bottle, Milli::ONE).unwrap();
        t.bar
            .conn
            .execute(
                "UPDATE orders SET status = 'PRINTING' WHERE id = ?1",
                [order.id],
            )
            .unwrap();
        t.bar
            .conn
            .execute(
                "UPDATE orders SET status = 'ISSUED', issued_at = ?2 WHERE id = ?1",
                rusqlite::params![order.id, fixture::NOW + 1],
            )
            .unwrap();
    }

    #[test]
    fn close_freezes_the_derived_bill_and_audit_in_one_commit() {
        let mut t = trading();
        issued_order(&t);
        let closed = close_tab(
            &mut t.bar.conn,
            &CloseTab {
                tab_id: t.tab_id,
                shift_id: t.shift_id,
                cashier_id: t.bar.cashier,
                comp_reason: None,
                customer_tin: None,
                closed_at: CLOSED_AT,
            },
        )
        .unwrap();

        assert_eq!(closed.tab.status, tabs::Status::Closed);
        assert_eq!(closed.payment.total, Money::from_minor(5_500));
        let audit: (String, i64) = t
            .bar
            .conn
            .query_row(
                "SELECT action, entity_id FROM audit_log ORDER BY sequence_no DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(audit, ("TAB_CLOSED".into(), t.tab_id));
    }

    #[test]
    fn a_refusal_rolls_back_the_tab_bill_tin_and_audit() {
        let mut t = trading();
        let before_audit: i64 = t
            .bar
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .unwrap();
        let error = close_tab(
            &mut t.bar.conn,
            &CloseTab {
                tab_id: t.tab_id,
                shift_id: t.shift_id,
                cashier_id: t.bar.cashier,
                comp_reason: None,
                customer_tin: Some("TIN-123"),
                closed_at: CLOSED_AT,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("issued order"), "got: {error}");
        let tab = tabs::find(&t.bar.conn, t.tab_id).unwrap();
        assert_eq!(tab.status, tabs::Status::Open);
        assert!(tab.customer_tin.is_none());
        let payments: i64 = t
            .bar
            .conn
            .query_row("SELECT COUNT(*) FROM tab_payments", [], |row| row.get(0))
            .unwrap();
        let audit: i64 = t
            .bar
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!((payments, audit), (0, before_audit));
    }

    #[test]
    fn an_authorised_comp_freezes_zero_liability_and_its_reason() {
        let mut t = trading();
        issued_order(&t);
        settings::put(
            &t.bar.conn,
            keys::COMPS_ENABLED,
            "1",
            Some(t.bar.owner),
            CLOSED_AT - 1,
        )
        .unwrap();

        let closed = close_tab(
            &mut t.bar.conn,
            &CloseTab {
                tab_id: t.tab_id,
                shift_id: t.shift_id,
                cashier_id: t.bar.cashier,
                comp_reason: Some("  service recovery  "),
                customer_tin: None,
                closed_at: CLOSED_AT,
            },
        )
        .unwrap();

        assert!(closed.tab.is_comped);
        assert_eq!(closed.payment.total, Money::from_minor(5_500));
        assert_eq!(closed.payment.liability, Money::ZERO);
        assert_eq!(
            closed.payment.comp_reason.as_deref(),
            Some("service recovery")
        );
        let action: String = t
            .bar
            .conn
            .query_row(
                "SELECT action FROM audit_log ORDER BY sequence_no DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(action, "TAB_COMPED");
    }

    #[test]
    fn close_time_tin_is_captured_only_when_the_tax_prompt_is_enabled() {
        let mut refused = trading();
        issued_order(&refused);
        let error = close_tab(
            &mut refused.bar.conn,
            &CloseTab {
                tab_id: refused.tab_id,
                shift_id: refused.shift_id,
                cashier_id: refused.bar.cashier,
                comp_reason: None,
                customer_tin: Some("  CUST-TIN-7  "),
                closed_at: CLOSED_AT,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("tax and the TIN prompt"));
        assert_eq!(
            tabs::find(&refused.bar.conn, refused.tab_id)
                .unwrap()
                .status,
            tabs::Status::Open
        );

        let mut enabled = trading();
        issued_order(&enabled);
        for key in [keys::TAX_ENABLED, keys::TABS_ASK_CUSTOMER_TIN] {
            settings::put(
                &enabled.bar.conn,
                key,
                "1",
                Some(enabled.bar.owner),
                CLOSED_AT - 1,
            )
            .unwrap();
        }
        let closed = close_tab(
            &mut enabled.bar.conn,
            &CloseTab {
                tab_id: enabled.tab_id,
                shift_id: enabled.shift_id,
                cashier_id: enabled.bar.cashier,
                comp_reason: None,
                customer_tin: Some("  CUST-TIN-7  "),
                closed_at: CLOSED_AT,
            },
        )
        .unwrap();
        assert_eq!(closed.tab.customer_tin.as_deref(), Some("CUST-TIN-7"));
    }

    #[test]
    fn unfinished_drafts_block_close_without_freezing_any_money() {
        let mut t = trading();
        issued_order(&t);
        let draft = orders::create(
            &t.bar.conn,
            orders::NewDraft {
                tab_id: t.tab_id,
                shift_id: t.shift_id,
                cashier_id: t.bar.cashier,
                created_at: fixture::NOW + 2,
            },
        )
        .unwrap();
        orders::add_line(&t.bar.conn, draft.id, t.bar.beer_bottle, Milli::ONE).unwrap();

        let error = close_tab(
            &mut t.bar.conn,
            &CloseTab {
                tab_id: t.tab_id,
                shift_id: t.shift_id,
                cashier_id: t.bar.cashier,
                comp_reason: None,
                customer_tin: None,
                closed_at: CLOSED_AT,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("resolve every draft"));
        assert_eq!(
            tabs::find(&t.bar.conn, t.tab_id).unwrap().status,
            tabs::Status::Open
        );
        let payments: i64 = t
            .bar
            .conn
            .query_row("SELECT COUNT(*) FROM tab_payments", [], |row| row.get(0))
            .unwrap();
        assert_eq!(payments, 0);
    }
}
