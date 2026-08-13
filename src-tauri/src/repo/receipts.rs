//! Numbered issue/customer receipts and their append-only print attempts (§6).
//!
//! Sequence allocation happens here, immediately before the consuming receipt
//! insert, but this module never opens or commits a transaction. Callers must
//! put `create_*` and the surrounding state changes inside the protocol's own
//! transaction. Rendering and device I/O do not belong in a repository: only
//! the once-frozen bytes and the reported attempt outcome are persisted here.
//!
//! Customer fiscal identity, including the optional D25 TIN, is copied from
//! the closed tab rather than inferred from rendered output.

use rusqlite::{Connection, OptionalExtension};

use crate::money::BasisPoints;
use crate::Money;

use super::{guarded, seq, RepoError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Issue,
    Customer,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "ISSUE",
            Self::Customer => "CUSTOMER",
        }
    }
}

fn parse_kind(text: &str) -> rusqlite::Result<Kind> {
    match text {
        "ISSUE" => Ok(Kind::Issue),
        "CUSTOMER" => Ok(Kind::Customer),
        other => Err(invalid_text(1, "receipt type", other)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Bar,
    Kitchen,
}

impl Destination {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bar => "BAR",
            Self::Kitchen => "KITCHEN",
        }
    }
}

fn parse_destination(text: &str) -> rusqlite::Result<Destination> {
    match text {
        "BAR" => Ok(Destination::Bar),
        "KITCHEN" => Ok(Destination::Kitchen),
        other => Err(invalid_text(6, "receipt destination", other)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Printed,
    Failed,
    Void,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Printed => "PRINTED",
            Self::Failed => "FAILED",
            Self::Void => "VOID",
        }
    }
}

fn parse_status(text: &str) -> rusqlite::Result<Status> {
    match text {
        "PENDING" => Ok(Status::Pending),
        "PRINTED" => Ok(Status::Printed),
        "FAILED" => Ok(Status::Failed),
        "VOID" => Ok(Status::Void),
        other => Err(invalid_text(7, "receipt status", other)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub id: i64,
    pub kind: Kind,
    pub sequence_no: i64,
    pub receipt_number: String,
    pub order_id: Option<i64>,
    pub tab_id: Option<i64>,
    pub destination: Option<Destination>,
    pub status: Status,
    pub rendered_text: Option<String>,
    pub waiter_name: String,
    pub cashier_name: Option<String>,
    pub customer_tin: Option<String>,
    pub subtotal: Option<Money>,
    pub service_charge: Option<Money>,
    pub tax: Option<Money>,
    pub total: Option<Money>,
    pub tax_rate: Option<BasisPoints>,
    pub service_rate: Option<BasisPoints>,
    pub tax_inclusive: Option<bool>,
    pub is_comped: bool,
    pub shift_id: i64,
    pub created_at: i64,
    pub printed_at: Option<i64>,
}

const RECEIPT_COLUMNS: &str = "id, receipt_type, sequence_no, receipt_number, order_id, tab_id,
     destination, status, rendered_text, waiter_name, cashier_name, subtotal_minor,
     service_charge_minor, tax_minor, total_minor, tax_rate_bp, service_rate_bp,
     tax_inclusive, is_comped, shift_id, created_at, printed_at, customer_tin";

fn read_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    let kind: String = row.get(1)?;
    let destination: Option<String> = row.get(6)?;
    let status: String = row.get(7)?;
    Ok(Receipt {
        id: row.get(0)?,
        kind: parse_kind(&kind)?,
        sequence_no: row.get(2)?,
        receipt_number: row.get(3)?,
        order_id: row.get(4)?,
        tab_id: row.get(5)?,
        destination: destination.as_deref().map(parse_destination).transpose()?,
        status: parse_status(&status)?,
        rendered_text: row.get(8)?,
        waiter_name: row.get(9)?,
        cashier_name: row.get(10)?,
        customer_tin: row.get(22)?,
        subtotal: optional_money(row, 11)?,
        service_charge: optional_money(row, 12)?,
        tax: optional_money(row, 13)?,
        total: optional_money(row, 14)?,
        tax_rate: optional_rate(row, 15)?,
        service_rate: optional_rate(row, 16)?,
        tax_inclusive: row.get::<_, Option<i64>>(17)?.map(|value| value == 1),
        is_comped: row.get::<_, i64>(18)? == 1,
        shift_id: row.get(19)?,
        created_at: row.get(20)?,
        printed_at: row.get(21)?,
    })
}

fn optional_money(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<Money>> {
    Ok(row.get::<_, Option<i64>>(index)?.map(Money::from_minor))
}

fn optional_rate(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<BasisPoints>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u32::try_from(value).map(BasisPoints).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

pub fn find(conn: &Connection, id: i64) -> Result<Receipt> {
    conn.query_row(
        &format!("SELECT {RECEIPT_COLUMNS} FROM receipts WHERE id = ?1"),
        [id],
        read_receipt,
    )
    .map_err(|err| missing(err, "receipt"))
}

pub fn find_by_number(conn: &Connection, receipt_number: &str) -> Result<Receipt> {
    conn.query_row(
        &format!("SELECT {RECEIPT_COLUMNS} FROM receipts WHERE receipt_number = ?1"),
        [receipt_number],
        read_receipt,
    )
    .map_err(|err| missing(err, "receipt"))
}

pub fn for_order(conn: &Connection, order_id: i64) -> Result<Vec<Receipt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RECEIPT_COLUMNS} FROM receipts WHERE order_id = ?1 ORDER BY sequence_no"
    ))?;
    let rows = stmt.query_map([order_id], read_receipt)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn for_tab(conn: &Connection, tab_id: i64) -> Result<Option<Receipt>> {
    conn.query_row(
        &format!("SELECT {RECEIPT_COLUMNS} FROM receipts WHERE tab_id = ?1"),
        [tab_id],
        read_receipt,
    )
    .optional()
    .map_err(RepoError::Sqlite)
}

