//! Order drafts, immutable sale snapshots, and correction records (§6).
//!
//! This module deliberately exposes statement-level persistence operations.
//! It never begins or commits a transaction: the command layer owns the
//! three transaction boundaries around printing. Multi-statement helpers such
//! as [`freeze_correction`], [`apply_pending_correction`], and [`record_void`]
//! therefore require the caller to keep the supplied connection inside one
//! transaction.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension};

use crate::{Milli, Money};

use super::{guarded, RepoError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Draft,
    Printing,
    Issued,
    Replaced,
    Voided,
    Abandoned,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "DRAFT",
            Self::Printing => "PRINTING",
            Self::Issued => "ISSUED",
            Self::Replaced => "REPLACED",
            Self::Voided => "VOIDED",
            Self::Abandoned => "ABANDONED",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        parse_status(text).map_err(RepoError::Sqlite)
    }
}

fn parse_status(text: &str) -> rusqlite::Result<Status> {
    match text {
        "DRAFT" => Ok(Status::Draft),
        "PRINTING" => Ok(Status::Printing),
        "ISSUED" => Ok(Status::Issued),
        "REPLACED" => Ok(Status::Replaced),
        "VOIDED" => Ok(Status::Voided),
        "ABANDONED" => Ok(Status::Abandoned),
        other => Err(invalid_text(5, "order status", other)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: i64,
    pub tab_id: i64,
    pub shift_id: i64,
    pub waiter_id: i64,
    pub cashier_id: i64,
    pub status: Status,
    pub created_at: i64,
    pub issued_at: Option<i64>,
    pub replaces_order_id: Option<i64>,
    pub root_order_id: Option<i64>,
    pub void_reason: Option<String>,
    pub voided_at: Option<i64>,
    pub voided_by: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub id: i64,
    pub order_id: i64,
    pub sale_item_id: i64,
    pub sale_item_name: String,
    pub recipe_id: i64,
    pub quantity: Milli,
    pub unit_price: Money,
    pub line_total: Money,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewDraft {
    pub tab_id: i64,
    pub shift_id: i64,
    pub cashier_id: i64,
    pub created_at: i64,
}

const ORDER_COLUMNS: &str = "id, tab_id, shift_id, waiter_id, cashier_id, status, created_at,
     issued_at, replaces_order_id, root_order_id, void_reason, voided_at, voided_by";

fn read_order(row: &rusqlite::Row<'_>) -> rusqlite::Result<Order> {
    let status: String = row.get(5)?;
    Ok(Order {
        id: row.get(0)?,
        tab_id: row.get(1)?,
        shift_id: row.get(2)?,
        waiter_id: row.get(3)?,
        cashier_id: row.get(4)?,
        status: parse_status(&status)?,
        created_at: row.get(6)?,
        issued_at: row.get(7)?,
        replaces_order_id: row.get(8)?,
        root_order_id: row.get(9)?,
        void_reason: row.get(10)?,
        voided_at: row.get(11)?,
        voided_by: row.get(12)?,
    })
}

fn read_line(row: &rusqlite::Row<'_>) -> rusqlite::Result<Line> {
    Ok(Line {
        id: row.get(0)?,
        order_id: row.get(1)?,
        sale_item_id: row.get(2)?,
        sale_item_name: row.get(3)?,
        recipe_id: row.get(4)?,
        quantity: Milli::from_thousandths(row.get(5)?),
        unit_price: Money::from_minor(row.get(6)?),
        line_total: Money::from_minor(row.get(7)?),
    })
}

pub fn find(conn: &Connection, id: i64) -> Result<Order> {
    conn.query_row(
        &format!("SELECT {ORDER_COLUMNS} FROM orders WHERE id = ?1"),
        [id],
        read_order,
    )
    .map_err(|err| missing(err, "order"))
}

pub fn for_tab(conn: &Connection, tab_id: i64) -> Result<Vec<Order>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ORDER_COLUMNS} FROM orders WHERE tab_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([tab_id], read_order)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Orders that require the non-dismissible print recovery path (D10).
pub fn stranded_prints(conn: &Connection) -> Result<Vec<Order>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ORDER_COLUMNS} FROM orders WHERE status = 'PRINTING' ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], read_order)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn lines(conn: &Connection, order_id: i64) -> Result<Vec<Line>> {
    let mut stmt = conn.prepare(
        "SELECT id, order_id, sale_item_id, sale_item_name, recipe_id,
                quantity_milli, unit_price_minor, line_total_minor
           FROM order_lines WHERE order_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([order_id], read_line)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Create a draft for the tab's current waiter.
///
/// Looking the waiter up here prevents a transferred tab from producing a new
/// order stamped with its former owner. The shift and tab checks run before
/// the insert as readable preconditions; the schema remains the final guard.
pub fn create(conn: &Connection, draft: NewDraft) -> Result<Order> {
    require_active_cashier(conn, draft.cashier_id)?;
    require_open_shift(conn, draft.shift_id)?;
    let waiter_id: i64 = conn
        .query_row(
            "SELECT waiter_id FROM tabs WHERE id = ?1 AND status = 'OPEN'",
            [draft.tab_id],
            |row| row.get(0),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                RepoError::Refused("an order needs a tab that is still open".into())
            }
            other => RepoError::Sqlite(other),
        })?;

    guarded!(conn.execute(
        "INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            draft.tab_id,
            draft.shift_id,
            waiter_id,
            draft.cashier_id,
            draft.created_at
        ],
    ))?;
    find(conn, conn.last_insert_rowid())
}

/// Start a correction chain member with the original sale's tab, shift and
/// waiter, but the cashier who is performing the correction now.
pub fn create_replacement(
    conn: &Connection,
    original_order_id: i64,
    cashier_id: i64,
    created_at: i64,
) -> Result<Order> {
    require_active_cashier(conn, cashier_id)?;
    let original = find(conn, original_order_id)?;
    if original.status != Status::Issued {
        return super::refuse("only an issued order can be corrected");
    }
    require_open_shift(conn, original.shift_id)?;
    require_open_tab(conn, original.tab_id)?;

    guarded!(conn.execute(
        "INSERT INTO orders
             (tab_id, shift_id, waiter_id, cashier_id, created_at,
              replaces_order_id, root_order_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            original.tab_id,
            original.shift_id,
            original.waiter_id,
            cashier_id,
            created_at,
            original.id,
            original.root_order_id.unwrap_or(original.id),
        ],
    ))?;
    find(conn, conn.last_insert_rowid())
}

/// Add a line using the recipe, name and price that are current now.
pub fn add_line(
    conn: &Connection,
    order_id: i64,
    sale_item_id: i64,
    quantity: Milli,
) -> Result<Line> {
    let must_copy_snapshot: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
               FROM orders replacement
               JOIN order_lines original_line
                 ON original_line.order_id = replacement.replaces_order_id
              WHERE replacement.id = ?1 AND original_line.sale_item_id = ?2
         )",
        rusqlite::params![order_id, sale_item_id],
        |row| row.get(0),
    )?;
    if must_copy_snapshot {
        return super::refuse(
            "an item already on the corrected order must keep its original recipe and price",
        );
    }
    let item = super::catalogue::menu_item(conn, sale_item_id)?;
    insert_line(
        conn,
        order_id,
        item.sale_item_id,
        &item.name,
        item.recipe_id,
        quantity,
        item.price,
    )
}

/// Add a correction line from the exact snapshot on the original order.
///
/// The original line id is explicit because duplicate sale-item lines are
/// legal. Guessing by sale item could silently select the wrong historical
/// recipe or price when two snapshots share the same item id.
pub fn add_line_from_original(
    conn: &Connection,
    replacement_order_id: i64,
    original_line_id: i64,
    quantity: Milli,
) -> Result<Line> {
    let snapshot = conn
        .query_row(
            "SELECT l.sale_item_id, l.sale_item_name, l.recipe_id, l.unit_price_minor
               FROM order_lines l
               JOIN orders replacement ON replacement.id = ?1
              WHERE l.id = ?2 AND l.order_id = replacement.replaces_order_id",
            rusqlite::params![replacement_order_id, original_line_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    Money::from_minor(row.get(3)?),
                ))
            },
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => RepoError::Refused(
                "a replacement can only copy a line from the order it corrects".into(),
            ),
            other => RepoError::Sqlite(other),
        })?;
    insert_line(
        conn,
        replacement_order_id,
        snapshot.0,
        &snapshot.1,
        snapshot.2,
        quantity,
        snapshot.3,
    )
}

