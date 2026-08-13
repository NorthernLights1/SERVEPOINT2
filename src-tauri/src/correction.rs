//! Order corrections and voids: the transactions around frozen replacement paper.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, TransactionBehavior};

use crate::ledger::{self, Event};
use crate::printing;
use crate::repo::{self, catalogue, orders, receipts, staff, stock, tabs};
use crate::{trading, Milli};

#[derive(Debug, thiserror::Error)]
pub enum CorrectionError {
    #[error("{0}")]
    Repo(#[from] repo::RepoError),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("audit: {0}")]
    Audit(#[from] ledger::LedgerError),
    #[error("{0}")]
    Trading(#[from] trading::TradingError),
    #[error("correction print for order {order_id} remains pending: {reason}")]
    PrintPending { order_id: i64, reason: String },
}

pub type Result<T> = std::result::Result<T, CorrectionError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementLine {
    pub original_line_id: Option<i64>,
    pub sale_item_id: i64,
    pub quantity: Milli,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnedProduct {
    pub product_id: i64,
    pub quantity: Milli,
    pub note: String,
}

#[derive(Clone, Copy, Debug)]
pub struct NewCorrection<'a> {
    pub original_order_id: i64,
    pub issue_receipt_number: &'a str,
    pub reason: &'a str,
    pub cashier_id: i64,
    pub at: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct NewVoid<'a> {
    pub original_order_id: i64,
    pub issue_receipt_number: &'a str,
    pub reason: &'a str,
    pub cashier_id: i64,
    pub at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintOutcome {
    Printed,
    Handwritten,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedCorrection {
    pub pending: orders::PendingCorrection,
    pub replacement: orders::Order,
    pub receipts: Vec<receipts::Receipt>,
}

pub fn deltas(
    conn: &Connection,
    original_order_id: i64,
    replacement_order_id: Option<i64>,
    returned: &[ReturnedProduct],
) -> Result<Vec<orders::CorrectionDelta>> {
    let before = orders::expanded_products(conn, original_order_id)?;
    let after = match replacement_order_id {
        Some(id) => orders::expanded_products(conn, id)?,
        None => BTreeMap::new(),
    };
    let mut dispositions = BTreeMap::new();
    for item in returned {
        if dispositions.insert(item.product_id, item).is_some() {
            return Err(repo::RepoError::Refused(
                "returned stock must name each product once".into(),
            )
            .into());
        }
    }
    let products: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    let mut result = Vec::new();
    for product_id in products {
        let previous = before.get(&product_id).copied().unwrap_or(Milli::ZERO);
        let next = after.get(&product_id).copied().unwrap_or(Milli::ZERO);
        let disposition = dispositions.remove(&product_id);
        result.push(orders::CorrectionDelta::new(
            product_id,
            previous,
            next,
            disposition.map(|item| item.quantity).unwrap_or(Milli::ZERO),
            disposition
                .map(|item| item.note.as_str())
                .unwrap_or_default(),
        )?);
    }
    if !dispositions.is_empty() {
        return Err(repo::RepoError::Refused(
            "returned stock must belong to a product on that correction".into(),
        )
        .into());
    }
    Ok(result)
}

/// Freeze replacement facts and its numbered slips before any printer I/O.
pub fn prepare(
    conn: &mut Connection,
    input: NewCorrection<'_>,
    lines: &[ReplacementLine],
    returned: &[ReturnedProduct],
) -> Result<PreparedCorrection> {
    if lines.is_empty() {
        return Err(repo::RepoError::Refused(
            "a correction needs at least one line; use void to remove the whole order".into(),
        )
        .into());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let original = orders::find(&tx, input.original_order_id)?;
    let replacement = orders::create_replacement(&tx, original.id, input.cashier_id, input.at)?;
    for line in lines {
        if let Some(original_line_id) = line.original_line_id {
            orders::add_line_from_original(&tx, replacement.id, original_line_id, line.quantity)?;
        } else {
            orders::add_line(&tx, replacement.id, line.sale_item_id, line.quantity)?;
        }
    }
    let frozen_deltas = deltas(&tx, original.id, Some(replacement.id), returned)?;
    let additional: Vec<_> = frozen_deltas
        .iter()
        .filter(|line| line.delta.thousandths() > 0)
        .map(|line| (line.product_id, line.delta))
        .collect();
    trading::ensure_available_with_printing(&tx, &additional)?;
    let pending = orders::freeze_correction(
        &tx,
        orders::NewCorrection {
            original_order_id: original.id,
            replacement_order_id: replacement.id,
            issue_receipt_number: input.issue_receipt_number,
            reason: input.reason,
            shift_id: original.shift_id,
            created_by: input.cashier_id,
            created_at: input.at,
        },
        &frozen_deltas,
    )?;
    let tab = tabs::find(&tx, original.tab_id)?;
    let waiter = staff::find(&tx, original.waiter_id)?;
    let mut prepared = Vec::new();
    for (destination, slip_lines) in trading::issue_slips(&tx, replacement.id)? {
        let receipt = receipts::create_issue(&tx, replacement.id, destination, input.at)?;
        let text = printing::render_issue(
            &receipt.receipt_number,
            destination,
            &tab.code,
            &tab.display_label,
            &waiter.name,
            &slip_lines,
        )
        .map_err(|error| repo::RepoError::Refused(error.to_string()))?;
        let text = printing::mark_replacement(&text, input.issue_receipt_number)
            .map_err(|error| repo::RepoError::Refused(error.to_string()))?;
        receipts::freeze_rendered_text(&tx, receipt.id, &text)?;
        prepared.push(receipts::find(&tx, receipt.id)?);
    }
    orders::mark_printing(&tx, replacement.id)?;
    tx.commit()?;
    Ok(PreparedCorrection {
        pending,
        replacement: orders::find(conn, replacement.id)?,
        receipts: prepared,
    })
}

/// Complete frozen replacement paper, posting only its delta effects.
pub fn complete(
    conn: &mut Connection,
    pending_id: i64,
    cashier_id: i64,
    at: i64,
    outcome: PrintOutcome,
) -> Result<orders::Correction> {
    // Deliberately not restricted to the cashier who began it. A stranded
    // correction holds the shift open (`shifts::recovery_complete`), and the
    // person who started it may well have gone home before the printer was
    // fixed. Whoever answers for the paper is recorded in the audit events.
    let frozen = orders::pending(conn, pending_id)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CorrectionError::PrintPending {
            order_id: frozen.replacement_order_id,
            reason: error.to_string(),
        })?;
    let result = (|| -> Result<orders::Correction> {
        let replacement = orders::find(&tx, frozen.replacement_order_id)?;
        let live = receipts::for_order(&tx, replacement.id)?;
        if live.is_empty()
            || live
                .iter()
                .any(|item| item.status != receipts::Status::Pending)
        {
            return Err(repo::RepoError::Refused(
                "every replacement slip must still be pending".into(),
            )
            .into());
        }
        for receipt in &live {
            let (attempt, reason) = match outcome {
                PrintOutcome::Printed => (receipts::FinalOutcome::Success, ""),
                PrintOutcome::Handwritten => {
                    (receipts::FinalOutcome::Failed, trading::HANDWRITTEN_REASON)
                }
            };
            receipts::record_first_issue_attempt(
                &tx,
                receipt.id,
                attempt,
                reason,
                frozen.shift_id,
                cashier_id,
                at,
            )?;
            match outcome {
                PrintOutcome::Printed => receipts::mark_printed(&tx, receipt.id, at)?,
                PrintOutcome::Handwritten => receipts::mark_failed(&tx, receipt.id)?,
            }
        }
        orders::mark_issued(&tx, replacement.id, at)?;
        let applied = orders::apply_pending_correction(&tx, pending_id)?;
        for line in &applied.lines {
            let product = catalogue::product(&tx, line.product_id)?;
            if line.delta.thousandths() > 0 {
                let quantity = Milli::ZERO
                    .checked_sub(line.delta)
                    .map_err(|_| repo::RepoError::Refused("that correction is too large".into()))?;
                stock::post(
                    &tx,
                    &stock::Movement::new(
                        line.product_id,
                        stock::Kind::Sale,
                        quantity,
                        at,
                        cashier_id,
                    )
                    .for_order(replacement.id)
                    .during(frozen.shift_id)
                    .costing(product.avg_cost),
                )?;
            }
            if !line.returned.is_zero() {
                stock::post(
                    &tx,
                    &stock::Movement::new(
                        line.product_id,
                        stock::Kind::Return,
                        line.returned,
                        at,
                        cashier_id,
                    )
                    .for_order(frozen.original_order_id)
                    .during(frozen.shift_id)
                    .because(&line.note)
                    .costing(product.avg_cost),
                )?;
            }
        }
        orders::mark_replaced(&tx, frozen.original_order_id)?;
        let issue_action = match outcome {
            PrintOutcome::Printed => "ORDER_ISSUED",
            PrintOutcome::Handwritten => "ORDER_ISSUED_HANDWRITTEN",
        };
        ledger::append(
            &tx,
            &Event::new(issue_action, "order", at)
                .about(replacement.id)
                .changed("PRINTING", "ISSUED")
                .by(cashier_id)
                .during(Some(frozen.shift_id)),
        )?;
        ledger::append(
            &tx,
            &Event::new("ORDER_CORRECTED", "order", at)
                .about(frozen.original_order_id)
                .changed("ISSUED", "REPLACED")
                .by(cashier_id)
                .during(Some(frozen.shift_id)),
        )?;
        orders::clear_pending_correction(&tx, pending_id)?;
        Ok(applied)
    })();
    match result {
        Ok(applied) => {
            tx.commit().map_err(|error| CorrectionError::PrintPending {
                order_id: frozen.replacement_order_id,
                reason: error.to_string(),
            })?;
            Ok(applied)
        }
        Err(error) => Err(CorrectionError::PrintPending {
            order_id: frozen.replacement_order_id,
            reason: error.to_string(),
        }),
    }
}

pub fn abandon(conn: &mut Connection, pending_id: i64, cashier_id: i64, at: i64) -> Result<()> {
    // Open to any cashier, for the same reason as `complete`.
    let frozen = orders::pending(conn, pending_id)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for receipt in receipts::for_order(&tx, frozen.replacement_order_id)? {
        if receipt.status != receipts::Status::Void {
            receipts::mark_void(&tx, receipt.id)?;
        }
    }
    orders::return_to_draft(&tx, frozen.replacement_order_id)?;
    orders::detach_replacement(&tx, frozen.replacement_order_id)?;
    orders::abandon(&tx, frozen.replacement_order_id)?;
    ledger::append(
        &tx,
        &Event::new("CORRECTION_ABANDONED", "order", at)
            .about(frozen.original_order_id)
            .by(cashier_id)
            .during(Some(frozen.shift_id)),
    )?;
    orders::clear_pending_correction(&tx, pending_id)?;
    tx.commit()?;
    Ok(())
}

pub fn void(
    conn: &mut Connection,
    input: NewVoid<'_>,
    returned: &[ReturnedProduct],
) -> Result<orders::Correction> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let original = orders::find(&tx, input.original_order_id)?;
    let frozen_deltas = deltas(&tx, original.id, None, returned)?;
    let applied = orders::record_void(
        &tx,
        orders::NewVoid {
            original_order_id: original.id,
            issue_receipt_number: input.issue_receipt_number,
            reason: input.reason,
            shift_id: original.shift_id,
            created_by: input.cashier_id,
            created_at: input.at,
        },
        &frozen_deltas,
    )?;
    for line in &applied.lines {
        if !line.returned.is_zero() {
            let product = catalogue::product(&tx, line.product_id)?;
            stock::post(
                &tx,
                &stock::Movement::new(
                    line.product_id,
                    stock::Kind::Return,
                    line.returned,
                    input.at,
                    input.cashier_id,
                )
                .for_order(original.id)
                .during(original.shift_id)
                .because(&line.note)
                .costing(product.avg_cost),
            )?;
        }
    }
    orders::mark_voided(&tx, original.id, input.reason, input.at, input.cashier_id)?;
    ledger::append(
        &tx,
        &Event::new("ORDER_VOIDED", "order", input.at)
            .about(original.id)
            .changed("ISSUED", "VOIDED")
            .by(input.cashier_id)
            .during(Some(original.shift_id)),
    )?;
    tx.commit()?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{fixture, orders, receipts, shifts, stock, tabs};
    use crate::{trading, Milli, Money};

    const LATER: i64 = fixture::NOW + 1_000;

    struct Trading {
        bar: fixture::Bar,
        tab_id: i64,
    }

    fn trading(quantity: Milli) -> (Trading, orders::Order, receipts::Receipt) {
        let mut bar = fixture::bar();
        fixture::stock_up(&bar.conn, bar.beer, 10_000, bar.owner);
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
        let order = orders::create(
            &bar.conn,
            orders::NewDraft {
                tab_id: tab.id,
                shift_id: shift.id,
                cashier_id: bar.cashier,
                created_at: fixture::NOW,
            },
        )
        .unwrap();
        orders::add_line(&bar.conn, order.id, bar.beer_bottle, quantity).unwrap();
        let prepared =
            trading::prepare_issue(&mut bar.conn, order.id, bar.cashier, fixture::NOW).unwrap();
        trading::confirm_issued(&mut bar.conn, order.id, bar.cashier, fixture::NOW).unwrap();
        let receipt = receipts::find(&bar.conn, prepared.receipts[0].id).unwrap();
        let order = orders::find(&bar.conn, order.id).unwrap();
        (
            Trading {
                bar,
                tab_id: tab.id,
            },
            order,
            receipt,
        )
    }

    #[test]
    fn a_decrease_posts_only_the_physical_return_and_never_the_full_replacement_sale() {
        let (mut t, original, receipt) = trading(Milli::from_units(5));
        let original_line = orders::lines(&t.bar.conn, original.id).unwrap().remove(0);

        let prepared = prepare(
            &mut t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "customer ordered three",
                cashier_id: t.bar.cashier,
                at: LATER,
            },
            &[ReplacementLine {
                original_line_id: Some(original_line.id),
                sale_item_id: original_line.sale_item_id,
                quantity: Milli::from_units(3),
            }],
            &[ReturnedProduct {
                product_id: t.bar.beer,
                quantity: Milli::ONE,
                note: "sealed bottle".into(),
            }],
        )
        .unwrap();

        assert_eq!(
            orders::find(&t.bar.conn, original.id).unwrap().status,
            orders::Status::Issued
        );
        assert_eq!(prepared.replacement.status, orders::Status::Printing);
        assert!(prepared.receipts[0]
            .rendered_text
            .as_deref()
            .unwrap()
            .contains(&format!("REPLACES {}", receipt.receipt_number)));
        assert_eq!(
            stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(),
            Milli::from_units(5)
        );

        complete(
            &mut t.bar.conn,
            prepared.pending.id,
            t.bar.cashier,
            LATER + 1,
            PrintOutcome::Printed,
        )
        .unwrap();

        assert_eq!(
            orders::find(&t.bar.conn, original.id).unwrap().status,
            orders::Status::Replaced
        );
        assert_eq!(
            orders::find(&t.bar.conn, prepared.replacement.id)
                .unwrap()
                .status,
            orders::Status::Issued
        );
        assert_eq!(
            tabs::running_total(&t.bar.conn, t.tab_id).unwrap(),
            Money::from_minor(15_000)
        );
        assert_eq!(
            stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(),
            Milli::from_units(6)
        );

        let (replacement_sales, returns, pending, corrected): (i64, i64, i64, i64) = t
            .bar
            .conn
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM stock_movements
                       WHERE order_id = ?1 AND movement_type = 'SALE'),
                     (SELECT COUNT(*) FROM stock_movements
                       WHERE order_id = ?2 AND movement_type = 'RETURN'),
                     (SELECT COUNT(*) FROM pending_order_corrections),
                     (SELECT COUNT(*) FROM audit_log WHERE action = 'ORDER_CORRECTED')",
                rusqlite::params![prepared.replacement.id, original.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (replacement_sales, returns, pending, corrected),
            (0, 1, 0, 1)
        );
    }

    #[test]
    fn a_void_is_one_atomic_history_stock_and_audit_change() {
        let (mut t, original, receipt) = trading(Milli::from_units(2));
        let applied = void(
            &mut t.bar.conn,
            NewVoid {
                original_order_id: original.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "duplicate round",
                cashier_id: t.bar.cashier,
                at: LATER,
            },
            &[ReturnedProduct {
                product_id: t.bar.beer,
                quantity: Milli::ONE,
                note: "one sealed bottle".into(),
            }],
        )
        .unwrap();

        assert_eq!(applied.kind, orders::CorrectionKind::Void);
        assert_eq!(applied.lines[0].written_off, Milli::ONE);
        assert_eq!(
            orders::find(&t.bar.conn, original.id).unwrap().status,
            orders::Status::Voided
        );
        assert_eq!(
            tabs::running_total(&t.bar.conn, t.tab_id).unwrap(),
            Money::ZERO
        );
        assert_eq!(
            stock::on_hand(&t.bar.conn, t.bar.beer).unwrap(),
            Milli::from_units(9)
        );
        let audited: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'ORDER_VOIDED'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audited, 1);
    }

    #[test]
    fn abandoning_unprinted_replacement_paper_releases_the_original_for_another_try() {
        let (mut t, original, receipt) = trading(Milli::from_units(2));
        let original_line = orders::lines(&t.bar.conn, original.id).unwrap().remove(0);
        let input = [ReplacementLine {
            original_line_id: Some(original_line.id),
            sale_item_id: original_line.sale_item_id,
            quantity: Milli::ONE,
        }];
        let prepared = prepare(
            &mut t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "one returned",
                cashier_id: t.bar.cashier,
                at: LATER,
            },
            &input,
            &[],
        )
        .unwrap();

        abandon(
            &mut t.bar.conn,
            prepared.pending.id,
            t.bar.cashier,
            LATER + 1,
        )
        .unwrap();

        let replacement = orders::find(&t.bar.conn, prepared.replacement.id).unwrap();
        assert_eq!(replacement.status, orders::Status::Abandoned);
        assert_eq!(replacement.replaces_order_id, None);
        assert_eq!(
            orders::find(&t.bar.conn, original.id).unwrap().status,
            orders::Status::Issued
        );
        assert!(orders::pending_corrections(&t.bar.conn).unwrap().is_empty());
        assert!(prepared
            .receipts
            .iter()
            .all(|item| receipts::find(&t.bar.conn, item.id).unwrap().status
                == receipts::Status::Void));

        prepare(
            &mut t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "try again",
                cashier_id: t.bar.cashier,
                at: LATER + 2,
            },
            &input,
            &[],
        )
        .unwrap();
    }
}