/// Receipts still waiting for an explicit print/recovery outcome (D10).
pub fn pending(conn: &Connection) -> Result<Vec<Receipt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RECEIPT_COLUMNS} FROM receipts WHERE status = 'PENDING' ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], read_receipt)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Allocate a BR number and consume it with the issue receipt.
///
/// The caller must own Transaction 1. Availability for the fully aggregated
/// recipe belongs in the pre-check layer and must run before this function,
/// so a D9 refusal never calls this sequence-consuming write.
pub fn create_issue(
    conn: &Connection,
    order_id: i64,
    destination: Destination,
    created_at: i64,
) -> Result<Receipt> {
    let duplicate: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM receipts
              WHERE order_id = ?1 AND destination = ?2 AND status <> 'VOID'
         )",
        rusqlite::params![order_id, destination.as_str()],
        |row| row.get(0),
    )?;
    if duplicate {
        return super::refuse("that order already has a live receipt for this destination");
    }
    let required: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
               FROM order_lines l
               JOIN recipe_lines rl ON rl.recipe_id = l.recipe_id
               JOIN products p ON p.id = rl.product_id
              WHERE l.order_id = ?1 AND p.destination = ?2
         )",
        rusqlite::params![order_id, destination.as_str()],
        |row| row.get(0),
    )?;
    if !required {
        return super::refuse("that order has nothing routed to this receipt destination");
    }
    let snapshot: Option<(String, i64)> = conn
        .query_row(
            "SELECT waiter.full_name, o.shift_id
               FROM orders o
               JOIN staff waiter ON waiter.id = o.waiter_id
               JOIN staff cashier ON cashier.id = o.cashier_id
               JOIN tabs t ON t.id = o.tab_id
               JOIN shifts s ON s.id = o.shift_id
              WHERE o.id = ?1 AND o.status = 'DRAFT'
                AND t.status = 'OPEN' AND s.status = 'OPEN'
                AND cashier.role = 'CASHIER' AND cashier.active = 1
                AND EXISTS (SELECT 1 FROM order_lines l WHERE l.order_id = o.id)",
            [order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (waiter_name, shift_id) = snapshot.ok_or_else(|| {
        RepoError::Refused(
            "an issue receipt needs a non-empty draft on an open tab and shift, rung by an active cashier"
                .into(),
        )
    })?;

    let (sequence_no, receipt_number) = seq::next(conn, seq::Counter::IssueReceipt)?;
    guarded!(conn.execute(
        "INSERT INTO receipts
             (receipt_type, sequence_no, receipt_number, order_id, destination,
              waiter_name, shift_id, created_at)
         VALUES ('ISSUE', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            sequence_no,
            receipt_number,
            order_id,
            destination.as_str(),
            waiter_name,
            shift_id,
            created_at,
        ],
    ))?;
    find(conn, conn.last_insert_rowid())
}

/// Allocate a CR number and copy the frozen bill from `tab_payments`.
///
/// The active cashier supplied here is the person producing the fiscal
/// document; their current name is frozen onto it. Money and rates are copied
/// only from the append-only payment row and are never recalculated.
pub fn create_customer(
    conn: &Connection,
    tab_id: i64,
    cashier_id: i64,
    created_at: i64,
) -> Result<Receipt> {
    require_active_cashier(conn, cashier_id)?;
    require_trading_shift(conn)?;
    let duplicate: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM receipts WHERE tab_id = ?1)",
        [tab_id],
        |row| row.get(0),
    )?;
    if duplicate {
        return super::refuse("that tab already has its customer receipt");
    }
    let snapshot: Option<CustomerSnapshot> = conn
        .query_row(
            "SELECT waiter.full_name,
                    p.subtotal_minor, p.service_charge_minor, p.tax_minor, p.total_minor,
                    p.tax_rate_bp, p.service_rate_bp, p.tax_inclusive, p.is_comped,
                    p.shift_id, p.charge_rates_known, closer.full_name, t.customer_tin
               FROM tab_payments p
               JOIN tabs t ON t.id = p.tab_id
               JOIN staff waiter ON waiter.id = p.waiter_id
               JOIN staff closer ON closer.id = t.closed_by
              WHERE p.tab_id = ?1 AND t.status IN ('CLOSED','RECONCILED')",
            [tab_id],
            |row| {
                Ok(CustomerSnapshot {
                    waiter_name: row.get(0)?,
                    subtotal_minor: row.get(1)?,
                    service_charge_minor: row.get(2)?,
                    tax_minor: row.get(3)?,
                    total_minor: row.get(4)?,
                    tax_rate_bp: row.get(5)?,
                    service_rate_bp: row.get(6)?,
                    tax_inclusive: row.get(7)?,
                    is_comped: row.get(8)?,
                    shift_id: row.get(9)?,
                    charge_rates_known: row.get(10)?,
                    cashier_name: row.get(11)?,
                    customer_tin: row.get(12)?,
                })
            },
        )
        .optional()?;
    let snapshot = snapshot.ok_or_else(|| {
        RepoError::Refused("a customer receipt needs a closed tab with a frozen bill".into())
    })?;
    if snapshot.charge_rates_known == 0
        && (snapshot.service_charge_minor != 0 || snapshot.tax_minor != 0)
    {
        return super::refuse(
            "this imported bill has charges with unknown rates and needs manual review",
        );
    }

    let (sequence_no, receipt_number) = seq::next(conn, seq::Counter::CustomerReceipt)?;
    guarded!(conn.execute(
        "INSERT INTO receipts
             (receipt_type, sequence_no, receipt_number, tab_id,
              waiter_name, cashier_name, subtotal_minor, service_charge_minor,
              tax_minor, total_minor, tax_rate_bp, service_rate_bp,
              tax_inclusive, is_comped, shift_id, created_at, customer_tin)
         VALUES ('CUSTOMER', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                 ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            sequence_no,
            receipt_number,
            tab_id,
            snapshot.waiter_name,
            snapshot.cashier_name,
            snapshot.subtotal_minor,
            snapshot.service_charge_minor,
            snapshot.tax_minor,
            snapshot.total_minor,
            snapshot.tax_rate_bp,
            snapshot.service_rate_bp,
            snapshot.tax_inclusive,
            snapshot.is_comped,
            snapshot.shift_id,
            created_at,
            snapshot.customer_tin,
        ],
    ))?;
    find(conn, conn.last_insert_rowid())
}

