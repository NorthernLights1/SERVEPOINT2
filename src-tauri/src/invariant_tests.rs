//! Phase-1 invariant tests from business logic section 11.4 and port decision D10.

use std::collections::HashSet;

use proptest::prelude::*;

use crate::audit::{BreakReason, ChainStatus};
use crate::bill::{Bill, ChargeConfig};
use crate::ledger::{self, Event};
use crate::repo::{cash, fixture, orders, receipts, shifts, stock, tabs};
use crate::resolution::{resolution_for, ReachableNonTerminalState};
use crate::{BasisPoints, Milli, Money};

fn half_up_ratio(numerator: i128, denominator: i128) -> i64 {
    i64::try_from((numerator + denominator / 2) / denominator).unwrap()
}

fn open_shift(bar: &fixture::Bar, opening_float_minor: i64) -> shifts::Shift {
    shifts::open(
        &bar.conn,
        &shifts::NewShift {
            business_date: "2025-08-01",
            opened_at: fixture::NOW,
            opened_by: bar.cashier,
            opening_float: Money::from_minor(opening_float_minor),
            expected_end_at: fixture::NOW + 12 * 60 * 60 * 1_000,
        },
    )
    .unwrap()
}

fn open_tab(bar: &fixture::Bar, shift_id: i64, index: usize) -> tabs::Tab {
    let reference = format!("PROPERTY-{index}");
    tabs::open(
        &bar.conn,
        &tabs::NewTab {
            opened_shift_id: shift_id,
            waiter_id: bar.sara,
            reference: tabs::Reference::custom(&reference),
            opened_at: fixture::NOW + i64::try_from(index).unwrap(),
            opened_by: bar.cashier,
        },
    )
    .unwrap()
}

fn draft_with_beer(
    bar: &fixture::Bar,
    shift_id: i64,
    tab_id: i64,
    quantity_milli: i64,
    index: usize,
) -> orders::Order {
    let order = orders::create(
        &bar.conn,
        orders::NewDraft {
            tab_id,
            shift_id,
            cashier_id: bar.cashier,
            created_at: fixture::NOW + 100 + i64::try_from(index).unwrap(),
        },
    )
    .unwrap();
    orders::add_line(
        &bar.conn,
        order.id,
        bar.beer_bottle,
        Milli::from_thousandths(quantity_milli),
    )
    .unwrap();
    order
}

fn prepare_issue(bar: &fixture::Bar, order_id: i64, offset: i64) -> receipts::Receipt {
    let receipt = receipts::create_issue(
        &bar.conn,
        order_id,
        receipts::Destination::Bar,
        fixture::NOW + 1_000 + offset,
    )
    .unwrap();
    orders::mark_printing(&bar.conn, order_id).unwrap();
    receipts::freeze_rendered_text(
        &bar.conn,
        receipt.id,
        &format!("generated receipt {}\n", receipt.receipt_number),
    )
    .unwrap();
    receipt
}

fn finish_issue(
    bar: &fixture::Bar,
    shift_id: i64,
    order_id: i64,
    receipt: &receipts::Receipt,
    outcome: receipts::FinalOutcome,
    offset: i64,
) {
    let reason = if outcome == receipts::FinalOutcome::Failed {
        "handwritten BR authorised"
    } else {
        ""
    };
    receipts::record_first_issue_attempt(
        &bar.conn,
        receipt.id,
        outcome,
        reason,
        shift_id,
        bar.cashier,
        fixture::NOW + 2_000 + offset,
    )
    .unwrap();
    match outcome {
        receipts::FinalOutcome::Success => {
            receipts::mark_printed(&bar.conn, receipt.id, fixture::NOW + 3_000 + offset).unwrap()
        }
        receipts::FinalOutcome::Failed => receipts::mark_failed(&bar.conn, receipt.id).unwrap(),
    }
    orders::mark_issued(&bar.conn, order_id, fixture::NOW + 4_000 + offset).unwrap();
}

