//! Atomic command-layer protocols for the risky parts of trading.
//!
//! Repository functions deliberately never choose transaction boundaries.
//! This module does: a numbered issue slip is durably prepared before device
//! I/O, and its eventual stock/audit effects commit together.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::repo::{self, catalogue, orders, receipts, staff, stock, tabs};
use crate::{ledger, printing, Milli};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedIssue {
    pub order: orders::Order,
    pub receipts: Vec<receipts::Receipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedIssue {
    pub order: orders::Order,
    pub receipts: Vec<receipts::Receipt>,
}

#[derive(Debug, thiserror::Error)]
pub enum TradingError {
    #[error("{0}")]
    Repo(#[from] repo::RepoError),

    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("audit: {0}")]
    Audit(#[from] ledger::LedgerError),

    /// Paper may already authorise a pour, while the all-or-nothing database
    /// completion did not commit. This is deliberately not an ordinary retry
    /// error: the durable PRINTING row must go through recovery first.
    #[error("issue print for order {order_id} remains pending: {reason}")]
    IssuePrintPending { order_id: i64, reason: String },
}

pub type Result<T> = std::result::Result<T, TradingError>;

pub const HANDWRITTEN_REASON: &str = "Printer failed; handwritten chit authorised";

/// Prepare every destination slip and commit the durable `PRINTING` state.
///
/// Slip text is rendered *here*, after `receipts::create_issue` has allocated
/// the BR number, so a slip cannot exist without the number the correction
/// path makes the cashier type back off the paper. Callers pass no text.
///
/// The exact document text is frozen before this returns. Printer I/O must
/// start only afterwards. Availability includes every other order already in
/// `PRINTING`, which acts as the durable reservation across that I/O gap.
pub fn prepare_issue(
    conn: &mut Connection,
    order_id: i64,
    cashier_id: i64,
    at: i64,
) -> Result<PreparedIssue> {
    let order = orders::find(conn, order_id)?;
    if order.cashier_id != cashier_id {
        return Err(repo::RepoError::Refused(
            "only the cashier who rang the draft may issue it".into(),
        )
        .into());
    }
    if order.status != orders::Status::Draft {
        return Err(
            repo::RepoError::Refused("only a draft can be prepared for printing".into()).into(),
        );
    }
    // IMMEDIATE makes the D9 availability check and durable PRINTING
    // reservation one serialized write. Two connections cannot both observe
    // the last bottle and then burn two numbers before either reservation is
    // visible.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = orders::find(&tx, order_id)?;
    if current.cashier_id != cashier_id || current.status != orders::Status::Draft {
        return Err(repo::RepoError::Refused(
            "the draft changed before its issue reservation could be committed".into(),
        )
        .into());
    }
    let wanted: Vec<_> = orders::expanded_products(&tx, order_id)?
        .into_iter()
        .collect();
    ensure_available_with_printing(&tx, &wanted)?;

    let slips = destination_slips(&tx, order_id)?;
    let tab = tabs::find(&tx, current.tab_id)?;
    let waiter = staff::find(&tx, current.waiter_id)?;
    let mut prepared = Vec::with_capacity(slips.len());
    for (destination, lines) in &slips {
        let receipt = receipts::create_issue(&tx, order_id, *destination, at)?;
        let text = printing::render_issue(
            &receipt.receipt_number,
            *destination,
            &tab.code,
            &tab.display_label,
            &waiter.name,
            lines,
        )
        .map_err(|error| repo::RepoError::Refused(error.to_string()))?;
        receipts::freeze_rendered_text(&tx, receipt.id, &text)?;
        prepared.push(receipts::find(&tx, receipt.id)?);
    }
    orders::mark_printing(&tx, order_id)?;
    tx.commit()?;

    Ok(PreparedIssue {
        order: orders::find(conn, order_id)?,
        receipts: prepared,
    })
}

/// Transaction 2 after paper is known to have emerged.
///
/// Receipt outcomes, the order transition, one aggregated SALE row per
/// tracked product, and the audit link commit together. A retry after that
/// commit is a read-only success; a failure before commit deliberately leaves
/// the durable PRINTING state for the recovery gate.
pub fn confirm_issued(
    conn: &mut Connection,
    order_id: i64,
    cashier_id: i64,
    at: i64,
) -> Result<CompletedIssue> {
    let order = orders::find(conn, order_id)?;
    if order.cashier_id != cashier_id {
        return Err(repo::RepoError::Refused(
            "only the cashier who rang the order may confirm its issue".into(),
        )
        .into());
    }
    if order.status == orders::Status::Issued {
        return completed_issue_with(conn, order_id, receipts::Status::Printed, "ORDER_ISSUED");
    }
    if order.status != orders::Status::Printing {
        return Err(repo::RepoError::Refused(
            "only a printing order can be confirmed as issued".into(),
        )
        .into());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| issue_print_pending(order_id, error))?;
    let transaction_result = (|| -> Result<()> {
        let pending = receipts::for_order(&tx, order_id)?;
        if pending.is_empty()
            || pending.iter().any(|receipt| {
                receipt.kind != receipts::Kind::Issue || receipt.status != receipts::Status::Pending
            })
        {
            return Err(repo::RepoError::Refused(
                "every destination receipt must still be pending before confirming issue".into(),
            )
            .into());
        }
        for receipt in &pending {
            receipts::record_first_issue_attempt(
                &tx,
                receipt.id,
                receipts::FinalOutcome::Success,
                "",
                order.shift_id,
                cashier_id,
                at,
            )?;
            receipts::mark_printed(&tx, receipt.id, at)?;
        }
        finish_issue_transaction(&tx, &order, cashier_id, at, "ORDER_ISSUED")?;
        Ok(())
    })();
    if let Err(error) = transaction_result {
        drop(tx);
        return Err(issue_print_pending(order_id, error));
    }
    tx.commit()
        .map_err(|error| issue_print_pending(order_id, error))?;
    completed_issue(conn, order_id)
}

/// §6.7 grouping: one slip per destination that actually has something to hand
/// over, in a fixed order, listing the products that destination pours or
/// plates. Derived from the order so no caller can split it a second way.
fn destination_slips(
    conn: &Connection,
    order_id: i64,
) -> Result<Vec<(receipts::Destination, Vec<(String, i64)>)>> {
    let mut bar = Vec::new();
    let mut kitchen = Vec::new();
    for (product_id, quantity) in orders::expanded_products(conn, order_id)? {
        // A recipe measure that rounds away consumes nothing and has nothing
        // to hand over; printing "x0" would only confuse the bartender.
        if quantity.is_zero() {
            continue;
        }
        let product = catalogue::product(conn, product_id)?;
        let line = (product.name, quantity.thousandths());
        match product.destination.as_str() {
            "BAR" => bar.push(line),
            "KITCHEN" => kitchen.push(line),
            unknown => {
                return Err(repo::RepoError::Refused(format!(
                    "product {} has an unknown destination {unknown}",
                    product.code
                ))
                .into())
            }
        }
    }
    let slips: Vec<_> = [
        (receipts::Destination::Bar, bar),
        (receipts::Destination::Kitchen, kitchen),
    ]
    .into_iter()
    .filter(|(_, lines)| !lines.is_empty())
    .collect();
    if slips.is_empty() {
        return Err(
            repo::RepoError::Refused("an issue needs at least one order line".into()).into(),
        );
    }
    Ok(slips)
}

/// Destination grouping shared by the correction protocol without exposing a
/// second implementation of what each bar or kitchen slip contains.
pub(crate) fn issue_slips(
    conn: &Connection,
    order_id: i64,
) -> Result<Vec<(receipts::Destination, Vec<(String, i64)>)>> {
    destination_slips(conn, order_id)
}

fn issue_print_pending(order_id: i64, error: impl std::fmt::Display) -> TradingError {
    TradingError::IssuePrintPending {
        order_id,
        reason: error.to_string(),
    }
}

/// Resolve a dead printer by recording the explicit handwritten substitute.
///
/// This is not an answer to an ambiguous device result: the cashier has
/// confirmed that no generated slip emerged and has actually written the
/// replacement chit. The failed attempts remain append-only evidence while
/// stock posts exactly as it does for a printed issue.
pub fn authorize_handwritten(
    conn: &mut Connection,
    order_id: i64,
    cashier_id: i64,
    at: i64,
) -> Result<CompletedIssue> {
    require_active_cashier(conn, cashier_id)?;
    let order = orders::find(conn, order_id)?;
    if order.status == orders::Status::Issued {
        return completed_issue_with(
            conn,
            order_id,
            receipts::Status::Failed,
            "ORDER_ISSUED_HANDWRITTEN",
        );
    }
    if order.status != orders::Status::Printing {
        return Err(repo::RepoError::Refused(
            "only a printing order can be authorised with a handwritten chit".into(),
        )
        .into());
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let issue_receipts = receipts::for_order(&tx, order_id)?;
    let live: Vec<_> = issue_receipts
        .iter()
        .filter(|receipt| receipt.status != receipts::Status::Void)
        .collect();
    if live.is_empty()
        || live
            .iter()
            .any(|receipt| receipt.status != receipts::Status::Pending)
    {
        return Err(repo::RepoError::Refused(
            "handwritten issue requires every live destination receipt to be pending".into(),
        )
        .into());
    }
    for receipt in live {
        receipts::record_first_issue_attempt(
            &tx,
            receipt.id,
            receipts::FinalOutcome::Failed,
            HANDWRITTEN_REASON,
            order.shift_id,
            cashier_id,
            at,
        )?;
        receipts::mark_failed(&tx, receipt.id)?;
    }
    finish_issue_transaction(&tx, &order, cashier_id, at, "ORDER_ISSUED_HANDWRITTEN")?;
    tx.commit()?;
    completed_issue(conn, order_id)
}

/// Resolve a definite non-print without ever reusing its BR identities.
///
/// Every live destination receipt becomes VOID, the order returns to DRAFT,
/// and PRINT_ABANDONED extends the audit chain in one transaction. The draft
/// can then be retried under new BR numbers or explicitly abandoned.
pub fn confirm_non_print(
    conn: &mut Connection,
    order_id: i64,
    cashier_id: i64,
    at: i64,
) -> Result<orders::Order> {
    require_active_cashier(conn, cashier_id)?;
    let order = orders::find(conn, order_id)?;
    if order.status == orders::Status::Draft && is_completed_non_print(conn, order_id)? {
        return Ok(order);
    }
    if order.status != orders::Status::Printing {
        return Err(repo::RepoError::Refused(
            "only a printing order can be resolved as a confirmed non-print".into(),
        )
        .into());
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let issue_receipts = receipts::for_order(&tx, order_id)?;
    let live: Vec<_> = issue_receipts
        .iter()
        .filter(|receipt| receipt.status != receipts::Status::Void)
        .collect();
    if live.is_empty()
        || live
            .iter()
            .any(|receipt| receipt.status != receipts::Status::Pending)
    {
        return Err(repo::RepoError::Refused(
            "confirmed non-print requires every live issue receipt to be unattempted and pending"
                .into(),
        )
        .into());
    }
    for receipt in live {
        receipts::mark_void(&tx, receipt.id)?;
    }
    orders::return_to_draft(&tx, order_id)?;
    ledger::append(
        &tx,
        &ledger::Event::new("PRINT_ABANDONED", "order", at)
            .about(order_id)
            .changed("PRINTING", "DRAFT")
            .by(cashier_id)
            .during(Some(order.shift_id)),
    )?;
    tx.commit()?;
    Ok(orders::find(conn, order_id)?)
}

fn is_completed_non_print(conn: &Connection, order_id: i64) -> Result<bool> {
    let receipts = receipts::for_order(conn, order_id)?;
    if receipts.is_empty()
        || receipts.iter().any(|receipt| {
            receipt.kind != receipts::Kind::Issue || receipt.status != receipts::Status::Void
        })
    {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM audit_log
              WHERE action = 'PRINT_ABANDONED'
                AND entity_type = 'order' AND entity_id = ?1)",
        [order_id],
        |row| row.get(0),
    )?)
}

fn require_active_cashier(conn: &Connection, cashier_id: i64) -> Result<()> {
    let actor: Option<(String, bool)> = conn
        .query_row(
            "SELECT role, active FROM staff WHERE id = ?1",
            [cashier_id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? == 1)),
        )
        .optional()?;
    match actor {
        Some((role, true)) if role == "CASHIER" => Ok(()),
        Some(_) => Err(repo::RepoError::Refused(
            "only an active cashier may resolve an issue print".into(),
        )
        .into()),
        None => Err(repo::RepoError::Missing { what: "cashier" }.into()),
    }
}

fn finish_issue_transaction(
    conn: &Connection,
    order: &orders::Order,
    cashier_id: i64,
    at: i64,
    audit_action: &str,
) -> Result<()> {
    let previous_stock: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stock_movements WHERE order_id = ?1)",
        [order.id],
        |row| row.get(0),
    )?;
    let previous_audit: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM audit_log
                        WHERE entity_type = 'order' AND entity_id = ?1
                          AND action IN ('ORDER_ISSUED','ORDER_ISSUED_HANDWRITTEN'))",
        [order.id],
        |row| row.get(0),
    )?;
    if previous_stock || previous_audit {
        return Err(repo::RepoError::Refused(
            "that printing order already has issue side effects and needs manual review".into(),
        )
        .into());
    }
    orders::mark_issued(conn, order.id, at)?;
    for (product_id, quantity) in orders::expanded_products(conn, order.id)? {
        let product = catalogue::product(conn, product_id)?;
        let sale_quantity = Milli::ZERO.checked_sub(quantity).map_err(|_| {
            repo::RepoError::Refused("that order is too large to post safely".into())
        })?;
        stock::post(
            conn,
            &stock::Movement::new(product_id, stock::Kind::Sale, sale_quantity, at, cashier_id)
                .for_order(order.id)
                .during(order.shift_id)
                .costing(product.avg_cost),
        )?;
    }
    ledger::append(
        conn,
        &ledger::Event::new(audit_action, "order", at)
            .about(order.id)
            .changed("PRINTING", "ISSUED")
            .by(cashier_id)
            .during(Some(order.shift_id)),
    )?;
    Ok(())
}