fn insert_line(
    conn: &Connection,
    order_id: i64,
    sale_item_id: i64,
    sale_item_name: &str,
    recipe_id: i64,
    quantity: Milli,
    unit_price: Money,
) -> Result<Line> {
    let line_total = checked_line_total(quantity, unit_price)?;
    guarded!(conn.execute(
        "INSERT INTO order_lines
             (order_id, sale_item_id, sale_item_name, recipe_id,
              quantity_milli, unit_price_minor, line_total_minor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            order_id,
            sale_item_id,
            sale_item_name,
            recipe_id,
            quantity.thousandths(),
            unit_price.minor(),
            line_total.minor(),
        ],
    ))?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT id, order_id, sale_item_id, sale_item_name, recipe_id,
                quantity_milli, unit_price_minor, line_total_minor
           FROM order_lines WHERE id = ?1",
        [id],
        read_line,
    )
    .map_err(RepoError::Sqlite)
}

fn checked_line_total(quantity: Milli, unit_price: Money) -> Result<Money> {
    if quantity.is_zero() || quantity.is_negative() {
        return super::refuse("an order-line quantity must be greater than zero");
    }
    if unit_price.is_negative() {
        return super::refuse("an order-line price cannot be negative");
    }
    let total = quantity
        .thousandths()
        .checked_mul(unit_price.minor())
        .and_then(|scaled| scaled.checked_add(500))
        .map(|rounded| rounded / 1_000)
        .ok_or_else(|| RepoError::Refused("that order line is too large to total safely".into()))?;
    Ok(Money::from_minor(total))
}

/// Carry a failed-print draft into the currently open shift.
pub fn carry_draft(conn: &Connection, order_id: i64, new_shift_id: i64) -> Result<()> {
    require_open_shift(conn, new_shift_id)?;
    let changed = guarded!(conn.execute(
        "UPDATE orders SET shift_id = ?2 WHERE id = ?1 AND status = 'DRAFT'",
        rusqlite::params![order_id, new_shift_id],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Draft,
        "only a draft can change shift",
    )
}

/// Transaction 1's order transition. A pending issue receipt must already
/// exist, and the tab is rechecked inside the same caller-owned transaction.
pub fn mark_printing(conn: &Connection, order_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE orders SET status = 'PRINTING'
          WHERE id = ?1 AND status = 'DRAFT'
            AND EXISTS (SELECT 1 FROM tabs t
                         WHERE t.id = orders.tab_id AND t.status = 'OPEN')
            AND EXISTS (SELECT 1 FROM order_lines l
                         JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
                         WHERE l.order_id = orders.id)
            AND NOT EXISTS (
                  SELECT 1
                    FROM order_lines l
                    JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
                    JOIN products p ON p.id = rl.product_id
                   WHERE l.order_id = orders.id
                     AND NOT EXISTS (
                           SELECT 1 FROM receipts r
                            WHERE r.order_id = orders.id
                              AND r.receipt_type = 'ISSUE'
                              AND r.destination = p.destination
                              AND r.status = 'PENDING'))
            AND NOT EXISTS (
                  SELECT 1 FROM receipts r
                   WHERE r.order_id = orders.id
                     AND r.receipt_type = 'ISSUE' AND r.status <> 'VOID'
                     AND NOT EXISTS (
                           SELECT 1
                             FROM order_lines l
                             JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
                             JOIN products p ON p.id = rl.product_id
                            WHERE l.order_id = orders.id
                              AND p.destination = r.destination))",
        [order_id],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Draft,
        "printing needs an open tab and a pending issue receipt",
    )
}

/// Recovery after a confirmed non-print. Every pending issue number must be
/// voided first; the numbers remain stored and are never reused.
pub fn return_to_draft(conn: &Connection, order_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE orders SET status = 'DRAFT'
          WHERE id = ?1 AND status = 'PRINTING'
            AND EXISTS (SELECT 1 FROM receipts r
                         WHERE r.order_id = orders.id AND r.receipt_type = 'ISSUE')
            AND NOT EXISTS (SELECT 1 FROM receipts r
                             WHERE r.order_id = orders.id
                               AND r.receipt_type = 'ISSUE' AND r.status <> 'VOID')",
        [order_id],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Printing,
        "every issue receipt must be void before returning the order to draft",
    )
}

/// Transaction 2's order transition. Stock and audit writes remain the
/// command layer's responsibility in the same caller-owned transaction.
pub fn mark_issued(conn: &Connection, order_id: i64, issued_at: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE orders SET status = 'ISSUED', issued_at = ?2
          WHERE id = ?1 AND status = 'PRINTING'
            AND EXISTS (SELECT 1 FROM tabs t
                         WHERE t.id = orders.tab_id AND t.status = 'OPEN')
            AND EXISTS (SELECT 1 FROM order_lines l
                         JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
                         WHERE l.order_id = orders.id)
            AND NOT EXISTS (
                  SELECT 1
                    FROM order_lines l
                    JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
                    JOIN products p ON p.id = rl.product_id
                   WHERE l.order_id = orders.id
                     AND NOT EXISTS (
                           SELECT 1 FROM receipts r
                            WHERE r.order_id = orders.id
                              AND r.receipt_type = 'ISSUE'
                              AND r.destination = p.destination
                              AND r.status IN ('PRINTED','FAILED')))
            AND NOT EXISTS (
                  SELECT 1 FROM receipts r
                   WHERE r.order_id = orders.id
                     AND r.receipt_type = 'ISSUE' AND r.status <> 'VOID'
                     AND (r.status NOT IN ('PRINTED','FAILED')
                          OR NOT EXISTS (
                               SELECT 1
                                 FROM order_lines l
                                 JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
                                 JOIN products p ON p.id = rl.product_id
                                WHERE l.order_id = orders.id
                                  AND p.destination = r.destination)))
            AND NOT EXISTS (
                  SELECT 1 FROM receipt_prints attempt
                  JOIN receipts r ON r.id = attempt.receipt_id
                 WHERE r.order_id = orders.id AND attempt.outcome = 'UNKNOWN')",
        rusqlite::params![order_id, issued_at],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Printing,
        "issuing needs an open tab and every issue receipt resolved",
    )
}

pub fn abandon(conn: &Connection, order_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE orders SET status = 'ABANDONED'
          WHERE id = ?1 AND status = 'DRAFT'
            AND NOT EXISTS (SELECT 1 FROM receipts r
                             WHERE r.order_id = orders.id AND r.status <> 'VOID')",
        [order_id],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Draft,
        "a draft with a live receipt cannot be abandoned",
    )
}

pub fn mark_replaced(conn: &Connection, original_order_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE orders SET status = 'REPLACED'
          WHERE id = ?1 AND status = 'ISSUED'
            AND EXISTS (SELECT 1 FROM order_corrections c
                         JOIN orders replacement ON replacement.id = c.replacement_order_id
                        WHERE c.original_order_id = orders.id
                          AND c.correction_type = 'CORRECTION'
                          AND replacement.status = 'ISSUED')",
        [original_order_id],
    ))?;
    expect_state_change(
        conn,
        original_order_id,
        changed,
        Status::Issued,
        "an applied, issued replacement is required before replacing the original",
    )
}