fn close_tab_with_payment(
    bar: &fixture::Bar,
    shift_id: i64,
    tab_id: i64,
    liability_minor: i64,
    offset: i64,
) -> i64 {
    let waiter_id: i64 = bar
        .conn
        .query_row(
            "SELECT waiter_id FROM tabs WHERE id = ?1",
            [tab_id],
            |row| row.get(0),
        )
        .unwrap();
    bar.conn
        .execute(
            "INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                tab_id,
                shift_id,
                waiter_id,
                bar.cashier,
                fixture::NOW + 4_000 + offset,
            ],
        )
        .unwrap();
    let order_id = bar.conn.last_insert_rowid();
    let recipe_id: i64 = bar
        .conn
        .query_row(
            "SELECT id FROM recipes
              WHERE sale_item_id = ?1 AND effective_to IS NULL",
            [bar.beer_bottle],
            |row| row.get(0),
        )
        .unwrap();
    bar.conn
        .execute(
            "INSERT INTO order_lines
                 (order_id, sale_item_id, sale_item_name, recipe_id,
                  quantity_milli, unit_price_minor, line_total_minor)
             VALUES (?1, ?2, 'Property bill', ?3, 1000, ?4, ?4)",
            rusqlite::params![order_id, bar.beer_bottle, recipe_id, liability_minor],
        )
        .unwrap();
    bar.conn
        .execute(
            "UPDATE orders SET status = 'PRINTING' WHERE id = ?1",
            [order_id],
        )
        .unwrap();
    bar.conn
        .execute(
            "UPDATE orders SET status = 'ISSUED', issued_at = ?2 WHERE id = ?1",
            rusqlite::params![order_id, fixture::NOW + 4_500 + offset],
        )
        .unwrap();
    bar.conn
        .execute(
            "UPDATE tabs
                SET status = 'CLOSED', closed_shift_id = ?2, closed_at = ?3,
                    closed_by = ?4
              WHERE id = ?1",
            rusqlite::params![tab_id, shift_id, fixture::NOW + 5_000 + offset, bar.cashier,],
        )
        .unwrap();
    cash::freeze_payment(
        &bar.conn,
        &cash::NewTabPayment {
            tab_id,
            comp_reason: None,
            shift_id,
            created_by: bar.cashier,
            created_at: fixture::NOW + 5_001 + offset,
        },
    )
    .unwrap()
    .liability
    .minor()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    #[test]
    fn inv_1_stock_on_hand_is_only_the_sum_of_its_ledger(
        movements in prop::collection::vec(
            (-250_000i64..=250_000).prop_filter("a movement cannot be zero", |q| *q != 0),
            1..32,
        )
    ) {
        let bar = fixture::bar();
        let mut expected = 0i64;
        for quantity in movements {
            stock::post(
                &bar.conn,
                &stock::Movement::new(
                    bar.beer,
                    stock::Kind::StockCorrection,
                    Milli::from_thousandths(quantity),
                    fixture::NOW,
                    bar.owner,
                )
                .because("generated count correction"),
            )
            .unwrap();
            expected = expected.checked_add(quantity).unwrap();
        }

        let independent: i64 = bar.conn.query_row(
            "SELECT SUM(quantity_milli) FROM stock_movements WHERE product_id = ?1",
            [bar.beer],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(independent, expected);
        prop_assert_eq!(stock::on_hand(&bar.conn, bar.beer).unwrap().thousandths(), expected);

        let product_columns = bar.conn
            .prepare("PRAGMA table_info(products)").unwrap()
            .query_map([], |row| row.get::<_, String>(1)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        let has_stored_stock = product_columns.iter().any(|name| {
            matches!(name.as_str(), "on_hand" | "on_hand_milli" | "stock_on_hand")
        });
        prop_assert!(!has_stored_stock);
    }

    #[test]
    fn inv_2_purchase_movements_match_lines_and_exact_totals_recompute_average_cost(
        deliveries in prop::collection::vec((1_000i64..100_000, 1i64..2_000_000), 1..20),
    ) {
        let bar = fixture::bar();
        bar.conn.execute(
            "INSERT INTO suppliers (name, normalized_name, created_at)
             VALUES ('Generated Supplier', 'generated supplier', ?1)",
            [fixture::NOW],
        ).unwrap();
        let supplier_id = bar.conn.last_insert_rowid();

        let mut existing_quantity = 0i64;
        let mut expected_average = 0i64;
        for (index, (quantity, exact_line_total)) in deliveries.iter().copied().enumerate() {
            let invoice = format!("PROP-{index}");
            bar.conn.execute(
                "INSERT INTO purchases
                     (supplier_id, invoice_ref, received_at, total_cost_minor,
                      created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    supplier_id,
                    invoice,
                    fixture::NOW + i64::try_from(index).unwrap(),
                    exact_line_total,
                    bar.owner,
                    fixture::NOW + i64::try_from(index).unwrap(),
                ],
            ).unwrap();
            let purchase_id = bar.conn.last_insert_rowid();
            let unit_cost = half_up_ratio(
                i128::from(exact_line_total) * 1_000,
                i128::from(quantity),
            );
            bar.conn.execute(
                "INSERT INTO purchase_lines
                     (purchase_id, product_id, quantity_milli,
                      unit_cost_minor, line_cost_minor)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    purchase_id,
                    bar.beer,
                    quantity,
                    unit_cost,
                    exact_line_total,
                ],
            ).unwrap();

            expected_average = half_up_ratio(
                i128::from(expected_average) * i128::from(existing_quantity)
                    + i128::from(exact_line_total) * 1_000,
                i128::from(existing_quantity + quantity),
            );
            bar.conn.execute(
                "UPDATE products SET avg_cost_minor = ?2 WHERE id = ?1",
                rusqlite::params![bar.beer, expected_average],
            ).unwrap();
            stock::post(
                &bar.conn,
                &stock::Movement::new(
                    bar.beer,
                    stock::Kind::Purchase,
                    Milli::from_thousandths(quantity),
                    fixture::NOW + i64::try_from(index).unwrap(),
                    bar.owner,
                )
                .for_purchase(purchase_id)
                .costing(Money::from_minor(unit_cost))
                .because(&invoice),
            ).unwrap();
            existing_quantity += quantity;
        }

        let unmatched: i64 = bar.conn.query_row(
            "SELECT COUNT(*)
               FROM stock_movements movement
              WHERE movement.movement_type = 'PURCHASE'
                AND NOT EXISTS (
                    SELECT 1 FROM purchase_lines line
                     WHERE line.purchase_id = movement.purchase_id
                       AND line.product_id = movement.product_id
                       AND line.quantity_milli = movement.quantity_milli
                )",
            [],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(unmatched, 0);

        let stored_average: i64 = bar.conn.query_row(
            "SELECT avg_cost_minor FROM products WHERE id = ?1",
            [bar.beer],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(stored_average, expected_average);
        prop_assert_eq!(
            stock::on_hand(&bar.conn, bar.beer).unwrap().thousandths(),
            existing_quantity,
        );

        let immutable = bar.conn.execute(
            "UPDATE purchase_lines SET line_cost_minor = line_cost_minor + 1
              WHERE id = (SELECT MIN(id) FROM purchase_lines)",
            [],
        );
        prop_assert!(immutable.is_err());
    }

    #[test]
    fn inv_3_tab_total_counts_only_issued_order_lines(
        history in prop::collection::vec((0u8..6, 1_000i64..10_000), 1..20),
    ) {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 0);
        let tab = open_tab(&bar, shift.id, 0);
        let mut expected = 0i64;

        for (index, (state, quantity)) in history.iter().copied().enumerate() {
            let order = draft_with_beer(&bar, shift.id, tab.id, quantity, index);
            let line_total = orders::lines(&bar.conn, order.id).unwrap()[0].line_total.minor();
            match state {
                0 => {}
                1 => {
                    let _receipt = prepare_issue(&bar, order.id, index as i64);
                }
                2 => {
                    let receipt = prepare_issue(&bar, order.id, index as i64);
                    finish_issue(
                        &bar,
                        shift.id,
                        order.id,
                        &receipt,
                        receipts::FinalOutcome::Success,
                        index as i64,
                    );
                    expected += line_total;
                }
                3 => {
                    let receipt = prepare_issue(&bar, order.id, index as i64);
                    finish_issue(
                        &bar,
                        shift.id,
                        order.id,
                        &receipt,
                        receipts::FinalOutcome::Success,
                        index as i64,
                    );
                    bar.conn.execute(
                        "UPDATE orders SET status = 'REPLACED' WHERE id = ?1",
                        [order.id],
                    ).unwrap();
                }
                4 => {
                    let receipt = prepare_issue(&bar, order.id, index as i64);
                    finish_issue(
                        &bar,
                        shift.id,
                        order.id,
                        &receipt,
                        receipts::FinalOutcome::Success,
                        index as i64,
                    );
                    bar.conn.execute(
                        "UPDATE orders
                            SET status = 'VOIDED', void_reason = 'generated void',
                                voided_at = ?2, voided_by = ?3
                          WHERE id = ?1",
                        rusqlite::params![
                            order.id,
                            fixture::NOW + 10_000 + index as i64,
                            bar.cashier,
                        ],
                    ).unwrap();
                }
                5 => orders::abandon(&bar.conn, order.id).unwrap(),
                _ => unreachable!(),
            }
        }

        let independent: i64 = bar.conn.query_row(
            "SELECT COALESCE(SUM(line.line_total_minor), 0)
               FROM orders order_row
               JOIN order_lines line ON line.order_id = order_row.id
              WHERE order_row.tab_id = ?1 AND order_row.status = 'ISSUED'",
            [tab.id],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(independent, expected);
        prop_assert_eq!(tabs::running_total(&bar.conn, tab.id).unwrap().minor(), expected);
    }

    #[test]
    fn inv_4_receipt_sequences_stay_dense_and_abandoned_br_numbers_stay_void(
        abandoned_issue in prop::collection::vec(any::<bool>(), 1..20),
        customer_totals in prop::collection::vec(1i64..200_000, 0..12),
    ) {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 0);
        let issue_tab = open_tab(&bar, shift.id, 0);

        for (index, abandoned) in abandoned_issue.iter().copied().enumerate() {
            let order = draft_with_beer(&bar, shift.id, issue_tab.id, 1_000, index);
            let receipt = receipts::create_issue(
                &bar.conn,
                order.id,
                receipts::Destination::Bar,
                fixture::NOW + 1_000 + index as i64,
            ).unwrap();
            if abandoned {
                receipts::mark_void(&bar.conn, receipt.id).unwrap();
                orders::abandon(&bar.conn, order.id).unwrap();
            }
        }

        for (index, total) in customer_totals.iter().copied().enumerate() {
            let tab = open_tab(&bar, shift.id, index + 1);
            close_tab_with_payment(&bar, shift.id, tab.id, total, index as i64);
            receipts::create_customer(
                &bar.conn,
                tab.id,
                bar.cashier,
                fixture::NOW + 20_000 + index as i64,
            ).unwrap();
        }

        for (kind, expected_count) in [
            ("ISSUE", abandoned_issue.len()),
            ("CUSTOMER", customer_totals.len()),
        ] {
            let numbers = bar.conn.prepare(
                "SELECT sequence_no FROM receipts
                  WHERE receipt_type = ?1 ORDER BY sequence_no",
            ).unwrap()
                .query_map([kind], |row| row.get::<_, i64>(0)).unwrap()
                .collect::<rusqlite::Result<Vec<_>>>().unwrap();
            let expected: Vec<i64> = (1..=i64::try_from(expected_count).unwrap()).collect();
            prop_assert_eq!(numbers, expected);
        }
        let lost_abandoned: i64 = bar.conn.query_row(
            "SELECT COUNT(*)
               FROM orders order_row
              WHERE order_row.status = 'ABANDONED'
                AND NOT EXISTS (
                    SELECT 1 FROM receipts receipt
                     WHERE receipt.order_id = order_row.id
                       AND receipt.receipt_type = 'ISSUE'
                       AND receipt.status = 'VOID'
                )",
            [],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(lost_abandoned, 0);
    }

    #[test]
    fn inv_5_generated_correction_chains_have_one_leaf_and_never_fork(
        chain_lengths in prop::collection::vec(0usize..7, 1..6),
    ) {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 0);
        let tab = open_tab(&bar, shift.id, 0);

        for (chain_index, replacements) in chain_lengths.iter().copied().enumerate() {
            let root = draft_with_beer(&bar, shift.id, tab.id, 1_000, chain_index * 10);
            let receipt = prepare_issue(&bar, root.id, (chain_index * 10) as i64);
            finish_issue(
                &bar,
                shift.id,
                root.id,
                &receipt,
                receipts::FinalOutcome::Success,
                (chain_index * 10) as i64,
            );
            let root_id = root.id;
            let mut previous = root;

            for link in 0..replacements {
                let replacement = orders::create_replacement(
                    &bar.conn,
                    previous.id,
                    bar.cashier,
                    fixture::NOW + 30_000 + (chain_index * 10 + link) as i64,
                ).unwrap();
                let original_line = orders::lines(&bar.conn, previous.id).unwrap()[0].clone();
                orders::add_line_from_original(
                    &bar.conn,
                    replacement.id,
                    original_line.id,
                    original_line.quantity,
                ).unwrap();
                let replacement_receipt = prepare_issue(
                    &bar,
                    replacement.id,
                    30_000 + (chain_index * 10 + link) as i64,
                );
                finish_issue(
                    &bar,
                    shift.id,
                    replacement.id,
                    &replacement_receipt,
                    receipts::FinalOutcome::Success,
                    30_000 + (chain_index * 10 + link) as i64,
                );
                bar.conn.execute(
                    "UPDATE orders SET status = 'REPLACED' WHERE id = ?1",
                    [previous.id],
                ).unwrap();
                previous = replacement;
            }

            if replacements > 0 {
                let fork = orders::create_replacement(
                    &bar.conn,
                    root_id,
                    bar.cashier,
                    fixture::NOW + 50_000 + chain_index as i64,
                );
                prop_assert!(fork.is_err(), "a replaced root accepted a second branch");
            }
        }

        let families_without_one_leaf: i64 = bar.conn.query_row(
            "SELECT COUNT(*) FROM (
                 SELECT COALESCE(root_order_id, id) AS family,
                        SUM(CASE WHEN status <> 'REPLACED' THEN 1 ELSE 0 END) AS leaves
                   FROM orders
                  GROUP BY COALESCE(root_order_id, id)
                 HAVING leaves <> 1
             )",
            [],
            |row| row.get(0),
        ).unwrap();
        let forked_orders: i64 = bar.conn.query_row(
            "SELECT COUNT(*) FROM (
                 SELECT replaces_order_id
                   FROM orders
                  WHERE replaces_order_id IS NOT NULL
                  GROUP BY replaces_order_id
                 HAVING COUNT(*) > 1
             )",
            [],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(families_without_one_leaf, 0);
        prop_assert_eq!(forked_orders, 0);
    }

    #[test]
    fn inv_6_waiter_held_balance_is_liability_less_finalized_settlements(
        liabilities in prop::collection::vec(1i64..200_000, 1..12),
        settlement_selector in any::<u64>(),
    ) {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 0);
        let mut tab_ids = Vec::with_capacity(liabilities.len());
        let mut total = 0i64;
        for (index, liability) in liabilities.iter().copied().enumerate() {
            let tab = open_tab(&bar, shift.id, index);
            total += close_tab_with_payment(&bar, shift.id, tab.id, liability, index as i64);
            tab_ids.push(tab.id);
        }

        let settled = i64::try_from(settlement_selector % u64::try_from(total).unwrap()).unwrap() + 1;
        let reconciliation = cash::create_reconciliation(
            &bar.conn,
            &cash::NewReconciliation {
                waiter_id: bar.sara,
                cashier_id: bar.cashier,
                expected: Money::from_minor(total),
                cash: Money::from_minor(settled),
                non_cash: Money::ZERO,
                written_off: Money::ZERO,
                write_off_reason: None,
                shift_id: shift.id,
                created_at: fixture::NOW + 60_000,
            },
        ).unwrap();
        for tab_id in tab_ids {
            cash::allocate_tab(&bar.conn, reconciliation.id, tab_id, bar.cashier).unwrap();
        }
        cash::finalize_reconciliation(
            &bar.conn,
            reconciliation.id,
            bar.cashier,
            fixture::NOW + 60_001,
        ).unwrap();

        let independent: i64 = bar.conn.query_row(
            "SELECT
                 COALESCE((SELECT SUM(liability_minor) FROM tab_payments
                            WHERE waiter_id = ?1), 0)
               - COALESCE((SELECT SUM(cash_minor + non_cash_minor + written_off_minor)
                             FROM reconciliations
                            WHERE waiter_id = ?1 AND finalized_at IS NOT NULL), 0)",
            [bar.sara],
            |row| row.get(0),
        ).unwrap();
        prop_assert!(independent >= 0);
        prop_assert_eq!(independent, total - settled);
        prop_assert_eq!(cash::held_balance(&bar.conn, bar.sara).unwrap().minor(), independent);
    }

    #[test]
    fn inv_7_expected_cash_is_only_the_cash_movement_ledger(
        opening_float in 0i64..200_000,
        adjustments in prop::collection::vec(
            (-50_000i64..=50_000).prop_filter("cash movements cannot be zero", |v| *v != 0),
            0..20,
        ),
        tab_liability in 1i64..200_000,
    ) {
        let bar = fixture::bar();
        let shift = open_shift(&bar, opening_float);
        let before_payment = cash::expected_cash(&bar.conn, shift.id).unwrap().minor();
        let tab = open_tab(&bar, shift.id, 0);
        close_tab_with_payment(&bar, shift.id, tab.id, tab_liability, 0);
        prop_assert_eq!(cash::expected_cash(&bar.conn, shift.id).unwrap().minor(), before_payment);

        let mut expected = opening_float;
        for (index, amount) in adjustments.iter().copied().enumerate() {
            cash::record_adjustment(
                &bar.conn,
                shift.id,
                Money::from_minor(amount),
                "generated drawer correction",
                bar.cashier,
                fixture::NOW + 70_000 + index as i64,
            ).unwrap();
            expected += amount;
        }
        let independent: i64 = bar.conn.query_row(
            "SELECT COALESCE(SUM(amount_minor), 0)
               FROM cash_movements WHERE shift_id = ?1",
            [shift.id],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(independent, expected);
        prop_assert_eq!(cash::expected_cash(&bar.conn, shift.id).unwrap().minor(), expected);

        let cash_columns = bar.conn
            .prepare("PRAGMA table_info(cash_movements)").unwrap()
            .query_map([], |row| row.get::<_, String>(1)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        let cash_references_payment = cash_columns.iter().any(|name| {
            matches!(name.as_str(), "tab_id" | "tab_payment_id")
        });
        prop_assert!(!cash_references_payment);
    }

    #[test]
    fn inv_8_generated_audit_histories_verify_and_name_the_first_tamper(
        entities in prop::collection::vec(1i64..10_000, 1..32),
        tamper_selector in any::<usize>(),
    ) {
        let conn = crate::db::open_in_memory().unwrap();
        for (offset, entity_id) in entities.iter().enumerate() {
            ledger::append(
                &conn,
                &Event::new("GENERATED_EVENT", "generated", fixture::NOW + offset as i64)
                    .about(*entity_id)
                    .recording("property history"),
            ).unwrap();
        }
        prop_assert_eq!(
            ledger::verify(&conn).unwrap(),
            ChainStatus::Intact { rows: entities.len() }
        );

        let changed_sequence = i64::try_from(tamper_selector % entities.len() + 1).unwrap();
        conn.execute_batch("DROP TRIGGER audit_log_no_update").unwrap();
        conn.execute(
            "UPDATE audit_log SET action = 'TAMPERED' WHERE sequence_no = ?1",
            [changed_sequence],
        ).unwrap();
        prop_assert_eq!(
            ledger::verify(&conn).unwrap(),
            ChainStatus::Broken {
                sequence_no: changed_sequence,
                reason: BreakReason::ContentAltered,
            }
        );
    }

    #[test]
    fn inv_9_schema_never_allows_two_open_shifts(attempts in 0usize..20) {
        let bar = fixture::bar();
        let _first = open_shift(&bar, 0);

        for index in 0..attempts {
            let duplicate = bar.conn.execute(
                "INSERT INTO shifts
                     (code, business_date, status, opened_at, opened_by,
                      opening_float_minor, expected_end_at)
                 VALUES (?1, ?2, 'OPEN', ?3, ?4, 0, ?5)",
                rusqlite::params![
                    format!("SHIFT-PROP-{index}"),
                    format!("2025-09-{:02}", index + 1),
                    fixture::NOW + 80_000 + index as i64,
                    bar.cashier,
                    fixture::NOW + 90_000 + index as i64,
                ],
            );
            prop_assert!(duplicate.is_err());
        }
        let open_count: i64 = bar.conn.query_row(
            "SELECT COUNT(*) FROM shifts WHERE status = 'OPEN'",
            [],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(open_count, 1);
    }

    #[test]
    fn inv_11_inclusive_and_exclusive_tax_match_an_independent_half_up_oracle(
        line_minor in 0i64..1_000_000_000,
        tax_bp in 0u32..=10_000,
        service_bp in 0u32..=10_000,
    ) {
        let line = Money::from_minor(line_minor);
        let service_exclusive = half_up_ratio(
            i128::from(line_minor) * i128::from(service_bp),
            10_000,
        );
        let taxable = line_minor + service_exclusive;
        let tax_exclusive = half_up_ratio(
            i128::from(taxable) * i128::from(tax_bp),
            10_000,
        );
        let exclusive = Bill::calculate(
            line,
            &ChargeConfig {
                tax_enabled: true,
                tax_rate: BasisPoints(tax_bp),
                tax_inclusive: false,
                service_enabled: true,
                service_rate: BasisPoints(service_bp),
            },
        ).unwrap();
        prop_assert_eq!(exclusive.net.minor(), line_minor);
        prop_assert_eq!(exclusive.service_charge.minor(), service_exclusive);
        prop_assert_eq!(exclusive.tax.minor(), tax_exclusive);
        prop_assert_eq!(exclusive.total.minor(), taxable + tax_exclusive);

        let divisor = 10_000i128 + i128::from(tax_bp);
        let net = half_up_ratio(i128::from(line_minor) * 10_000, divisor);
        let tax_on_lines = line_minor - net;
        let service = half_up_ratio(i128::from(net) * i128::from(service_bp), 10_000);
        let tax_on_service = half_up_ratio(
            i128::from(service) * i128::from(tax_bp),
            10_000,
        );
        let inclusive = Bill::calculate(
            line,
            &ChargeConfig {
                tax_enabled: true,
                tax_rate: BasisPoints(tax_bp),
                tax_inclusive: true,
                service_enabled: true,
                service_rate: BasisPoints(service_bp),
            },
        ).unwrap();
        prop_assert_eq!(inclusive.net.minor(), net);
        prop_assert_eq!(inclusive.service_charge.minor(), service);
        prop_assert_eq!(inclusive.tax.minor(), tax_on_lines + tax_on_service);
        prop_assert_eq!(inclusive.total.minor(), net + service + tax_on_lines + tax_on_service);
    }

    #[test]
    fn inv_12_issued_orders_have_terminal_receipts_and_unknown_attempts_block_close(
        handwritten in prop::collection::vec(any::<bool>(), 1..20),
    ) {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 0);
        let tab = open_tab(&bar, shift.id, 0);
        let mut receipts_by_order = Vec::with_capacity(handwritten.len());

        for (index, failed) in handwritten.iter().copied().enumerate() {
            let order = draft_with_beer(&bar, shift.id, tab.id, 1_000, index);
            let receipt = prepare_issue(&bar, order.id, index as i64);
            finish_issue(
                &bar,
                shift.id,
                order.id,
                &receipt,
                if failed {
                    receipts::FinalOutcome::Failed
                } else {
                    receipts::FinalOutcome::Success
                },
                index as i64,
            );
            receipts_by_order.push(receipt);
        }

        let invalid_issued: i64 = bar.conn.query_row(
            "SELECT COUNT(*)
               FROM orders order_row
              WHERE order_row.status = 'ISSUED'
                AND NOT EXISTS (
                    SELECT 1
                      FROM receipts receipt
                     WHERE receipt.order_id = order_row.id
                       AND receipt.receipt_type = 'ISSUE'
                       AND (
                           receipt.status = 'PRINTED'
                           OR (receipt.status = 'FAILED' AND EXISTS (
                               SELECT 1 FROM receipt_prints attempt
                                WHERE attempt.receipt_id = receipt.id
                                  AND attempt.outcome = 'FAILED'
                                  AND TRIM(attempt.reason) <> ''
                           ))
                       )
                )",
            [],
            |row| row.get(0),
        ).unwrap();
        prop_assert_eq!(invalid_issued, 0);

        // A reprint is recorded UNKNOWN before device I/O. The order itself
        // is already issued, so this isolates the UNKNOWN recovery gate from
        // the separate PRINTING-order gate.
        let receipt = &receipts_by_order[0];
        let attempt = receipts::begin_attempt(
            &bar.conn,
            receipt.id,
            "generated reprint check",
            shift.id,
            bar.cashier,
            fixture::NOW + 100_000,
        ).unwrap();
        prop_assert_eq!(attempt.outcome, receipts::Outcome::Unknown);
        prop_assert!(!shifts::recovery_complete(&bar.conn).unwrap());
        let close = shifts::begin_closing(&bar.conn, shift.id, bar.cashier);
        prop_assert!(close.is_err());
        prop_assert_eq!(shifts::find(&bar.conn, shift.id).unwrap().status, shifts::Status::Open);
    }
}

#[test]
fn inv_13_every_reachable_non_terminal_state_has_one_resolution_command() {
    let mut commands = HashSet::new();
    for state in ReachableNonTerminalState::ALL {
        let registered = resolution_for(*state);
        assert!(
            !registered.command.as_str().trim().is_empty(),
            "{state:?} registered a blank command"
        );
        assert!(
            !registered.purpose.trim().is_empty(),
            "{state:?} registered no recovery purpose"
        );
        assert!(
            commands.insert(registered.command),
            "{state:?} reuses a command and is not independently resolvable"
        );
    }
}