struct CustomerSnapshot {
    waiter_name: String,
    subtotal_minor: i64,
    service_charge_minor: i64,
    tax_minor: i64,
    total_minor: i64,
    tax_rate_bp: i64,
    service_rate_bp: i64,
    tax_inclusive: i64,
    is_comped: i64,
    shift_id: i64,
    charge_rates_known: i64,
    cashier_name: String,
    customer_tin: Option<String>,
}

/// Transaction 1b: freeze the exact bytes before any device I/O.
/// Repeating the identical text is idempotent; changing it is refused.
pub fn freeze_rendered_text(conn: &Connection, receipt_id: i64, rendered_text: &str) -> Result<()> {
    if rendered_text.trim().is_empty() {
        return super::refuse("a receipt cannot freeze empty rendered text");
    }
    let changed = guarded!(conn.execute(
        "UPDATE receipts SET rendered_text = ?2
          WHERE id = ?1 AND status = 'PENDING'
            AND (rendered_text IS NULL OR rendered_text = ?2)",
        rusqlite::params![receipt_id, rendered_text],
    ))?;
    if changed == 1 {
        return Ok(());
    }
    let found: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status, rendered_text FROM receipts WHERE id = ?1",
            [receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match found {
        None => Err(RepoError::Missing { what: "receipt" }),
        Some((status, _)) if status != "PENDING" => {
            super::refuse("receipt text can only be frozen while printing is pending")
        }
        Some(_) => super::refuse("receipt text was already frozen to different bytes"),
    }
}

/// Resolve a receipt after a successful attempt. The most recent attempt must
/// say SUCCESS and the rendered text must already be frozen.
pub fn mark_printed(conn: &Connection, receipt_id: i64, printed_at: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE receipts SET status = 'PRINTED', printed_at = ?2
          WHERE id = ?1 AND status IN ('PENDING','FAILED')
            AND rendered_text IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM receipt_prints p
                             WHERE p.receipt_id = receipts.id AND p.outcome = 'UNKNOWN')
            AND (SELECT outcome FROM receipt_prints p
                  WHERE p.receipt_id = receipts.id ORDER BY p.print_no DESC LIMIT 1) = 'SUCCESS'",
        rusqlite::params![receipt_id, printed_at],
    ))?;
    expect_receipt_change(
        conn,
        receipt_id,
        changed,
        "printing requires frozen text and a successful final attempt",
    )
}