pub fn mark_voided(
    conn: &Connection,
    order_id: i64,
    reason: &str,
    voided_at: i64,
    voided_by: i64,
) -> Result<()> {
    require_active_cashier(conn, voided_by)?;
    let reason = require_text(reason, "void reason")?;
    let changed = guarded!(conn.execute(
        "UPDATE orders
            SET status = 'VOIDED', void_reason = ?2, voided_at = ?3, voided_by = ?4
          WHERE id = ?1 AND status = 'ISSUED'
            AND EXISTS (SELECT 1 FROM order_corrections c
                         WHERE c.original_order_id = orders.id
                           AND c.correction_type = 'VOID'
                           AND c.reason = ?2
                           AND c.created_at = ?3
                           AND c.created_by = ?4)",
        rusqlite::params![order_id, reason, voided_at, voided_by],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Issued,
        "record the void correction before voiding the order",
    )
}

/// Release a replacement whose new paper was confirmed not to have printed.
/// The caller voids every receipt first and abandons the detached draft in the
/// same transaction.
pub fn detach_replacement(conn: &Connection, order_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE orders SET replaces_order_id = NULL
          WHERE id = ?1 AND status = 'DRAFT' AND replaces_order_id IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM receipts r
                             WHERE r.order_id = orders.id AND r.status <> 'VOID')",
        [order_id],
    ))?;
    expect_state_change(
        conn,
        order_id,
        changed,
        Status::Draft,
        "only a correction draft with no live receipt can release its original",
    )
}

fn expect_state_change(
    conn: &Connection,
    order_id: i64,
    changed: usize,
    expected: Status,
    message: &str,
) -> Result<()> {
    if changed == 1 {
        return Ok(());
    }
    let actual: Option<String> = conn
        .query_row(
            "SELECT status FROM orders WHERE id = ?1",
            [order_id],
            |row| row.get(0),
        )
        .optional()?;
    match actual {
        None => Err(RepoError::Missing { what: "order" }),
        Some(actual) if actual != expected.as_str() => super::refuse(format!(
            "{message}; the order is {}",
            actual.to_ascii_lowercase()
        )),
        Some(_) => super::refuse(message),
    }
}

fn require_open_shift(conn: &Connection, shift_id: i64) -> Result<()> {
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM shifts WHERE id = ?1",
            [shift_id],
            |row| row.get(0),
        )
        .optional()?;
    match status.as_deref() {
        Some("OPEN") => Ok(()),
        Some(_) => super::refuse("orders belong to the shift that is currently open"),
        None => Err(RepoError::Missing { what: "shift" }),
    }
}

fn require_open_tab(conn: &Connection, tab_id: i64) -> Result<()> {
    let status: Option<String> = conn
        .query_row("SELECT status FROM tabs WHERE id = ?1", [tab_id], |row| {
            row.get(0)
        })
        .optional()?;
    match status.as_deref() {
        Some("OPEN") => Ok(()),
        Some(_) => super::refuse("corrections require a tab that is still open"),
        None => Err(RepoError::Missing { what: "tab" }),
    }
}

fn require_active_cashier(conn: &Connection, person_id: i64) -> Result<()> {
    let actor: Option<(String, bool)> = conn
        .query_row(
            "SELECT role, active FROM staff WHERE id = ?1",
            [person_id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? == 1)),
        )
        .optional()?;
    match actor {
        Some((role, true)) if role == "CASHIER" => Ok(()),
        Some(_) => super::refuse("only an active cashier may operate the till"),
        None => Err(RepoError::Missing { what: "cashier" }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrectionKind {
    Correction,
    Void,
}

impl CorrectionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Correction => "CORRECTION",
            Self::Void => "VOID",
        }
    }
}

fn parse_correction_kind(text: &str) -> rusqlite::Result<CorrectionKind> {
    match text {
        "CORRECTION" => Ok(CorrectionKind::Correction),
        "VOID" => Ok(CorrectionKind::Void),
        other => Err(invalid_text(1, "correction type", other)),
    }
}

/// One product's frozen before/after correction arithmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionDelta {
    pub product_id: i64,
    pub before: Milli,
    pub after: Milli,
    pub delta: Milli,
    pub returned: Milli,
    pub written_off: Milli,
    pub note: String,
}

impl CorrectionDelta {
    /// Build the only valid disposition: additions return nothing; reductions
    /// divide exactly into physical returns and written-off stock.
    pub fn new(
        product_id: i64,
        before: Milli,
        after: Milli,
        returned: Milli,
        note: impl Into<String>,
    ) -> Result<Self> {
        if before.is_negative() || after.is_negative() || returned.is_negative() {
            return super::refuse("correction quantities cannot be negative");
        }
        let delta_value = after
            .thousandths()
            .checked_sub(before.thousandths())
            .ok_or_else(|| RepoError::Refused("that correction quantity is too large".into()))?;
        let returned_value = returned.thousandths();
        let written_off = if delta_value >= 0 {
            if returned_value != 0 {
                return super::refuse(
                    "returned stock is only valid for a product removed from the bill",
                );
            }
            0
        } else {
            let reduction = delta_value.checked_neg().ok_or_else(|| {
                RepoError::Refused("that correction quantity is too large".into())
            })?;
            if returned_value > reduction {
                return super::refuse("more stock was returned than was removed from the bill");
            }
            reduction - returned_value
        };
        Ok(Self {
            product_id,
            before,
            after,
            delta: Milli::from_thousandths(delta_value),
            returned,
            written_off: Milli::from_thousandths(written_off),
            note: note.into(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub id: i64,
    pub kind: CorrectionKind,
    pub original_order_id: i64,
    pub replacement_order_id: Option<i64>,
    pub issue_receipt_number: String,
    pub reason: String,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
    pub lines: Vec<CorrectionDelta>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCorrection {
    pub id: i64,
    pub original_order_id: i64,
    pub replacement_order_id: i64,
    pub issue_receipt_number: String,
    pub reason: String,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
    pub lines: Vec<CorrectionDelta>,
}

#[derive(Clone, Copy, Debug)]
pub struct NewCorrection<'a> {
    pub original_order_id: i64,
    pub replacement_order_id: i64,
    pub issue_receipt_number: &'a str,
    pub reason: &'a str,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct NewVoid<'a> {
    pub original_order_id: i64,
    pub issue_receipt_number: &'a str,
    pub reason: &'a str,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
}

/// Freeze a correction intent before the replacement slip leaves the process.
/// The caller must wrap the header and line inserts in Transaction 1.
pub fn freeze_correction(
    conn: &Connection,
    correction: NewCorrection<'_>,
    deltas: &[CorrectionDelta],
) -> Result<PendingCorrection> {
    require_active_cashier(conn, correction.created_by)?;
    let receipt = require_text(correction.issue_receipt_number, "issue receipt number")?;
    let reason = require_text(correction.reason, "correction reason")?;
    let valid_relationship: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
               FROM orders original
               JOIN orders replacement ON replacement.id = ?2
               JOIN tabs tab ON tab.id = original.tab_id
               JOIN shifts shift ON shift.id = original.shift_id
               JOIN receipts receipt ON receipt.receipt_number = ?3
              WHERE original.id = ?1
                AND original.status = 'ISSUED' AND replacement.status = 'DRAFT'
                AND replacement.replaces_order_id = original.id
                AND replacement.tab_id = original.tab_id
                AND replacement.shift_id = original.shift_id
                AND replacement.cashier_id = ?5
                AND original.shift_id = ?4
                AND tab.status = 'OPEN' AND shift.status = 'OPEN'
                AND receipt.receipt_type = 'ISSUE'
                AND receipt.order_id = original.id
                AND receipt.status IN ('PRINTED','FAILED')
                AND EXISTS (SELECT 1 FROM order_lines line
                             WHERE line.order_id = replacement.id)
         )",
        rusqlite::params![
            correction.original_order_id,
            correction.replacement_order_id,
            receipt,
            correction.shift_id,
            correction.created_by,
        ],
        |row| row.get(0),
    )?;
    if !valid_relationship {
        return super::refuse(
            "a correction needs the linked draft replacement, open original shift and tab, and that order's typed receipt",
        );
    }
    validate_deltas_against_orders(
        conn,
        correction.original_order_id,
        Some(correction.replacement_order_id),
        deltas,
    )?;
    guarded!(conn.execute(
        "INSERT INTO pending_order_corrections
             (original_order_id, replacement_order_id, issue_receipt_number,
              reason, shift_id, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            correction.original_order_id,
            correction.replacement_order_id,
            receipt,
            reason,
            correction.shift_id,
            correction.created_by,
            correction.created_at,
        ],
    ))?;
    let pending_id = conn.last_insert_rowid();
    insert_deltas(
        conn,
        "pending_order_correction_lines",
        "pending_id",
        pending_id,
        deltas,
    )?;
    pending(conn, pending_id)
}

/// Copy the already-frozen intent into the append-only correction ledger.
/// Status changes and stock movements are intentionally separate statements in
/// the same caller-owned Transaction 2.
pub fn apply_pending_correction(conn: &Connection, pending_id: i64) -> Result<Correction> {
    let frozen = pending(conn, pending_id)?;
    let valid_relationship: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
               FROM orders original
               JOIN orders replacement ON replacement.id = ?2
               JOIN tabs tab ON tab.id = original.tab_id
               JOIN shifts shift ON shift.id = original.shift_id
              WHERE original.id = ?1
                AND original.status = 'ISSUED' AND replacement.status = 'ISSUED'
                AND replacement.replaces_order_id = original.id
                AND replacement.tab_id = original.tab_id
                AND replacement.shift_id = original.shift_id
                AND original.shift_id = ?3
                AND tab.status = 'OPEN' AND shift.status = 'OPEN'
         )",
        rusqlite::params![
            frozen.original_order_id,
            frozen.replacement_order_id,
            frozen.shift_id,
        ],
        |row| row.get(0),
    )?;
    if !valid_relationship {
        return super::refuse(
            "the frozen correction no longer has an issued linked replacement on its open tab and shift",
        );
    }
    validate_deltas_against_orders(
        conn,
        frozen.original_order_id,
        Some(frozen.replacement_order_id),
        &frozen.lines,
    )?;
    let changed = guarded!(conn.execute(
        "INSERT INTO order_corrections
             (correction_type, original_order_id, replacement_order_id,
              issue_receipt_number, reason, shift_id, created_by, created_at)
         SELECT 'CORRECTION', original_order_id, replacement_order_id,
                issue_receipt_number, reason, shift_id, created_by, created_at
           FROM pending_order_corrections WHERE id = ?1",
        [pending_id],
    ))?;
    if changed == 0 {
        return Err(RepoError::Missing {
            what: "pending correction",
        });
    }
    let correction_id = conn.last_insert_rowid();
    guarded!(conn.execute(
        "INSERT INTO order_correction_lines
             (correction_id, product_id, before_milli, after_milli, delta_milli,
              returned_milli, written_off_milli, note)
         SELECT ?2, product_id, before_milli, after_milli, delta_milli,
                returned_milli, written_off_milli, note
           FROM pending_order_correction_lines
          WHERE pending_id = ?1 ORDER BY id",
        rusqlite::params![pending_id, correction_id],
    ))?;
    correction(conn, correction_id)
}

/// Persist a void as a correction to an empty order. The caller owns the one
/// transaction that also posts returns, marks the order VOIDED, and audits it.
pub fn record_void(
    conn: &Connection,
    void: NewVoid<'_>,
    deltas: &[CorrectionDelta],
) -> Result<Correction> {
    require_active_cashier(conn, void.created_by)?;
    if deltas.iter().any(|line| !line.after.is_zero()) {
        return super::refuse("a void removes every product from the bill");
    }
    let receipt = require_text(void.issue_receipt_number, "issue receipt number")?;
    let reason = require_text(void.reason, "void reason")?;
    let valid_original: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
               FROM orders original
               JOIN tabs tab ON tab.id = original.tab_id
               JOIN shifts shift ON shift.id = original.shift_id
               JOIN receipts receipt ON receipt.receipt_number = ?2
              WHERE original.id = ?1 AND original.status = 'ISSUED'
                AND original.shift_id = ?3
                AND tab.status = 'OPEN' AND shift.status = 'OPEN'
                AND receipt.receipt_type = 'ISSUE'
                AND receipt.order_id = original.id
                AND receipt.status IN ('PRINTED','FAILED')
         )",
        rusqlite::params![void.original_order_id, receipt, void.shift_id],
        |row| row.get(0),
    )?;
    if !valid_original {
        return super::refuse(
            "a void needs an issued order on its open tab and shift, and that order's typed receipt",
        );
    }
    validate_deltas_against_orders(conn, void.original_order_id, None, deltas)?;
    guarded!(conn.execute(
        "INSERT INTO order_corrections
             (correction_type, original_order_id, replacement_order_id,
              issue_receipt_number, reason, shift_id, created_by, created_at)
         VALUES ('VOID', ?1, NULL, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            void.original_order_id,
            receipt,
            reason,
            void.shift_id,
            void.created_by,
            void.created_at,
        ],
    ))?;
    let correction_id = conn.last_insert_rowid();
    insert_deltas(
        conn,
        "order_correction_lines",
        "correction_id",
        correction_id,
        deltas,
    )?;
    correction(conn, correction_id)
}