fn completed_issue(conn: &Connection, order_id: i64) -> Result<CompletedIssue> {
    Ok(CompletedIssue {
        order: orders::find(conn, order_id)?,
        receipts: receipts::for_order(conn, order_id)?,
    })
}

fn completed_issue_with(
    conn: &Connection,
    order_id: i64,
    receipt_status: receipts::Status,
    audit_action: &str,
) -> Result<CompletedIssue> {
    let completed = completed_issue(conn, order_id)?;
    let audit_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM audit_log
          WHERE entity_type = 'order' AND entity_id = ?1 AND action = ?2",
        rusqlite::params![order_id, audit_action],
        |row| row.get(0),
    )?;
    if completed.receipts.is_empty()
        || completed
            .receipts
            .iter()
            .filter(|receipt| receipt.status != receipts::Status::Void)
            .any(|receipt| receipt.status != receipt_status)
        || audit_count != 1
    {
        return Err(repo::RepoError::Refused(
            "the issued order does not match this recovery outcome and needs manual review".into(),
        )
        .into());
    }
    Ok(completed)
}

pub(crate) fn ensure_available_with_printing(
    conn: &Connection,
    wanted: &[(i64, Milli)],
) -> Result<()> {
    let mut totals: BTreeMap<_, _> = wanted.iter().copied().collect();
    let printing_ids = {
        let mut statement = conn.prepare("SELECT id FROM orders WHERE status = 'PRINTING'")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<i64>>>()?
    };
    for printing_id in printing_ids {
        for (product_id, quantity) in orders::expanded_products(conn, printing_id)? {
            let current = totals.entry(product_id).or_insert(Milli::ZERO);
            *current = current.checked_add(quantity).map_err(|_| {
                repo::RepoError::Refused("reserved stock is too large to total safely".into())
            })?;
        }
    }
    let wanted: Vec<_> = totals
        .into_iter()
        .filter(|(_, quantity)| !quantity.is_zero())
        .collect();
    let shortages = stock::shortages(conn, &wanted)?;
    if !shortages.is_empty() {
        return Err(repo::RepoError::Refused(stock::shortage_message(&shortages)).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{fixture, orders, receipts, shifts, tabs};
    use crate::{Milli, Money};

    const LATER: i64 = fixture::NOW + 1_000;

    struct Trading {
        bar: fixture::Bar,
        shift_id: i64,
        tab_id: i64,
    }

    fn trading(stock_milli: i64) -> Trading {
        let bar = fixture::bar();
        if stock_milli != 0 {
            fixture::stock_up(&bar.conn, bar.beer, stock_milli, bar.owner);
        }
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
                reference: tabs::Reference::table("7"),
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

    fn beer_draft(t: &Trading) -> orders::Order {
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
        order
    }

    #[test]
    fn a_durable_printing_order_reserves_stock_before_a_second_number_is_burned() {
        let mut t = trading(1_000);
        let first = beer_draft(&t);
        let second = beer_draft(&t);

        let prepared = prepare_issue(&mut t.bar.conn, first.id, t.bar.cashier, LATER).unwrap();
        assert_eq!(prepared.receipts.len(), 1);

        let refused =
            prepare_issue(&mut t.bar.conn, second.id, t.bar.cashier, LATER + 1).unwrap_err();
        assert!(
            refused.to_string().contains("Not enough stock"),
            "got: {refused}"
        );

        let second = orders::find(&t.bar.conn, second.id).unwrap();
        assert_eq!(second.status, orders::Status::Draft);
        let next: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT next_value FROM sequences WHERE name = 'ISSUE_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, 2, "the refused order did not consume BR-000002");
    }

    #[test]
    fn confirming_printed_issues_every_destination_and_posts_stock_and_audit_once() {
        let mut t = trading(1_000);
        let order = beer_draft(&t);
        let prepared = prepare_issue(&mut t.bar.conn, order.id, t.bar.cashier, LATER).unwrap();

        let completed =
            confirm_issued(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 1).unwrap();
        assert_eq!(completed.order.status, orders::Status::Issued);
        assert_eq!(completed.receipts[0].status, receipts::Status::Printed);
        assert_eq!(
            receipts::attempts(&t.bar.conn, prepared.receipts[0].id)
                .unwrap()
                .iter()
                .map(|attempt| attempt.outcome)
                .collect::<Vec<_>>(),
            vec![receipts::Outcome::Success]
        );
        assert_eq!(
            stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(),
            Milli::ZERO
        );

        // A recovery-screen retry is idempotent: it returns the committed
        // result without consuming stock or extending the audit chain again.
        confirm_issued(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 2).unwrap();
        let (sales, audits): (i64, i64) = t
            .bar
            .conn
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM stock_movements
                       WHERE order_id = ?1 AND movement_type = 'SALE'),
                     (SELECT COUNT(*) FROM audit_log
                       WHERE entity_type = 'order' AND entity_id = ?1
                         AND action = 'ORDER_ISSUED')",
                [order.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((sales, audits), (1, 1));
    }

    #[test]
    fn confirmed_non_print_voids_every_number_and_returns_the_order_to_draft_atomically() {
        let mut t = trading(1_000);
        let order = beer_draft(&t);
        let prepared = prepare_issue(&mut t.bar.conn, order.id, t.bar.cashier, LATER).unwrap();

        let recovered =
            confirm_non_print(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 1).unwrap();
        assert_eq!(recovered.status, orders::Status::Draft);
        assert_eq!(
            receipts::find(&t.bar.conn, prepared.receipts[0].id)
                .unwrap()
                .status,
            receipts::Status::Void
        );
        assert_eq!(stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(), Milli::ONE);
        let audit_count: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log
                  WHERE action = 'PRINT_ABANDONED'
                    AND entity_type = 'order' AND entity_id = ?1",
                [order.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_count, 1);

        // Lost UI acknowledgement after COMMIT is safe to retry.
        confirm_non_print(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 2).unwrap();
        let audit_count: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'PRINT_ABANDONED'
                   AND entity_id = ?1",
                [order.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_count, 1);
    }

    #[test]
    fn handwritten_authorisation_is_durable_and_issues_stock_normally() {
        let mut t = trading(1_000);
        let order = beer_draft(&t);
        let prepared = prepare_issue(&mut t.bar.conn, order.id, t.bar.cashier, LATER).unwrap();

        let completed =
            authorize_handwritten(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 1).unwrap();
        assert_eq!(completed.order.status, orders::Status::Issued);
        assert_eq!(completed.receipts[0].status, receipts::Status::Failed);
        let attempts = receipts::attempts(&t.bar.conn, prepared.receipts[0].id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, receipts::Outcome::Failed);
        assert_eq!(attempts[0].reason, HANDWRITTEN_REASON);
        assert_eq!(
            stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(),
            Milli::ZERO
        );

        authorize_handwritten(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 2).unwrap();
        let (sales, audits): (i64, i64) = t
            .bar
            .conn
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM stock_movements
                       WHERE order_id = ?1 AND movement_type = 'SALE'),
                     (SELECT COUNT(*) FROM audit_log
                       WHERE action = 'ORDER_ISSUED_HANDWRITTEN'
                         AND entity_type = 'order' AND entity_id = ?1)",
                [order.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((sales, audits), (1, 1));
    }

    #[test]
    fn a_transaction_two_failure_is_reported_as_ambiguous_and_rolls_every_side_effect_back() {
        let mut t = trading(1_000);
        let order = beer_draft(&t);
        let prepared = prepare_issue(&mut t.bar.conn, order.id, t.bar.cashier, LATER).unwrap();
        t.bar
            .conn
            .execute_batch(
                "CREATE TRIGGER test_fail_issue_audit BEFORE INSERT ON audit_log
                 WHEN NEW.action = 'ORDER_ISSUED'
                 BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
            )
            .unwrap();

        let error =
            confirm_issued(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 1).unwrap_err();
        assert!(matches!(
            error,
            TradingError::IssuePrintPending { order_id, .. } if order_id == order.id
        ));
        assert_eq!(
            orders::find(&t.bar.conn, order.id).unwrap().status,
            orders::Status::Printing
        );
        assert_eq!(
            receipts::find(&t.bar.conn, prepared.receipts[0].id)
                .unwrap()
                .status,
            receipts::Status::Pending
        );
        assert!(receipts::attempts(&t.bar.conn, prepared.receipts[0].id)
            .unwrap()
            .is_empty());
        assert_eq!(stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(), Milli::ONE);
        let (sales, audits): (i64, i64) = t
            .bar
            .conn
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM stock_movements WHERE order_id = ?1),
                     (SELECT COUNT(*) FROM audit_log
                       WHERE entity_type = 'order' AND entity_id = ?1)",
                [order.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((sales, audits), (0, 0));
    }

    #[test]
    fn transaction_two_resolves_all_destinations_and_posts_one_aggregated_sale_per_product() {
        let mut t = trading(0);
        fixture::stock_up(&t.bar.conn, t.bar.gin, 2_000, t.bar.owner);
        fixture::stock_up(&t.bar.conn, t.bar.tonic, 500, t.bar.owner);
        t.bar
            .conn
            .execute(
                "UPDATE products SET destination = 'KITCHEN' WHERE id = ?1",
                [t.bar.tonic],
            )
            .unwrap();
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
        orders::add_line(&t.bar.conn, order.id, t.bar.gin_tonic, Milli::ONE).unwrap();
        prepare_issue(&mut t.bar.conn, order.id, t.bar.cashier, LATER).unwrap();

        let completed =
            confirm_issued(&mut t.bar.conn, order.id, t.bar.cashier, LATER + 1).unwrap();
        assert_eq!(completed.receipts.len(), 2);
        assert!(completed
            .receipts
            .iter()
            .all(|receipt| receipt.status == receipts::Status::Printed));

        // The cocktail is split by product destination, and every slip carries
        // the number the correction path makes the cashier type back.
        for receipt in &completed.receipts {
            let text = receipt.rendered_text.as_deref().unwrap();
            assert!(
                text.contains(&receipt.receipt_number),
                "slip has no BR number: {text}"
            );
            assert!(text.contains("Tab: TAB-000001 — Table 7"));
            assert!(text.contains("Waiter: Sara"));
            match receipt.destination.unwrap() {
                receipts::Destination::Bar => {
                    assert!(text.contains("Gin x2") && !text.contains("Tonic"))
                }
                receipts::Destination::Kitchen => {
                    assert!(text.contains("Tonic x0.5") && !text.contains("Gin"))
                }
            }
        }
        let movements = t
            .bar
            .conn
            .prepare(
                "SELECT product_id, quantity_milli FROM stock_movements
                  WHERE order_id = ?1 AND movement_type = 'SALE' ORDER BY product_id",
            )
            .unwrap()
            .query_map([order.id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(movements, vec![(t.bar.gin, -2_000), (t.bar.tonic, -500)]);
    }

    #[test]
    fn two_connections_cannot_both_reserve_the_last_bottle() {
        let path = std::env::temp_dir().join(format!(
            "servepoint-issue-interleaving-{}-{}.sqlite3",
            std::process::id(),
            fixture::NOW
        ));
        let _ = std::fs::remove_file(&path);
        let mut first_conn = crate::db::open(&path).unwrap();
        let owner = fixture::staff(&first_conn, "OWN-C", "Owner", "OWNER");
        let cashier = fixture::staff(&first_conn, "CSH-C", "Cashier", "CASHIER");
        let waiter = fixture::staff(&first_conn, "WTR-C", "Waiter", "WAITER");
        let product = fixture::product(&first_conn, "P-LAST", "Last beer", "BOTTLE", 1_000);
        let item = fixture::sale_item(&first_conn, "S-LAST", "Last beer", "Beer", 5_000);
        fixture::recipe(&first_conn, item, &[(product, 1_000)]);
        fixture::stock_up(&first_conn, product, 1_000, owner);
        let shift = shifts::open(
            &first_conn,
            &shifts::NewShift {
                business_date: "2025-08-01",
                opened_at: fixture::NOW,
                opened_by: cashier,
                opening_float: Money::ZERO,
                expected_end_at: fixture::NOW + 43_200_000,
            },
        )
        .unwrap();
        let tab = tabs::open(
            &first_conn,
            &tabs::NewTab {
                opened_shift_id: shift.id,
                waiter_id: waiter,
                reference: tabs::Reference::table("88"),
                opened_at: fixture::NOW,
                opened_by: cashier,
            },
        )
        .unwrap();
        let make_draft = |conn: &Connection| {
            let order = orders::create(
                conn,
                orders::NewDraft {
                    tab_id: tab.id,
                    shift_id: shift.id,
                    cashier_id: cashier,
                    created_at: fixture::NOW,
                },
            )
            .unwrap();
            orders::add_line(conn, order.id, item, Milli::ONE).unwrap();
            order
        };
        let first = make_draft(&first_conn);
        let second = make_draft(&first_conn);
        let mut second_conn = crate::db::open(&path).unwrap();

        prepare_issue(&mut first_conn, first.id, cashier, LATER).unwrap();
        let refused = prepare_issue(&mut second_conn, second.id, cashier, LATER + 1).unwrap_err();
        assert!(refused.to_string().contains("Not enough stock"));
        assert_eq!(
            orders::find(&second_conn, second.id).unwrap().status,
            orders::Status::Draft
        );
        let next: i64 = second_conn
            .query_row(
                "SELECT next_value FROM sequences WHERE name = 'ISSUE_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, 2);

        drop(second_conn);
        drop(first_conn);
        std::fs::remove_file(&path).unwrap();
    }
}