/// Persist a failed/handwritten outcome after a FAILED attempt is recorded.
pub fn mark_failed(conn: &Connection, receipt_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE receipts SET status = 'FAILED'
          WHERE id = ?1 AND status = 'PENDING' AND rendered_text IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM receipt_prints p
                             WHERE p.receipt_id = receipts.id AND p.outcome = 'UNKNOWN')
            AND (SELECT outcome FROM receipt_prints p
                  WHERE p.receipt_id = receipts.id ORDER BY p.print_no DESC LIMIT 1) = 'FAILED'",
        [receipt_id],
    ))?;
    expect_receipt_change(
        conn,
        receipt_id,
        changed,
        "failure requires frozen text and a failed final attempt",
    )
}

/// Retain a BR number after a confirmed non-print. Customer fiscal numbers and
/// receipts with attempt history cannot take this recovery path.
pub fn mark_void(conn: &Connection, receipt_id: i64) -> Result<()> {
    let changed = guarded!(conn.execute(
        "UPDATE receipts SET status = 'VOID'
          WHERE id = ?1 AND receipt_type = 'ISSUE' AND status = 'PENDING'
            AND NOT EXISTS (SELECT 1 FROM receipt_prints p WHERE p.receipt_id = receipts.id)",
        [receipt_id],
    ))?;
    expect_receipt_change(
        conn,
        receipt_id,
        changed,
        "only an unattempted pending issue receipt can be voided",
    )
}

fn expect_receipt_change(
    conn: &Connection,
    receipt_id: i64,
    changed: usize,
    message: &str,
) -> Result<()> {
    if changed == 1 {
        return Ok(());
    }
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM receipts WHERE id = ?1",
            [receipt_id],
            |row| row.get(0),
        )
        .optional()?;
    match status {
        None => Err(RepoError::Missing { what: "receipt" }),
        Some(status) => super::refuse(format!(
            "{message}; the receipt is {}",
            status.to_lowercase()
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Unknown,
    Success,
    Failed,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Success => "SUCCESS",
            Self::Failed => "FAILED",
        }
    }
}

fn parse_outcome(text: &str) -> rusqlite::Result<Outcome> {
    match text {
        "UNKNOWN" => Ok(Outcome::Unknown),
        "SUCCESS" => Ok(Outcome::Success),
        "FAILED" => Ok(Outcome::Failed),
        other => Err(invalid_text(3, "print outcome", other)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalOutcome {
    Success,
    Failed,
}

impl FinalOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::Failed => "FAILED",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub id: i64,
    pub receipt_id: i64,
    pub print_no: i64,
    pub outcome: Outcome,
    pub reason: String,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
}

fn read_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let outcome: String = row.get(3)?;
    Ok(Attempt {
        id: row.get(0)?,
        receipt_id: row.get(1)?,
        print_no: row.get(2)?,
        outcome: parse_outcome(&outcome)?,
        reason: row.get(4)?,
        shift_id: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
    })
}

const ATTEMPT_COLUMNS: &str =
    "id, receipt_id, print_no, outcome, reason, shift_id, created_by, created_at";

pub fn attempts(conn: &Connection, receipt_id: i64) -> Result<Vec<Attempt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ATTEMPT_COLUMNS} FROM receipt_prints WHERE receipt_id = ?1 ORDER BY print_no"
    ))?;
    let rows = stmt.query_map([receipt_id], read_attempt)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// UNKNOWN attempts that must be resolved before reprinting or shift close.
pub fn unresolved_attempts(conn: &Connection) -> Result<Vec<Attempt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ATTEMPT_COLUMNS} FROM receipt_prints
          WHERE outcome = 'UNKNOWN' ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], read_attempt)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Record the first ISSUE attempt after device I/O, as §6.3 specifies.
/// A failed handwritten substitution needs an explicit reason.
pub fn record_first_issue_attempt(
    conn: &Connection,
    receipt_id: i64,
    outcome: FinalOutcome,
    reason: &str,
    shift_id: i64,
    created_by: i64,
    created_at: i64,
) -> Result<Attempt> {
    require_active_cashier(conn, created_by)?;
    require_open_shift(conn, shift_id)?;
    let reason = reason.trim();
    if outcome == FinalOutcome::Failed && reason.is_empty() {
        return super::refuse("a failed issue attempt needs a reason");
    }
    let changed = guarded!(conn.execute(
        "INSERT INTO receipt_prints
             (receipt_id, print_no, outcome, reason, shift_id, created_by, created_at)
         SELECT r.id, 1, ?2, ?3, ?4, ?5, ?6
           FROM receipts r
          WHERE r.id = ?1 AND r.receipt_type = 'ISSUE' AND r.status = 'PENDING'
            AND r.rendered_text IS NOT NULL
            AND r.shift_id = ?4
            AND NOT EXISTS (SELECT 1 FROM receipt_prints p WHERE p.receipt_id = r.id)",
        rusqlite::params![
            receipt_id,
            outcome.as_str(),
            reason,
            shift_id,
            created_by,
            created_at,
        ],
    ))?;
    if changed == 0 {
        return super::refuse("only the first pending issue attempt uses this outcome path");
    }
    attempt(conn, conn.last_insert_rowid())
}