fn insert_deltas(
    conn: &Connection,
    table: &str,
    owner_column: &str,
    owner_id: i64,
    deltas: &[CorrectionDelta],
) -> Result<()> {
    // `table` and `owner_column` are constants supplied only by this module;
    // values remain bound parameters.
    let sql = format!(
        "INSERT INTO {table}
             ({owner_column}, product_id, before_milli, after_milli, delta_milli,
              returned_milli, written_off_milli, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
    );
    for delta in deltas {
        guarded!(conn.execute(
            &sql,
            rusqlite::params![
                owner_id,
                delta.product_id,
                delta.before.thousandths(),
                delta.after.thousandths(),
                delta.delta.thousandths(),
                delta.returned.thousandths(),
                delta.written_off.thousandths(),
                delta.note,
            ],
        ))?;
    }
    Ok(())
}

pub(crate) fn expanded_products(conn: &Connection, order_id: i64) -> Result<BTreeMap<i64, Milli>> {
    let mut stmt = conn.prepare(
        "SELECT recipe_id, quantity_milli
           FROM order_lines WHERE order_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([order_id], |row| {
        Ok((row.get::<_, i64>(0)?, Milli::from_thousandths(row.get(1)?)))
    })?;
    let snapshots = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    // Use the same expansion routine as issue and stock posting. It aggregates
    // duplicate recipe lines before rounding and ignores products the venue
    // does not track; an independent SQL formula could disagree with D9.
    let mut totals = BTreeMap::<i64, Milli>::new();
    for (recipe_id, quantity) in snapshots {
        for consumed in super::catalogue::expand(conn, recipe_id, quantity)? {
            let current = totals
                .get(&consumed.product_id)
                .copied()
                .unwrap_or(Milli::ZERO);
            let total = current
                .checked_add(consumed.quantity)
                .map_err(|_| RepoError::Refused("that corrected order is too large".into()))?;
            totals.insert(consumed.product_id, total);
        }
    }
    Ok(totals)
}

type FrozenBillLine = (i64, String, i64, i64, i64, i64);

fn frozen_bill_lines(conn: &Connection, order_id: i64) -> Result<Vec<FrozenBillLine>> {
    let mut stmt = conn.prepare(
        "SELECT sale_item_id, sale_item_name, recipe_id, quantity_milli,
                unit_price_minor, line_total_minor
           FROM order_lines WHERE order_id = ?1
          ORDER BY sale_item_id, sale_item_name, recipe_id, quantity_milli,
                   unit_price_minor, line_total_minor",
    )?;
    let rows = stmt.query_map([order_id], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn validate_deltas_against_orders(
    conn: &Connection,
    original_order_id: i64,
    replacement_order_id: Option<i64>,
    deltas: &[CorrectionDelta],
) -> Result<()> {
    let before = expanded_products(conn, original_order_id)?;
    let after = match replacement_order_id {
        Some(id) => expanded_products(conn, id)?,
        None => BTreeMap::new(),
    };

    if let Some(replacement_id) = replacement_order_id {
        if frozen_bill_lines(conn, original_order_id)? == frozen_bill_lines(conn, replacement_id)? {
            return super::refuse("a correction must actually change the order");
        }
    }
    if deltas.is_empty() && before == after {
        return Ok(());
    }

    let expected_products: BTreeSet<i64> = before.keys().chain(after.keys()).copied().collect();
    let mut seen = BTreeSet::new();
    for line in deltas {
        if !seen.insert(line.product_id) {
            return super::refuse("a correction has exactly one delta per product");
        }
        let expected_before = before.get(&line.product_id).copied().unwrap_or(Milli::ZERO);
        let expected_after = after.get(&line.product_id).copied().unwrap_or(Milli::ZERO);
        if line.before != expected_before || line.after != expected_after {
            return super::refuse(
                "correction before and after quantities must match the frozen order recipes",
            );
        }
        let expected_delta = expected_after
            .thousandths()
            .checked_sub(expected_before.thousandths())
            .ok_or_else(|| RepoError::Refused("that correction quantity is too large".into()))?;
        if line.delta.thousandths() != expected_delta
            || line.returned.is_negative()
            || line.written_off.is_negative()
        {
            return super::refuse("a correction delta or disposition was altered after validation");
        }
        if expected_delta >= 0 {
            if !line.returned.is_zero() || !line.written_off.is_zero() {
                return super::refuse(
                    "an added or unchanged product cannot be returned or written off",
                );
            }
        } else {
            let reduction = expected_delta.checked_neg().ok_or_else(|| {
                RepoError::Refused("that correction quantity is too large".into())
            })?;
            let disposition = line
                .returned
                .thousandths()
                .checked_add(line.written_off.thousandths())
                .ok_or_else(|| {
                    RepoError::Refused("that correction quantity is too large".into())
                })?;
            if disposition != reduction {
                return super::refuse(
                    "returned and written-off stock must equal the quantity removed",
                );
            }
        }
    }
    if seen != expected_products {
        return super::refuse(
            "correction deltas must cover every product in the original and replacement exactly once",
        );
    }
    Ok(())
}

pub fn pending(conn: &Connection, id: i64) -> Result<PendingCorrection> {
    let mut item = conn
        .query_row(
            "SELECT id, original_order_id, replacement_order_id,
                    issue_receipt_number, reason, shift_id, created_by, created_at
               FROM pending_order_corrections WHERE id = ?1",
            [id],
            |row| {
                Ok(PendingCorrection {
                    id: row.get(0)?,
                    original_order_id: row.get(1)?,
                    replacement_order_id: row.get(2)?,
                    issue_receipt_number: row.get(3)?,
                    reason: row.get(4)?,
                    shift_id: row.get(5)?,
                    created_by: row.get(6)?,
                    created_at: row.get(7)?,
                    lines: Vec::new(),
                })
            },
        )
        .map_err(|err| missing(err, "pending correction"))?;
    item.lines = read_deltas(
        conn,
        "pending_order_correction_lines",
        "pending_id",
        item.id,
    )?;
    Ok(item)
}

/// Every correction print that must be completed or abandoned (D10).
pub fn pending_corrections(conn: &Connection) -> Result<Vec<PendingCorrection>> {
    let mut stmt =
        conn.prepare("SELECT id FROM pending_order_corrections ORDER BY created_at, id")?;
    let ids = stmt.query_map([], |row| row.get::<_, i64>(0))?;
    let ids = ids.collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter().map(|id| pending(conn, id)).collect()
}

pub fn correction(conn: &Connection, id: i64) -> Result<Correction> {
    let mut item = conn
        .query_row(
            "SELECT id, correction_type, original_order_id, replacement_order_id,
                    issue_receipt_number, reason, shift_id, created_by, created_at
               FROM order_corrections WHERE id = ?1",
            [id],
            |row| {
                let kind: String = row.get(1)?;
                Ok(Correction {
                    id: row.get(0)?,
                    kind: parse_correction_kind(&kind)?,
                    original_order_id: row.get(2)?,
                    replacement_order_id: row.get(3)?,
                    issue_receipt_number: row.get(4)?,
                    reason: row.get(5)?,
                    shift_id: row.get(6)?,
                    created_by: row.get(7)?,
                    created_at: row.get(8)?,
                    lines: Vec::new(),
                })
            },
        )
        .map_err(|err| missing(err, "correction"))?;
    item.lines = read_deltas(conn, "order_correction_lines", "correction_id", item.id)?;
    Ok(item)
}

fn read_deltas(
    conn: &Connection,
    table: &str,
    owner_column: &str,
    owner_id: i64,
) -> Result<Vec<CorrectionDelta>> {
    let sql = format!(
        "SELECT product_id, before_milli, after_milli, delta_milli,
                returned_milli, written_off_milli, note
           FROM {table} WHERE {owner_column} = ?1 ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([owner_id], |row| {
        Ok(CorrectionDelta {
            product_id: row.get(0)?,
            before: Milli::from_thousandths(row.get(1)?),
            after: Milli::from_thousandths(row.get(2)?),
            delta: Milli::from_thousandths(row.get(3)?),
            returned: Milli::from_thousandths(row.get(4)?),
            written_off: Milli::from_thousandths(row.get(5)?),
            note: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Delete a completed or abandoned frozen intent, lines first as required by
/// the schema. The append-only applied correction remains untouched.
pub fn clear_pending_correction(conn: &Connection, pending_id: i64) -> Result<()> {
    let removable: Option<bool> = conn
        .query_row(
            "SELECT CASE
                  WHEN EXISTS (
                       SELECT 1 FROM order_corrections c
                        JOIN orders original ON original.id = c.original_order_id
                       WHERE c.original_order_id = pending.original_order_id
                         AND c.replacement_order_id = pending.replacement_order_id
                         AND original.status = 'REPLACED')
                    THEN 1
                  WHEN EXISTS (
                       SELECT 1 FROM orders replacement
                       WHERE replacement.id = pending.replacement_order_id
                         AND replacement.status = 'ABANDONED'
                         AND replacement.replaces_order_id IS NULL
                         AND NOT EXISTS (
                             SELECT 1 FROM receipts r
                              WHERE r.order_id = replacement.id AND r.status <> 'VOID'))
                    THEN 1 ELSE 0 END
           FROM pending_order_corrections pending WHERE pending.id = ?1",
            [pending_id],
            |row| row.get(0),
        )
        .optional()?;
    match removable {
        None => {
            return Err(RepoError::Missing {
                what: "pending correction",
            })
        }
        Some(false) => {
            return super::refuse(
                "a frozen correction stays until it is fully applied or safely abandoned",
            )
        }
        Some(true) => {}
    }
    guarded!(conn.execute(
        "DELETE FROM pending_order_correction_lines WHERE pending_id = ?1",
        [pending_id],
    ))?;
    guarded!(conn.execute(
        "DELETE FROM pending_order_corrections WHERE id = ?1",
        [pending_id]
    ))?;
    Ok(())
}

fn require_text<'a>(text: &'a str, field: &str) -> Result<&'a str> {
    let text = text.trim();
    if text.is_empty() {
        return super::refuse(format!("{field} cannot be blank"));
    }
    Ok(text)
}

fn missing(err: rusqlite::Error, what: &'static str) -> RepoError {
    match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what },
        other => RepoError::Sqlite(other),
    }
}

fn invalid_text(index: usize, domain: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown {domain} '{value}'"),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{fixture, receipts};

    const LATER: i64 = fixture::NOW + 1_000;

    struct Trading {
        bar: fixture::Bar,
        shift: i64,
        tab: i64,
    }

    fn trading() -> Trading {
        let bar = fixture::bar();
        bar.conn
            .execute(
                "INSERT INTO shifts
                     (code, business_date, opened_at, opened_by,
                      opening_float_minor, expected_end_at)
                 VALUES ('SHIFT-000001', '2025-08-01', ?1, ?2, 0, ?3)",
                rusqlite::params![fixture::NOW, bar.cashier, fixture::NOW + 43_200_000],
            )
            .unwrap();
        let shift = bar.conn.last_insert_rowid();
        bar.conn
            .execute(
                "INSERT INTO tabs
                     (code, opened_shift_id, waiter_id, reference_mode, table_no,
                      display_label, opened_at, opened_by)
                 VALUES ('TAB-000001', ?1, ?2, 'TABLE', '7', 'Table 7', ?3, ?4)",
                rusqlite::params![shift, bar.sara, fixture::NOW, bar.cashier],
            )
            .unwrap();
        let tab = bar.conn.last_insert_rowid();
        Trading { bar, shift, tab }
    }

    fn draft(t: &Trading) -> Order {
        create(
            &t.bar.conn,
            NewDraft {
                tab_id: t.tab,
                shift_id: t.shift,
                cashier_id: t.bar.cashier,
                created_at: fixture::NOW,
            },
        )
        .unwrap()
    }

    fn issued(t: &Trading, sale_item_id: i64) -> (Order, receipts::Receipt) {
        let order = draft(t);
        add_line(&t.bar.conn, order.id, sale_item_id, Milli::ONE).unwrap();
        let receipt = receipts::create_issue(
            &t.bar.conn,
            order.id,
            receipts::Destination::Bar,
            fixture::NOW,
        )
        .unwrap();
        mark_printing(&t.bar.conn, order.id).unwrap();
        receipts::freeze_rendered_text(&t.bar.conn, receipt.id, "BR test bytes").unwrap();
        receipts::record_first_issue_attempt(
            &t.bar.conn,
            receipt.id,
            receipts::FinalOutcome::Success,
            "",
            t.shift,
            t.bar.cashier,
            LATER,
        )
        .unwrap();
        receipts::mark_printed(&t.bar.conn, receipt.id, LATER).unwrap();
        mark_issued(&t.bar.conn, order.id, LATER).unwrap();
        (
            find(&t.bar.conn, order.id).unwrap(),
            receipts::find(&t.bar.conn, receipt.id).unwrap(),
        )
    }

    fn frozen_beer_correction(t: &Trading) -> (Order, Order, PendingCorrection) {
        let (original, receipt) = issued(t, t.bar.beer_bottle);
        let replacement =
            create_replacement(&t.bar.conn, original.id, t.bar.cashier, LATER).unwrap();
        let original_line = lines(&t.bar.conn, original.id).unwrap().remove(0);
        add_line_from_original(
            &t.bar.conn,
            replacement.id,
            original_line.id,
            Milli::from_thousandths(500),
        )
        .unwrap();
        let delta = CorrectionDelta::new(
            t.bar.beer,
            Milli::ONE,
            Milli::from_thousandths(500),
            Milli::ZERO,
            "not returned",
        )
        .unwrap();
        let frozen = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "wrong quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            &[delta],
        )
        .unwrap();
        (original, replacement, frozen)
    }

    fn issue_existing(t: &Trading, order_id: i64, created_at: i64) -> receipts::Receipt {
        let receipt = receipts::create_issue(
            &t.bar.conn,
            order_id,
            receipts::Destination::Bar,
            created_at,
        )
        .unwrap();
        mark_printing(&t.bar.conn, order_id).unwrap();
        receipts::freeze_rendered_text(&t.bar.conn, receipt.id, "BR test bytes").unwrap();
        receipts::record_first_issue_attempt(
            &t.bar.conn,
            receipt.id,
            receipts::FinalOutcome::Success,
            "",
            t.shift,
            t.bar.cashier,
            created_at + 1,
        )
        .unwrap();
        receipts::mark_printed(&t.bar.conn, receipt.id, created_at + 1).unwrap();
        mark_issued(&t.bar.conn, order_id, created_at + 1).unwrap();
        receipts::find(&t.bar.conn, receipt.id).unwrap()
    }

    fn untracked_sale_item(t: &Trading) -> i64 {
        let product = fixture::product(&t.bar.conn, "P-SERVICE", "Service", "UNIT", 1_000);
        t.bar
            .conn
            .execute(
                "UPDATE products SET tracks_inventory = 0 WHERE id = ?1",
                [product],
            )
            .unwrap();
        let item = fixture::sale_item(&t.bar.conn, "S-SERVICE", "Cover service", "Services", 2_000);
        fixture::recipe(&t.bar.conn, item, &[(product, 1_000)]);
        item
    }

    #[test]
    fn a_draft_stamps_the_tabs_current_waiter_and_lines_snapshot_catalogue_facts() {
        let t = trading();
        let order = draft(&t);
        assert_eq!(order.status, Status::Draft);
        assert_eq!(order.waiter_id, t.bar.sara);

        let line = add_line(
            &t.bar.conn,
            order.id,
            t.bar.beer_bottle,
            Milli::from_units(2),
        )
        .unwrap();
        assert_eq!(line.sale_item_name, "Beer");
        assert_eq!(line.unit_price, Money::from_minor(5_000));
        assert_eq!(line.line_total, Money::from_minor(10_000));

        t.bar
            .conn
            .execute(
                "UPDATE sale_items SET name = 'Lager' WHERE id = ?1",
                [t.bar.beer_bottle],
            )
            .unwrap();
        super::super::catalogue::reprice(
            &t.bar.conn,
            t.bar.beer_bottle,
            Money::from_minor(6_000),
            LATER,
            t.bar.owner,
        )
        .unwrap();
        let stored = lines(&t.bar.conn, order.id).unwrap();
        assert_eq!(stored[0].sale_item_name, "Beer");
        assert_eq!(stored[0].unit_price, Money::from_minor(5_000));
        assert_eq!(stored[0].line_total, Money::from_minor(10_000));
    }

    #[test]
    fn replacement_lines_copy_the_original_snapshot_not_todays_catalogue() {
        let t = trading();
        let (original, _) = issued(&t, t.bar.beer_bottle);
        let original_line = lines(&t.bar.conn, original.id).unwrap().remove(0);
        super::super::catalogue::reprice(
            &t.bar.conn,
            t.bar.beer_bottle,
            Money::from_minor(9_000),
            LATER + 1,
            t.bar.owner,
        )
        .unwrap();

        let replacement =
            create_replacement(&t.bar.conn, original.id, t.bar.cashier, LATER).unwrap();
        let bypass = add_line(
            &t.bar.conn,
            replacement.id,
            t.bar.beer_bottle,
            Milli::from_units(2),
        )
        .unwrap_err();
        assert!(bypass.to_string().contains("original recipe and price"));
        let copied = add_line_from_original(
            &t.bar.conn,
            replacement.id,
            original_line.id,
            Milli::from_units(2),
        )
        .unwrap();
        assert_eq!(copied.recipe_id, original_line.recipe_id);
        assert_eq!(copied.unit_price, Money::from_minor(5_000));
        assert_eq!(copied.line_total, Money::from_minor(10_000));
    }

    #[test]
    fn a_confirmed_non_print_keeps_the_number_and_returns_only_to_draft() {
        let t = trading();
        let order = draft(&t);
        add_line(&t.bar.conn, order.id, t.bar.beer_bottle, Milli::ONE).unwrap();
        let receipt = receipts::create_issue(
            &t.bar.conn,
            order.id,
            receipts::Destination::Bar,
            fixture::NOW,
        )
        .unwrap();
        mark_printing(&t.bar.conn, order.id).unwrap();
        assert_eq!(stranded_prints(&t.bar.conn).unwrap()[0].id, order.id);
        assert!(return_to_draft(&t.bar.conn, order.id).is_err());

        receipts::mark_void(&t.bar.conn, receipt.id).unwrap();
        return_to_draft(&t.bar.conn, order.id).unwrap();
        assert_eq!(find(&t.bar.conn, order.id).unwrap().status, Status::Draft);
        assert_eq!(
            receipts::find(&t.bar.conn, receipt.id).unwrap().status,
            receipts::Status::Void
        );
    }

    #[test]
    fn printed_or_failed_authorisation_can_never_be_resurrected_as_a_draft() {
        let t = trading();
        for (index, outcome) in [
            receipts::FinalOutcome::Success,
            receipts::FinalOutcome::Failed,
        ]
        .into_iter()
        .enumerate()
        {
            let order = draft(&t);
            add_line(&t.bar.conn, order.id, t.bar.beer_bottle, Milli::ONE).unwrap();
            let receipt = receipts::create_issue(
                &t.bar.conn,
                order.id,
                receipts::Destination::Bar,
                fixture::NOW + index as i64,
            )
            .unwrap();
            mark_printing(&t.bar.conn, order.id).unwrap();
            receipts::freeze_rendered_text(&t.bar.conn, receipt.id, "authorising bytes").unwrap();
            receipts::record_first_issue_attempt(
                &t.bar.conn,
                receipt.id,
                outcome,
                if outcome == receipts::FinalOutcome::Failed {
                    "handwritten chit authorised"
                } else {
                    ""
                },
                t.shift,
                t.bar.cashier,
                LATER + index as i64,
            )
            .unwrap();
            match outcome {
                receipts::FinalOutcome::Success => {
                    receipts::mark_printed(&t.bar.conn, receipt.id, LATER + index as i64).unwrap()
                }
                receipts::FinalOutcome::Failed => {
                    receipts::mark_failed(&t.bar.conn, receipt.id).unwrap()
                }
            }
            let error = return_to_draft(&t.bar.conn, order.id).unwrap_err();
            assert!(error.to_string().contains("must be void"));
            assert_eq!(
                find(&t.bar.conn, order.id).unwrap().status,
                Status::Printing
            );
        }
    }

    #[test]
    fn printing_and_issue_require_exact_receipt_destination_coverage() {
        let t = trading();
        t.bar
            .conn
            .execute(
                "UPDATE products SET destination = 'KITCHEN' WHERE id = ?1",
                [t.bar.tonic],
            )
            .unwrap();
        let order = draft(&t);
        add_line(&t.bar.conn, order.id, t.bar.gin_tonic, Milli::ONE).unwrap();
        let bar_receipt = receipts::create_issue(
            &t.bar.conn,
            order.id,
            receipts::Destination::Bar,
            fixture::NOW,
        )
        .unwrap();
        assert!(mark_printing(&t.bar.conn, order.id).is_err());
        let kitchen_receipt = receipts::create_issue(
            &t.bar.conn,
            order.id,
            receipts::Destination::Kitchen,
            fixture::NOW,
        )
        .unwrap();
        mark_printing(&t.bar.conn, order.id).unwrap();

        for receipt in [&bar_receipt, &kitchen_receipt] {
            receipts::freeze_rendered_text(&t.bar.conn, receipt.id, "destination bytes").unwrap();
            receipts::record_first_issue_attempt(
                &t.bar.conn,
                receipt.id,
                receipts::FinalOutcome::Success,
                "",
                t.shift,
                t.bar.cashier,
                LATER,
            )
            .unwrap();
        }
        receipts::mark_printed(&t.bar.conn, bar_receipt.id, LATER).unwrap();
        assert!(mark_issued(&t.bar.conn, order.id, LATER).is_err());
        receipts::mark_printed(&t.bar.conn, kitchen_receipt.id, LATER).unwrap();
        mark_issued(&t.bar.conn, order.id, LATER).unwrap();
    }

    #[test]
    fn terminal_order_states_cannot_be_reopened() {
        let t = trading();
        let order = draft(&t);
        abandon(&t.bar.conn, order.id).unwrap();
        let err = carry_draft(&t.bar.conn, order.id, t.shift).unwrap_err();
        assert!(err.to_string().contains("abandoned"), "got: {err}");
    }

    #[test]
    fn correction_disposition_is_computed_and_invalid_returns_are_refused() {
        let reduced = CorrectionDelta::new(
            7,
            Milli::from_units(5),
            Milli::from_units(2),
            Milli::ONE,
            "one sealed bottle",
        )
        .unwrap();
        assert_eq!(reduced.delta, Milli::from_units(-3));
        assert_eq!(reduced.written_off, Milli::from_units(2));

        assert!(CorrectionDelta::new(7, Milli::ONE, Milli::from_units(2), Milli::ONE, "").is_err());
        assert!(
            CorrectionDelta::new(7, Milli::ONE, Milli::ZERO, Milli::from_units(2), "").is_err()
        );
    }

    #[test]
    fn frozen_correction_intent_copies_exactly_to_append_only_history() {
        let t = trading();
        let (original, receipt) = issued(&t, t.bar.beer_bottle);
        let replacement =
            create_replacement(&t.bar.conn, original.id, t.bar.cashier, LATER).unwrap();
        let original_line = lines(&t.bar.conn, original.id).unwrap().remove(0);
        add_line_from_original(
            &t.bar.conn,
            replacement.id,
            original_line.id,
            Milli::from_thousandths(500),
        )
        .unwrap();
        let delta = CorrectionDelta::new(
            t.bar.beer,
            Milli::ONE,
            Milli::from_thousandths(500),
            Milli::from_thousandths(250),
            "half returned",
        )
        .unwrap();
        let pending = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "wrong quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            std::slice::from_ref(&delta),
        )
        .unwrap();
        assert_eq!(pending.lines, [delta.clone()]);
        assert_eq!(pending_corrections(&t.bar.conn).unwrap().len(), 1);

        let replacement_receipt = receipts::create_issue(
            &t.bar.conn,
            replacement.id,
            receipts::Destination::Bar,
            LATER,
        )
        .unwrap();
        mark_printing(&t.bar.conn, replacement.id).unwrap();
        receipts::freeze_rendered_text(&t.bar.conn, replacement_receipt.id, "replacement BR bytes")
            .unwrap();
        receipts::record_first_issue_attempt(
            &t.bar.conn,
            replacement_receipt.id,
            receipts::FinalOutcome::Success,
            "",
            t.shift,
            t.bar.cashier,
            LATER + 1,
        )
        .unwrap();
        receipts::mark_printed(&t.bar.conn, replacement_receipt.id, LATER + 1).unwrap();
        mark_issued(&t.bar.conn, replacement.id, LATER + 1).unwrap();

        let applied = apply_pending_correction(&t.bar.conn, pending.id).unwrap();
        assert_eq!(applied.kind, CorrectionKind::Correction);
        assert_eq!(applied.lines, [delta]);
        mark_replaced(&t.bar.conn, original.id).unwrap();
        clear_pending_correction(&t.bar.conn, pending.id).unwrap();
        assert!(pending_corrections(&t.bar.conn).unwrap().is_empty());
        assert_eq!(
            find(&t.bar.conn, original.id).unwrap().status,
            Status::Replaced
        );
    }

    #[test]
    fn pending_correction_cannot_be_cleared_before_completion_or_abandonment() {
        for state in [Status::Draft, Status::Printing, Status::Issued] {
            let t = trading();
            let (_, replacement, frozen) = frozen_beer_correction(&t);

            if state != Status::Draft {
                let receipt = receipts::create_issue(
                    &t.bar.conn,
                    replacement.id,
                    receipts::Destination::Bar,
                    LATER,
                )
                .unwrap();
                mark_printing(&t.bar.conn, replacement.id).unwrap();
                if state == Status::Issued {
                    receipts::freeze_rendered_text(&t.bar.conn, receipt.id, "replacement BR bytes")
                        .unwrap();
                    receipts::record_first_issue_attempt(
                        &t.bar.conn,
                        receipt.id,
                        receipts::FinalOutcome::Success,
                        "",
                        t.shift,
                        t.bar.cashier,
                        LATER + 1,
                    )
                    .unwrap();
                    receipts::mark_printed(&t.bar.conn, receipt.id, LATER + 1).unwrap();
                    mark_issued(&t.bar.conn, replacement.id, LATER + 1).unwrap();
                }
            }

            let error = clear_pending_correction(&t.bar.conn, frozen.id).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("fully applied or safely abandoned"),
                "state {state:?}: got {error}"
            );
            assert_eq!(pending(&t.bar.conn, frozen.id).unwrap(), frozen);
        }
    }

    #[test]
    fn correction_with_only_an_untracked_bill_change_needs_no_stock_deltas() {
        let t = trading();
        let service_item = untracked_sale_item(&t);
        let original = draft(&t);
        add_line(&t.bar.conn, original.id, t.bar.beer_bottle, Milli::ONE).unwrap();
        add_line(&t.bar.conn, original.id, service_item, Milli::ONE).unwrap();
        let original_receipt = issue_existing(&t, original.id, fixture::NOW);

        let replacement =
            create_replacement(&t.bar.conn, original.id, t.bar.cashier, LATER).unwrap();
        for original_line in lines(&t.bar.conn, original.id).unwrap() {
            let quantity = if original_line.sale_item_id == service_item {
                Milli::from_units(2)
            } else {
                original_line.quantity
            };
            add_line_from_original(&t.bar.conn, replacement.id, original_line.id, quantity)
                .unwrap();
        }

        let frozen = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &original_receipt.receipt_number,
                reason: "service quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            &[],
        )
        .unwrap();
        assert!(frozen.lines.is_empty());

        issue_existing(&t, replacement.id, LATER + 1);
        let applied = apply_pending_correction(&t.bar.conn, frozen.id).unwrap();
        assert!(applied.lines.is_empty());
        mark_replaced(&t.bar.conn, original.id).unwrap();
        clear_pending_correction(&t.bar.conn, frozen.id).unwrap();
        assert_eq!(
            find(&t.bar.conn, original.id).unwrap().status,
            Status::Replaced
        );
    }

    #[test]
    fn correction_cannot_omit_a_changed_tracked_product_delta() {
        let t = trading();
        let (original, receipt) = issued(&t, t.bar.beer_bottle);
        let replacement =
            create_replacement(&t.bar.conn, original.id, t.bar.cashier, LATER).unwrap();
        let original_line = lines(&t.bar.conn, original.id).unwrap().remove(0);
        add_line_from_original(
            &t.bar.conn,
            replacement.id,
            original_line.id,
            Milli::from_thousandths(500),
        )
        .unwrap();

        let error = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "wrong quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            &[],
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("cover every product"),
            "got: {error}"
        );
    }

    #[test]
    fn correction_freeze_rejects_unrelated_replacements_and_forged_deltas() {
        let t = trading();
        let (first, first_receipt) = issued(&t, t.bar.beer_bottle);
        let (second, second_receipt) = issued(&t, t.bar.beer_bottle);
        let replacement = create_replacement(&t.bar.conn, first.id, t.bar.cashier, LATER).unwrap();
        let first_line = lines(&t.bar.conn, first.id).unwrap().remove(0);
        add_line_from_original(
            &t.bar.conn,
            replacement.id,
            first_line.id,
            Milli::from_thousandths(500),
        )
        .unwrap();
        let delta = CorrectionDelta::new(
            t.bar.beer,
            Milli::ONE,
            Milli::from_thousandths(500),
            Milli::ZERO,
            "not returned",
        )
        .unwrap();

        let unrelated = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: second.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &second_receipt.receipt_number,
                reason: "wrong quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            std::slice::from_ref(&delta),
        )
        .unwrap_err();
        assert!(unrelated.to_string().contains("linked draft replacement"));

        let mut forged = delta.clone();
        forged.before = Milli::from_units(9);
        let forged_error = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: first.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &first_receipt.receipt_number,
                reason: "wrong quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            &[forged],
        )
        .unwrap_err();
        assert!(forged_error.to_string().contains("frozen order recipes"));

        let duplicate_error = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: first.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &first_receipt.receipt_number,
                reason: "wrong quantity",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            &[delta.clone(), delta],
        )
        .unwrap_err();
        assert!(duplicate_error.to_string().contains("exactly one delta"));
    }

    #[test]
    fn a_void_persists_its_typed_receipt_and_physical_disposition() {
        let t = trading();
        let (order, receipt) = issued(&t, t.bar.beer_bottle);
        let delta = CorrectionDelta::new(
            t.bar.beer,
            Milli::ONE,
            Milli::ZERO,
            Milli::from_thousandths(750),
            "sealed return",
        )
        .unwrap();
        let stored = record_void(
            &t.bar.conn,
            NewVoid {
                original_order_id: order.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "duplicate round",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER + 2,
            },
            &[delta],
        )
        .unwrap();
        let mismatched = mark_voided(
            &t.bar.conn,
            order.id,
            "different reason",
            LATER + 2,
            t.bar.cashier,
        )
        .unwrap_err();
        assert!(mismatched
            .to_string()
            .contains("record the void correction"));
        mark_voided(
            &t.bar.conn,
            order.id,
            "duplicate round",
            LATER + 2,
            t.bar.cashier,
        )
        .unwrap();
        assert_eq!(stored.kind, CorrectionKind::Void);
        assert_eq!(stored.replacement_order_id, None);
        assert_eq!(stored.lines[0].written_off, Milli::from_thousandths(250));
        assert_eq!(find(&t.bar.conn, order.id).unwrap().status, Status::Voided);
    }

    #[test]
    fn voiding_an_untracked_bill_needs_no_stock_deltas() {
        let t = trading();
        let service_item = untracked_sale_item(&t);
        let (order, receipt) = issued(&t, service_item);

        let stored = record_void(
            &t.bar.conn,
            NewVoid {
                original_order_id: order.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "service cancelled",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER + 2,
            },
            &[],
        )
        .unwrap();
        assert!(stored.lines.is_empty());

        mark_voided(
            &t.bar.conn,
            order.id,
            "service cancelled",
            LATER + 2,
            t.bar.cashier,
        )
        .unwrap();
        assert_eq!(find(&t.bar.conn, order.id).unwrap().status, Status::Voided);
    }

    #[test]
    fn a_closed_tab_cannot_be_voided_or_restate_its_frozen_bill() {
        let t = trading();
        let (order, receipt) = issued(&t, t.bar.beer_bottle);
        t.bar
            .conn
            .execute(
                "UPDATE tabs SET status = 'CLOSED', closed_shift_id = ?2,
                                 closed_at = ?3, closed_by = ?4
                  WHERE id = ?1",
                rusqlite::params![t.tab, t.shift, LATER, t.bar.cashier],
            )
            .unwrap();
        let delta = CorrectionDelta::new(
            t.bar.beer,
            Milli::ONE,
            Milli::ZERO,
            Milli::ZERO,
            "not returned",
        )
        .unwrap();
        let error = record_void(
            &t.bar.conn,
            NewVoid {
                original_order_id: order.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "too late",
                shift_id: t.shift,
                created_by: t.bar.cashier,
                created_at: LATER,
            },
            &[delta],
        )
        .unwrap_err();
        assert!(error.to_string().contains("open tab and shift"));
        assert_eq!(find(&t.bar.conn, order.id).unwrap().status, Status::Issued);
    }

    #[test]
    fn till_order_operations_require_an_active_cashier() {
        let t = trading();
        let create_as_owner = create(
            &t.bar.conn,
            NewDraft {
                tab_id: t.tab,
                shift_id: t.shift,
                cashier_id: t.bar.owner,
                created_at: fixture::NOW,
            },
        )
        .unwrap_err();
        assert!(create_as_owner.to_string().contains("active cashier"));

        let (original, receipt) = issued(&t, t.bar.beer_bottle);
        let replacement_as_owner =
            create_replacement(&t.bar.conn, original.id, t.bar.owner, LATER).unwrap_err();
        assert!(replacement_as_owner.to_string().contains("active cashier"));
        let replacement =
            create_replacement(&t.bar.conn, original.id, t.bar.cashier, LATER).unwrap();
        let delta = CorrectionDelta::new(
            t.bar.beer,
            Milli::ONE,
            Milli::ZERO,
            Milli::ZERO,
            "not returned",
        )
        .unwrap();

        let freeze_as_owner = freeze_correction(
            &t.bar.conn,
            NewCorrection {
                original_order_id: original.id,
                replacement_order_id: replacement.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "wrong round",
                shift_id: t.shift,
                created_by: t.bar.owner,
                created_at: LATER,
            },
            std::slice::from_ref(&delta),
        )
        .unwrap_err();
        assert!(freeze_as_owner.to_string().contains("active cashier"));

        let void_as_owner = record_void(
            &t.bar.conn,
            NewVoid {
                original_order_id: original.id,
                issue_receipt_number: &receipt.receipt_number,
                reason: "wrong round",
                shift_id: t.shift,
                created_by: t.bar.owner,
                created_at: LATER,
            },
            std::slice::from_ref(&delta),
        )
        .unwrap_err();
        assert!(void_as_owner.to_string().contains("active cashier"));

        let mark_as_owner =
            mark_voided(&t.bar.conn, original.id, "wrong round", LATER, t.bar.owner).unwrap_err();
        assert!(mark_as_owner.to_string().contains("active cashier"));

        t.bar
            .conn
            .execute("UPDATE staff SET active = 0 WHERE id = ?1", [t.bar.cashier])
            .unwrap();
        let create_as_inactive = create(
            &t.bar.conn,
            NewDraft {
                tab_id: t.tab,
                shift_id: t.shift,
                cashier_id: t.bar.cashier,
                created_at: LATER,
            },
        )
        .unwrap_err();
        assert!(create_as_inactive.to_string().contains("active cashier"));
    }
}