/// Record UNKNOWN before first customer printing or any reprint device I/O.
/// ISSUE first-print completion uses [`record_first_issue_attempt`] instead.
pub fn begin_attempt(
    conn: &Connection,
    receipt_id: i64,
    reason: &str,
    shift_id: i64,
    created_by: i64,
    created_at: i64,
) -> Result<Attempt> {
    require_active_cashier(conn, created_by)?;
    require_open_shift(conn, shift_id)?;
    let receipt = find(conn, receipt_id)?;
    if receipt.rendered_text.is_none() {
        return super::refuse("freeze the receipt text before recording a print attempt");
    }
    let previous: i64 = conn.query_row(
        "SELECT COUNT(*) FROM receipt_prints WHERE receipt_id = ?1",
        [receipt_id],
        |row| row.get(0),
    )?;
    let reason = reason.trim();
    if previous == 0 {
        if receipt.kind != Kind::Customer || receipt.status != Status::Pending {
            return super::refuse(
                "only a pending customer receipt records UNKNOWN on its first print",
            );
        }
        if !reason.is_empty() {
            return super::refuse("a first customer print is not a reprint and has no reason");
        }
    } else if reason.is_empty() {
        return super::refuse("a reprint needs a reason");
    }

    let print_no = previous
        .checked_add(1)
        .ok_or_else(|| RepoError::Refused("that receipt has too many print attempts".into()))?;
    guarded!(conn.execute(
        "INSERT INTO receipt_prints
             (receipt_id, print_no, outcome, reason, shift_id, created_by, created_at)
         VALUES (?1, ?2, 'UNKNOWN', ?3, ?4, ?5, ?6)",
        rusqlite::params![receipt_id, print_no, reason, shift_id, created_by, created_at],
    ))?;
    attempt(conn, conn.last_insert_rowid())
}

pub fn resolve_attempt(
    conn: &Connection,
    attempt_id: i64,
    outcome: FinalOutcome,
) -> Result<Attempt> {
    let changed = guarded!(conn.execute(
        "UPDATE receipt_prints SET outcome = ?2 WHERE id = ?1 AND outcome = 'UNKNOWN'",
        rusqlite::params![attempt_id, outcome.as_str()],
    ))?;
    if changed == 0 {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_prints WHERE id = ?1)",
            [attempt_id],
            |row| row.get(0),
        )?;
        return if exists {
            super::refuse("a print attempt can be answered only once")
        } else {
            Err(RepoError::Missing {
                what: "print attempt",
            })
        };
    }
    attempt(conn, attempt_id)
}

fn attempt(conn: &Connection, id: i64) -> Result<Attempt> {
    conn.query_row(
        &format!("SELECT {ATTEMPT_COLUMNS} FROM receipt_prints WHERE id = ?1"),
        [id],
        read_attempt,
    )
    .map_err(|err| missing(err, "print attempt"))
}

fn require_active_cashier(conn: &Connection, person_id: i64) -> Result<String> {
    let actor: Option<(String, String, bool)> = conn
        .query_row(
            "SELECT full_name, role, active FROM staff WHERE id = ?1",
            [person_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? == 1)),
        )
        .optional()?;
    match actor {
        Some((name, role, true)) if role == "CASHIER" => Ok(name),
        Some(_) => super::refuse("only an active cashier may operate the till"),
        None => Err(RepoError::Missing { what: "cashier" }),
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
        Some(_) => super::refuse("print attempts belong to the shift that is currently open"),
        None => Err(RepoError::Missing { what: "shift" }),
    }
}

fn require_trading_shift(conn: &Connection) -> Result<()> {
    let open: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM shifts WHERE status = 'OPEN')",
        [],
        |row| row.get(0),
    )?;
    if open {
        Ok(())
    } else {
        super::refuse("a customer receipt can only be created while a shift is open")
    }
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
    use crate::repo::{fixture, orders};
    use crate::Milli;

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
                      display_label, customer_tin, opened_at, opened_by)
                 VALUES ('TAB-000001', ?1, ?2, 'TABLE', '8', 'Table 8',
                         'TIN-123', ?3, ?4)",
                rusqlite::params![shift, bar.sara, fixture::NOW, bar.cashier],
            )
            .unwrap();
        let tab = bar.conn.last_insert_rowid();
        Trading { bar, shift, tab }
    }

    fn draft_with_line(t: &Trading) -> orders::Order {
        let order = orders::create(
            &t.bar.conn,
            orders::NewDraft {
                tab_id: t.tab,
                shift_id: t.shift,
                cashier_id: t.bar.cashier,
                created_at: fixture::NOW,
            },
        )
        .unwrap();
        orders::add_line(&t.bar.conn, order.id, t.bar.beer_bottle, Milli::ONE).unwrap();
        order
    }

    fn close_with_bill(t: &Trading) {
        t.bar
            .conn
            .execute(
                "UPDATE tabs SET status = 'CLOSED', closed_shift_id = ?2,
                                 closed_at = ?3, closed_by = ?4
                  WHERE id = ?1",
                rusqlite::params![t.tab, t.shift, LATER, t.bar.cashier],
            )
            .unwrap();
        t.bar
            .conn
            .execute(
                "INSERT INTO tab_payments
                     (tab_id, waiter_id, subtotal_minor, service_charge_minor,
                      tax_minor, total_minor, liability_minor, tax_rate_bp,
                      service_rate_bp, tax_inclusive, shift_id, created_by, created_at)
                 VALUES (?1, ?2, 10000, 1000, 1500, 12500, 12500,
                         1500, 1000, 0, ?3, ?4, ?5)",
                rusqlite::params![t.tab, t.bar.sara, t.shift, t.bar.cashier, LATER],
            )
            .unwrap();
    }

    fn printed_issue(t: &Trading) -> (orders::Order, Receipt) {
        let order = draft_with_line(t);
        let receipt = create_issue(&t.bar.conn, order.id, Destination::Bar, fixture::NOW).unwrap();
        orders::mark_printing(&t.bar.conn, order.id).unwrap();
        freeze_rendered_text(&t.bar.conn, receipt.id, "BR immutable bytes\n").unwrap();
        record_first_issue_attempt(
            &t.bar.conn,
            receipt.id,
            FinalOutcome::Success,
            "",
            t.shift,
            t.bar.cashier,
            LATER,
        )
        .unwrap();
        mark_printed(&t.bar.conn, receipt.id, LATER).unwrap();
        orders::mark_issued(&t.bar.conn, order.id, LATER).unwrap();
        (
            orders::find(&t.bar.conn, order.id).unwrap(),
            find(&t.bar.conn, receipt.id).unwrap(),
        )
    }

    #[test]
    fn issue_identity_and_sequence_are_consumed_in_the_callers_transaction() {
        let mut t = trading();
        let order = draft_with_line(&t);
        {
            let tx = t.bar.conn.transaction().unwrap();
            let receipt = create_issue(&tx, order.id, Destination::Bar, fixture::NOW).unwrap();
            assert_eq!(
                (receipt.sequence_no, receipt.receipt_number.as_str()),
                (1, "BR-000001")
            );
            tx.rollback().unwrap();
        }
        let next_value: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT next_value FROM sequences WHERE name = 'ISSUE_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let count: i64 = t
            .bar
            .conn
            .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            (next_value, count),
            (1, 0),
            "number and receipt roll back together"
        );

        let receipt = create_issue(&t.bar.conn, order.id, Destination::Bar, fixture::NOW).unwrap();
        assert_eq!(receipt.receipt_number, "BR-000001");
        assert!(create_issue(&t.bar.conn, order.id, Destination::Bar, fixture::NOW).is_err());
        mark_void(&t.bar.conn, receipt.id).unwrap();
        let replacement_number =
            create_issue(&t.bar.conn, order.id, Destination::Bar, LATER).unwrap();
        assert_eq!(replacement_number.receipt_number, "BR-000002");
        assert_eq!(
            find_by_number(&t.bar.conn, "BR-000001").unwrap().status,
            Status::Void
        );
    }

    #[test]
    fn customer_receipt_copies_the_frozen_bill_and_has_its_own_identity() {
        let t = trading();
        close_with_bill(&t);
        let printing_cashier = fixture::staff(&t.bar.conn, "CSH-2", "Marta", "CASHIER");
        let receipt = create_customer(&t.bar.conn, t.tab, printing_cashier, LATER).unwrap();
        assert_eq!(receipt.kind, Kind::Customer);
        assert_eq!(receipt.receipt_number, "CR-000001");
        assert_eq!(receipt.waiter_name, "Sara");
        // Abel closed and froze the bill. Marta happens to print it later;
        // that must not rewrite who actually closed the tab.
        assert_eq!(receipt.cashier_name.as_deref(), Some("Abel"));
        assert_eq!(receipt.customer_tin.as_deref(), Some("TIN-123"));
        assert_eq!(receipt.subtotal, Some(Money::from_minor(10_000)));
        assert_eq!(receipt.service_charge, Some(Money::from_minor(1_000)));
        assert_eq!(receipt.tax, Some(Money::from_minor(1_500)));
        assert_eq!(receipt.total, Some(Money::from_minor(12_500)));
        assert_eq!(receipt.tax_rate, Some(BasisPoints(1_500)));
        assert_eq!(receipt.service_rate, Some(BasisPoints(1_000)));
        assert_eq!(receipt.tax_inclusive, Some(false));
        assert!(create_customer(&t.bar.conn, t.tab, t.bar.cashier, LATER + 1).is_err());
        assert_eq!(for_tab(&t.bar.conn, t.tab).unwrap().unwrap().id, receipt.id);
        let tin_change = t.bar.conn.execute(
            "UPDATE receipts SET customer_tin = 'TIN-CHANGED' WHERE id = ?1",
            [receipt.id],
        );
        assert!(tin_change.is_err(), "the fiscal identity is frozen");
        let next_customer: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT next_value FROM sequences WHERE name = 'CUSTOMER_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            next_customer, 2,
            "a known duplicate refusal burns no CR number"
        );
    }

    #[test]
    fn rendered_text_and_receipt_identity_are_immutable() {
        let t = trading();
        let (_, receipt) = printed_issue(&t);
        assert_eq!(
            receipt.rendered_text.as_deref(),
            Some("BR immutable bytes\n")
        );
        assert!(freeze_rendered_text(&t.bar.conn, receipt.id, "changed bytes").is_err());
        let identity_change = t.bar.conn.execute(
            "UPDATE receipts SET waiter_name = 'Someone else' WHERE id = ?1",
            [receipt.id],
        );
        assert!(identity_change.is_err());
        assert_eq!(find(&t.bar.conn, receipt.id).unwrap().waiter_name, "Sara");
    }

    #[test]
    fn customer_attempt_is_unknown_before_io_and_can_be_answered_once() {
        let t = trading();
        close_with_bill(&t);
        let receipt = create_customer(&t.bar.conn, t.tab, t.bar.cashier, LATER).unwrap();
        freeze_rendered_text(&t.bar.conn, receipt.id, "CR immutable bytes").unwrap();
        let attempt =
            begin_attempt(&t.bar.conn, receipt.id, "", t.shift, t.bar.cashier, LATER).unwrap();
        assert_eq!((attempt.print_no, attempt.outcome), (1, Outcome::Unknown));
        assert_eq!(unresolved_attempts(&t.bar.conn).unwrap(), [attempt.clone()]);
        assert!(begin_attempt(
            &t.bar.conn,
            receipt.id,
            "duplicate",
            t.shift,
            t.bar.cashier,
            LATER
        )
        .is_err());

        let resolved = resolve_attempt(&t.bar.conn, attempt.id, FinalOutcome::Success).unwrap();
        assert_eq!(resolved.outcome, Outcome::Success);
        assert!(resolve_attempt(&t.bar.conn, attempt.id, FinalOutcome::Failed).is_err());
        mark_printed(&t.bar.conn, receipt.id, LATER + 1).unwrap();
        assert_eq!(
            find(&t.bar.conn, receipt.id).unwrap().status,
            Status::Printed
        );
    }

    #[test]
    fn failed_first_customer_print_retries_the_same_cr_number() {
        let t = trading();
        close_with_bill(&t);
        let receipt = create_customer(&t.bar.conn, t.tab, t.bar.cashier, LATER).unwrap();
        freeze_rendered_text(&t.bar.conn, receipt.id, "CR retry bytes").unwrap();

        let first =
            begin_attempt(&t.bar.conn, receipt.id, "", t.shift, t.bar.cashier, LATER).unwrap();
        resolve_attempt(&t.bar.conn, first.id, FinalOutcome::Failed).unwrap();
        mark_failed(&t.bar.conn, receipt.id).unwrap();

        let retry = begin_attempt(
            &t.bar.conn,
            receipt.id,
            "first print failed",
            t.shift,
            t.bar.cashier,
            LATER + 1,
        )
        .unwrap();
        assert_eq!((retry.print_no, retry.outcome), (2, Outcome::Unknown));
        assert_eq!(
            find(&t.bar.conn, receipt.id).unwrap().receipt_number,
            "CR-000001"
        );
        resolve_attempt(&t.bar.conn, retry.id, FinalOutcome::Success).unwrap();
        mark_printed(&t.bar.conn, receipt.id, LATER + 2).unwrap();

        let final_receipt = find(&t.bar.conn, receipt.id).unwrap();
        assert_eq!(final_receipt.status, Status::Printed);
        assert_eq!(final_receipt.receipt_number, "CR-000001");
        assert_eq!(attempts(&t.bar.conn, receipt.id).unwrap().len(), 2);
        let next_cr: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT next_value FROM sequences WHERE name = 'CUSTOMER_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next_cr, 2, "retrying the first print allocates no new CR");
    }

    #[test]
    fn reprints_keep_receipt_identity_and_require_a_reason() {
        let t = trading();
        let (_, receipt) = printed_issue(&t);
        assert!(begin_attempt(
            &t.bar.conn,
            receipt.id,
            "",
            t.shift,
            t.bar.cashier,
            LATER + 1
        )
        .is_err());
        let reprint = begin_attempt(
            &t.bar.conn,
            receipt.id,
            "customer copy damaged",
            t.shift,
            t.bar.cashier,
            LATER + 1,
        )
        .unwrap();
        assert_eq!((reprint.print_no, reprint.outcome), (2, Outcome::Unknown));
        resolve_attempt(&t.bar.conn, reprint.id, FinalOutcome::Failed).unwrap();
        assert_eq!(
            find(&t.bar.conn, receipt.id).unwrap().receipt_number,
            receipt.receipt_number
        );
        assert_eq!(attempts(&t.bar.conn, receipt.id).unwrap().len(), 2);
    }

    #[test]
    fn failed_issue_attempts_require_reason_and_permit_the_handwritten_path() {
        let t = trading();
        let order = draft_with_line(&t);
        let receipt = create_issue(&t.bar.conn, order.id, Destination::Bar, fixture::NOW).unwrap();
        orders::mark_printing(&t.bar.conn, order.id).unwrap();
        freeze_rendered_text(&t.bar.conn, receipt.id, "BR failed bytes").unwrap();
        assert!(record_first_issue_attempt(
            &t.bar.conn,
            receipt.id,
            FinalOutcome::Failed,
            "",
            t.shift,
            t.bar.cashier,
            LATER
        )
        .is_err());
        record_first_issue_attempt(
            &t.bar.conn,
            receipt.id,
            FinalOutcome::Failed,
            "Printer failed; handwritten chit authorised",
            t.shift,
            t.bar.cashier,
            LATER,
        )
        .unwrap();
        mark_failed(&t.bar.conn, receipt.id).unwrap();
        orders::mark_issued(&t.bar.conn, order.id, LATER).unwrap();
        assert_eq!(
            find(&t.bar.conn, receipt.id).unwrap().status,
            Status::Failed
        );
        assert_eq!(
            orders::find(&t.bar.conn, order.id).unwrap().status,
            orders::Status::Issued
        );
    }

    #[test]
    fn no_attempt_can_be_recorded_before_exact_rendered_text_is_frozen() {
        let t = trading();
        let issue_order = draft_with_line(&t);
        let issue =
            create_issue(&t.bar.conn, issue_order.id, Destination::Bar, fixture::NOW).unwrap();
        orders::mark_printing(&t.bar.conn, issue_order.id).unwrap();
        let issue_error = record_first_issue_attempt(
            &t.bar.conn,
            issue.id,
            FinalOutcome::Success,
            "",
            t.shift,
            t.bar.cashier,
            LATER,
        )
        .unwrap_err();
        assert!(issue_error
            .to_string()
            .contains("first pending issue attempt"));

        close_with_bill(&t);
        let customer = create_customer(&t.bar.conn, t.tab, t.bar.cashier, LATER).unwrap();
        let customer_error =
            begin_attempt(&t.bar.conn, customer.id, "", t.shift, t.bar.cashier, LATER).unwrap_err();
        assert!(customer_error
            .to_string()
            .contains("freeze the receipt text"));
        assert!(attempts(&t.bar.conn, issue.id).unwrap().is_empty());
        assert!(attempts(&t.bar.conn, customer.id).unwrap().is_empty());
    }

    #[test]
    fn till_operations_refuse_owner_and_inactive_cashier_identities() {
        let t = trading();
        close_with_bill(&t);
        let owner = create_customer(&t.bar.conn, t.tab, t.bar.owner, LATER).unwrap_err();
        assert!(owner.to_string().contains("active cashier"), "got: {owner}");

        t.bar
            .conn
            .execute("UPDATE staff SET active = 0 WHERE id = ?1", [t.bar.cashier])
            .unwrap();
        let inactive = create_customer(&t.bar.conn, t.tab, t.bar.cashier, LATER).unwrap_err();
        assert!(
            inactive.to_string().contains("active cashier"),
            "got: {inactive}"
        );
    }

    #[test]
    fn customer_receipt_requires_a_trading_shift_before_allocating_cr() {
        let t = trading();
        close_with_bill(&t);
        t.bar
            .conn
            .execute(
                "UPDATE shifts SET status = 'CLOSING' WHERE id = ?1",
                [t.shift],
            )
            .unwrap();
        let error = create_customer(&t.bar.conn, t.tab, t.bar.cashier, LATER).unwrap_err();
        assert!(error.to_string().contains("shift is open"), "got: {error}");
        let next_value: i64 = t
            .bar
            .conn
            .query_row(
                "SELECT next_value FROM sequences WHERE name = 'CUSTOMER_RECEIPT'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next_value, 1, "a closed-venue refusal burns no CR number");
    }
}
