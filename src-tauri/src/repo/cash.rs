//! Frozen bills, waiter settlements, and the drawer (section 7).
//!
//! Closing a tab records what the waiter owes; it does **not** put money in
//! the drawer.  Only a finalized cash reconciliation may create a
//! `RECONCILIATION` cash movement.  Keeping those paths separate is the main
//! control in this module: [`expected_cash`] is therefore only a checked sum
//! of the cash ledger.
//!
//! Repository functions never start or commit a transaction.  In particular,
//! creating, allocating, and finalizing a reconciliation are deliberately
//! separate so the command layer can wrap the whole workflow (and its audit
//! row) in one caller-owned transaction.

use std::collections::HashSet;

use rusqlite::Connection;

use super::{guarded, shifts, staff, tabs, RepoError, Result};
use crate::bill::Bill;
use crate::Settings;
use crate::{BasisPoints, Money};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabPayment {
    pub id: i64,
    pub tab_id: i64,
    pub waiter_id: i64,
    pub subtotal: Money,
    pub service_charge: Money,
    pub tax: Money,
    pub total: Money,
    pub liability: Money,
    pub is_comped: bool,
    pub comp_reason: Option<String>,
    pub tax_rate: BasisPoints,
    pub service_rate: BasisPoints,
    pub tax_inclusive: bool,
    pub charge_rates_known: bool,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
}

/// Close-time facts that cannot be derived from the closed tab itself.
///
/// Money and rates are intentionally absent. [`freeze_payment`] derives them
/// from immutable issued lines and the current validated venue settings, so a
/// caller cannot persist a self-consistent but false fiscal bill.
#[derive(Clone, Debug)]
pub struct NewTabPayment<'a> {
    pub tab_id: i64,
    pub comp_reason: Option<&'a str>,
    pub shift_id: i64,
    pub created_by: i64,
    pub created_at: i64,
}

const PAYMENT_COLUMNS: &str = "id, tab_id, waiter_id, subtotal_minor,
     service_charge_minor, tax_minor, total_minor, liability_minor, is_comped,
     comp_reason, tax_rate_bp, service_rate_bp, tax_inclusive,
     charge_rates_known, shift_id, created_by, created_at";

fn read_payment(row: &rusqlite::Row<'_>) -> rusqlite::Result<TabPayment> {
    let tax_rate: i64 = row.get(10)?;
    let service_rate: i64 = row.get(11)?;
    Ok(TabPayment {
        id: row.get(0)?,
        tab_id: row.get(1)?,
        waiter_id: row.get(2)?,
        subtotal: Money::from_minor(row.get(3)?),
        service_charge: Money::from_minor(row.get(4)?),
        tax: Money::from_minor(row.get(5)?),
        total: Money::from_minor(row.get(6)?),
        liability: Money::from_minor(row.get(7)?),
        is_comped: row.get::<_, i64>(8)? == 1,
        comp_reason: row.get(9)?,
        tax_rate: BasisPoints(
            u32::try_from(tax_rate)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, tax_rate))?,
        ),
        service_rate: BasisPoints(
            u32::try_from(service_rate)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, service_rate))?,
        ),
        tax_inclusive: row.get::<_, i64>(12)? == 1,
        charge_rates_known: row.get::<_, i64>(13)? == 1,
        shift_id: row.get(14)?,
        created_by: row.get(15)?,
        created_at: row.get(16)?,
    })
}

pub fn payment_for_tab(conn: &Connection, tab_id: i64) -> Result<TabPayment> {
    conn.query_row(
        &format!("SELECT {PAYMENT_COLUMNS} FROM tab_payments WHERE tab_id = ?1"),
        [tab_id],
        read_payment,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing {
            what: "frozen tab payment",
        },
        other => RepoError::Sqlite(other),
    })
}

/// Insert the append-only bill after the tab's closing facts have been set.
pub fn freeze_payment(conn: &Connection, new: &NewTabPayment<'_>) -> Result<TabPayment> {
    require_cashier(conn, new.created_by)?;
    require_open_shift(conn, new.shift_id)?;

    let tab = tabs::find(conn, new.tab_id)?;
    if tab.status != tabs::Status::Closed {
        return super::refuse("a bill can only be frozen for a closed tab");
    }
    if tab.closed_shift_id != Some(new.shift_id) {
        return super::refuse("the frozen bill must belong to the shift that closed the tab");
    }

    // Merely checking that caller-supplied components add up would allow a
    // perfectly self-consistent but false tax document. No fiscal amount is
    // accepted from the caller.
    let line_total = tabs::running_total(conn, new.tab_id)?;
    let config = Settings::load(conn)?.charge_config();
    let bill = Bill::calculate(line_total, &config).map_err(|_| {
        RepoError::Refused("that frozen bill is too large to calculate safely".into())
    })?;
    let liability = if tab.is_comped {
        Money::ZERO
    } else {
        bill.total
    };

    let comp_reason = new.comp_reason.map(str::trim);
    if tab.is_comped {
        if comp_reason.is_none_or(str::is_empty) {
            return super::refuse("a comped tab needs a reason");
        }
    } else {
        if comp_reason.is_some() {
            return super::refuse("a chargeable tab cannot carry a comp reason");
        }
    }

    guarded!(conn.execute(
        "INSERT INTO tab_payments
             (tab_id, waiter_id, subtotal_minor, service_charge_minor,
              tax_minor, total_minor, liability_minor, is_comped, comp_reason,
              tax_rate_bp, service_rate_bp, tax_inclusive, charge_rates_known,
              shift_id, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1,
                 ?13, ?14, ?15)",
        rusqlite::params![
            new.tab_id,
            tab.waiter_id,
            bill.net.minor(),
            bill.service_charge.minor(),
            bill.tax.minor(),
            bill.total.minor(),
            liability.minor(),
            i64::from(tab.is_comped),
            comp_reason,
            i64::from(bill.tax_rate.0),
            i64::from(bill.service_rate.0),
            i64::from(bill.tax_inclusive),
            new.shift_id,
            new.created_by,
            new.created_at,
        ],
    ))?;
    payment_for_tab(conn, new.tab_id)
}

/// Waiter liability still outstanding, including shortfalls carried forward.
pub fn held_balance(conn: &Connection, waiter_id: i64) -> Result<Money> {
    require_waiter(conn, waiter_id)?;

    let mut held = Money::ZERO;
    let mut liabilities = conn.prepare(
        "SELECT liability_minor FROM tab_payments
          WHERE waiter_id = ?1 ORDER BY id",
    )?;
    for row in liabilities.query_map([waiter_id], |row| row.get::<_, i64>(0))? {
        held = checked_add(held, Money::from_minor(row?))?;
    }

    let mut settlements = conn.prepare(
        "SELECT cash_minor, non_cash_minor, written_off_minor
           FROM reconciliations
          WHERE waiter_id = ?1 AND finalized_at IS NOT NULL
          ORDER BY id",
    )?;
    for row in settlements.query_map([waiter_id], |row| {
        Ok((
            Money::from_minor(row.get(0)?),
            Money::from_minor(row.get(1)?),
            Money::from_minor(row.get(2)?),
        ))
    })? {
        let (cash, non_cash, written_off) = row?;
        let settled = checked_add(checked_add(cash, non_cash)?, written_off)?;
        held = checked_sub(held, settled)?;
    }
    if held.is_negative() {
        return super::refuse("the waiter's held balance became negative");
    }
    Ok(held)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reconciliation {
    pub id: i64,
    pub waiter_id: i64,
    pub cashier_id: i64,
    pub expected: Money,
    pub cash: Money,
    pub non_cash: Money,
    pub written_off: Money,
    pub shortfall: Money,
    pub write_off_reason: Option<String>,
    pub shift_id: i64,
    pub created_at: i64,
    pub finalized_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct NewReconciliation<'a> {
    pub waiter_id: i64,
    pub cashier_id: i64,
    /// Exact liability of the tabs that will be allocated, or the current
    /// held balance for an old-shortfall settlement with no allocations.
    pub expected: Money,
    pub cash: Money,
    pub non_cash: Money,
    pub written_off: Money,
    pub write_off_reason: Option<&'a str>,
    pub shift_id: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub id: i64,
    pub reconciliation_id: i64,
    pub tab_id: i64,
    pub amount: Money,
}

const RECONCILIATION_COLUMNS: &str = "id, waiter_id, cashier_id, expected_minor,
     cash_minor, non_cash_minor, written_off_minor, shortfall_minor,
     write_off_reason, shift_id, created_at, finalized_at";

fn read_reconciliation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reconciliation> {
    Ok(Reconciliation {
        id: row.get(0)?,
        waiter_id: row.get(1)?,
        cashier_id: row.get(2)?,
        expected: Money::from_minor(row.get(3)?),
        cash: Money::from_minor(row.get(4)?),
        non_cash: Money::from_minor(row.get(5)?),
        written_off: Money::from_minor(row.get(6)?),
        shortfall: Money::from_minor(row.get(7)?),
        write_off_reason: row.get(8)?,
        shift_id: row.get(9)?,
        created_at: row.get(10)?,
        finalized_at: row.get(11)?,
    })
}

pub fn find_reconciliation(conn: &Connection, id: i64) -> Result<Reconciliation> {
    conn.query_row(
        &format!("SELECT {RECONCILIATION_COLUMNS} FROM reconciliations WHERE id = ?1"),
        [id],
        read_reconciliation,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing {
            what: "reconciliation",
        },
        other => RepoError::Sqlite(other),
    })
}

pub fn create_reconciliation(
    conn: &Connection,
    new: &NewReconciliation<'_>,
) -> Result<Reconciliation> {
    require_cashier(conn, new.cashier_id)?;
    require_waiter(conn, new.waiter_id)?;
    require_open_shift(conn, new.shift_id)?;

    for (name, amount) in [
        ("expected amount", new.expected),
        ("cash amount", new.cash),
        ("non-cash amount", new.non_cash),
        ("write-off amount", new.written_off),
    ] {
        if amount.is_negative() {
            return super::refuse(format!("the reconciliation's {name} cannot be negative"));
        }
    }
    let methods = [new.cash, new.non_cash, new.written_off]
        .into_iter()
        .filter(|amount| !amount.is_zero())
        .count();
    if methods > 1 {
        return super::refuse("a reconciliation uses one settlement method, not split tender");
    }

    let reason = new.write_off_reason.map(str::trim);
    if !new.written_off.is_zero() && reason.is_none_or(str::is_empty) {
        return super::refuse("a write-off needs a reason");
    }
    if new.written_off.is_zero() && reason.is_some() {
        return super::refuse("a write-off reason belongs only to a write-off");
    }

    let settled = checked_add(checked_add(new.cash, new.non_cash)?, new.written_off)?;
    if settled > new.expected {
        return super::refuse(format!(
            "settled {} exceeds the {} owed; return the difference to the waiter",
            settled, new.expected
        ));
    }
    let shortfall = checked_sub(new.expected, settled)?;

    guarded!(conn.execute(
        "INSERT INTO reconciliations
             (waiter_id, cashier_id, expected_minor, cash_minor, non_cash_minor,
              written_off_minor, shortfall_minor, write_off_reason, shift_id,
              created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            new.waiter_id,
            new.cashier_id,
            new.expected.minor(),
            new.cash.minor(),
            new.non_cash.minor(),
            new.written_off.minor(),
            shortfall.minor(),
            reason,
            new.shift_id,
            new.created_at,
        ],
    ))?;
    find_reconciliation(conn, conn.last_insert_rowid())
}

/// Attach one closed tab at its exact frozen liability and mark it reconciled.
/// The caller must roll back the surrounding transaction if either write
/// fails; committing between them is never valid.
pub fn allocate_tab(
    conn: &Connection,
    reconciliation_id: i64,
    tab_id: i64,
    by: i64,
) -> Result<Allocation> {
    require_cashier(conn, by)?;
    let reconciliation = find_reconciliation(conn, reconciliation_id)?;
    if reconciliation.cashier_id != by {
        return super::refuse("the cashier who started a reconciliation must finish it");
    }
    if reconciliation.finalized_at.is_some() {
        return super::refuse("nothing may be allocated after a reconciliation is finalized");
    }

    let tab = tabs::find(conn, tab_id)?;
    if tab.status != tabs::Status::Closed {
        return super::refuse("only a closed, unreconciled tab can be allocated");
    }
    let payment = payment_for_tab(conn, tab_id)?;
    if payment.waiter_id != reconciliation.waiter_id {
        return super::refuse("that tab belongs to a different waiter's balance");
    }

    guarded!(conn.execute(
        "INSERT INTO reconciliation_tabs (reconciliation_id, tab_id, amount_minor)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![reconciliation_id, tab_id, payment.liability.minor()],
    ))?;
    let allocation_id = conn.last_insert_rowid();

    let changed = guarded!(conn.execute(
        "UPDATE tabs SET status = 'RECONCILED'
          WHERE id = ?1 AND status = 'CLOSED'",
        [tab_id],
    ))?;
    if changed != 1 {
        return super::refuse("the tab changed before it could be reconciled");
    }

    Ok(Allocation {
        id: allocation_id,
        reconciliation_id,
        tab_id,
        amount: payment.liability,
    })
}

pub fn allocations(conn: &Connection, reconciliation_id: i64) -> Result<Vec<Allocation>> {
    let mut stmt = conn.prepare(
        "SELECT id, reconciliation_id, tab_id, amount_minor
           FROM reconciliation_tabs
          WHERE reconciliation_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([reconciliation_id], |row| {
        Ok(Allocation {
            id: row.get(0)?,
            reconciliation_id: row.get(1)?,
            tab_id: row.get(2)?,
            amount: Money::from_minor(row.get(3)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Seal a fully constructed reconciliation and, for cash only, credit the
/// drawer.  This function intentionally has no dependency on `tab_payments`.
pub fn finalize_reconciliation(
    conn: &Connection,
    reconciliation_id: i64,
    by: i64,
    finalized_at: i64,
) -> Result<Reconciliation> {
    require_cashier(conn, by)?;
    let reconciliation = find_reconciliation(conn, reconciliation_id)?;
    if reconciliation.cashier_id != by {
        return super::refuse("the cashier who started a reconciliation must finish it");
    }
    if reconciliation.finalized_at.is_some() {
        return super::refuse("the reconciliation is already finalized");
    }
    if finalized_at < reconciliation.created_at {
        return super::refuse("a reconciliation cannot finish before it started");
    }
    require_open_shift(conn, reconciliation.shift_id)?;

    let allocations = allocations(conn, reconciliation_id)?;
    let mut allocated = Money::ZERO;
    let mut distinct = HashSet::with_capacity(allocations.len());
    for allocation in &allocations {
        if !distinct.insert(allocation.tab_id) {
            return super::refuse("a tab was allocated more than once");
        }
        allocated = checked_add(allocated, allocation.amount)?;
    }

    if allocations.is_empty() {
        // This is the separate old-shortfall path from section 7.5.  Closed
        // tabs must be allocated first; otherwise their liability would be
        // cleared once here and then again when those tabs were reconciled.
        let closed_tabs_remain: bool = conn.query_row(
            "SELECT EXISTS (
                 SELECT 1
                   FROM tabs t JOIN tab_payments p ON p.tab_id = t.id
                  WHERE p.waiter_id = ?1 AND t.status = 'CLOSED'
             )",
            [reconciliation.waiter_id],
            |row| row.get(0),
        )?;
        if closed_tabs_remain {
            return super::refuse(
                "allocate every unreconciled closed tab before settling an old balance",
            );
        }
        if reconciliation.expected != held_balance(conn, reconciliation.waiter_id)? {
            return super::refuse("an old-balance settlement must use the current held balance");
        }
        let settled = checked_add(
            checked_add(reconciliation.cash, reconciliation.non_cash)?,
            reconciliation.written_off,
        )?;
        if settled.is_zero() {
            return super::refuse("an old-balance settlement must actually settle some money");
        }
    } else if allocated != reconciliation.expected {
        return super::refuse("allocated tab liabilities do not equal the expected settlement");
    }

    // Seal first, then write the matching drawer fact.  The command/service
    // layer owns the surrounding transaction, so either both statements
    // commit or neither does. The schema additionally refuses a movement
    // unless it sees this exact finalized timestamp.
    let changed = guarded!(conn.execute(
        "UPDATE reconciliations SET finalized_at = ?2
          WHERE id = ?1 AND finalized_at IS NULL",
        rusqlite::params![reconciliation_id, finalized_at],
    ))?;
    if changed != 1 {
        return super::refuse("the reconciliation was finalized concurrently");
    }
    if !reconciliation.cash.is_zero() {
        record_reconciliation_cash(conn, reconciliation_id, by, finalized_at)?;
    }
    find_reconciliation(conn, reconciliation_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovementType {
    OpeningFloat,
    Reconciliation,
    Payout,
    Adjustment,
}

impl MovementType {
    pub const fn as_str(self) -> &'static str {
        match self {
            MovementType::OpeningFloat => "OPENING_FLOAT",
            MovementType::Reconciliation => "RECONCILIATION",
            MovementType::Payout => "PAYOUT",
            MovementType::Adjustment => "ADJUSTMENT",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "OPENING_FLOAT" => Ok(Self::OpeningFloat),
            "RECONCILIATION" => Ok(Self::Reconciliation),
            "PAYOUT" => Ok(Self::Payout),
            "ADJUSTMENT" => Ok(Self::Adjustment),
            _ => super::refuse(format!(
                "'{text}' is not a cash movement type this build knows"
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CashMovement {
    pub id: i64,
    pub shift_id: i64,
    pub movement_type: MovementType,
    pub amount: Money,
    pub category: Option<String>,
    pub reason: String,
    pub reconciliation_id: Option<i64>,
    pub created_by: i64,
    pub created_at: i64,
}

fn read_movement(row: &rusqlite::Row<'_>) -> rusqlite::Result<CashMovement> {
    let raw_type: String = row.get(2)?;
    let movement_type = MovementType::parse(&raw_type).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown cash movement type '{raw_type}'"
            ))),
        )
    })?;
    Ok(CashMovement {
        id: row.get(0)?,
        shift_id: row.get(1)?,
        movement_type,
        amount: Money::from_minor(row.get(3)?),
        category: row.get(4)?,
        reason: row.get(5)?,
        reconciliation_id: row.get(6)?,
        created_by: row.get(7)?,
        created_at: row.get(8)?,
    })
}

const MOVEMENT_COLUMNS: &str = "id, shift_id, movement_type, amount_minor,
     category, reason, reconciliation_id, created_by, created_at";

pub fn movements(conn: &Connection, shift_id: i64) -> Result<Vec<CashMovement>> {
    shifts::find(conn, shift_id)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {MOVEMENT_COLUMNS} FROM cash_movements
          WHERE shift_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([shift_id], read_movement)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Credit the drawer with exactly the cash portion of one finalized settlement.
fn record_reconciliation_cash(
    conn: &Connection,
    reconciliation_id: i64,
    by: i64,
    at: i64,
) -> Result<CashMovement> {
    require_cashier(conn, by)?;
    let reconciliation = find_reconciliation(conn, reconciliation_id)?;
    if reconciliation.cashier_id != by {
        return super::refuse("only the reconciliation's cashier can receive its cash");
    }
    if reconciliation.finalized_at != Some(at) {
        return super::refuse(
            "reconciliation cash enters the drawer only after it is finalized, at the exact finalization time",
        );
    }
    if reconciliation.cash.is_zero() || reconciliation.cash.is_negative() {
        return super::refuse("only a positive cash settlement enters the drawer");
    }
    require_open_shift(conn, reconciliation.shift_id)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM cash_movements WHERE reconciliation_id = ?1
         )",
        [reconciliation_id],
        |row| row.get(0),
    )?;
    if exists {
        return super::refuse("that reconciliation's cash is already in the drawer");
    }
    insert_movement(
        conn,
        reconciliation.shift_id,
        MovementType::Reconciliation,
        reconciliation.cash,
        None,
        "",
        Some(reconciliation_id),
        by,
        at,
    )
}

pub fn record_payout(
    conn: &Connection,
    shift_id: i64,
    amount: Money,
    category: &str,
    reason: &str,
    by: i64,
    at: i64,
) -> Result<CashMovement> {
    require_cashier(conn, by)?;
    require_open_shift(conn, shift_id)?;
    if amount.is_zero() || amount.is_negative() {
        return super::refuse("a payout amount must be positive");
    }
    let category = category.trim();
    if category.is_empty() {
        return super::refuse("a payout needs a category");
    }
    let stored = amount
        .minor()
        .checked_neg()
        .map(Money::from_minor)
        .ok_or_else(overflow)?;
    insert_movement(
        conn,
        shift_id,
        MovementType::Payout,
        stored,
        Some(category),
        reason.trim(),
        None,
        by,
        at,
    )
}

pub fn record_adjustment(
    conn: &Connection,
    shift_id: i64,
    amount: Money,
    reason: &str,
    by: i64,
    at: i64,
) -> Result<CashMovement> {
    require_cashier(conn, by)?;
    require_open_shift(conn, shift_id)?;
    if amount.is_zero() {
        return super::refuse("a cash adjustment cannot be zero");
    }
    let reason = reason.trim();
    if reason.is_empty() {
        return super::refuse("a cash adjustment needs a reason");
    }
    insert_movement(
        conn,
        shift_id,
        MovementType::Adjustment,
        amount,
        None,
        reason,
        None,
        by,
        at,
    )
}

/// The sole expected-cash formula: a checked sum of the drawer ledger.
pub fn expected_cash(conn: &Connection, shift_id: i64) -> Result<Money> {
    shifts::find(conn, shift_id)?;
    let mut expected = Money::ZERO;
    let mut stmt = conn.prepare(
        "SELECT amount_minor FROM cash_movements
          WHERE shift_id = ?1 ORDER BY id",
    )?;
    for row in stmt.query_map([shift_id], |row| row.get::<_, i64>(0))? {
        expected = checked_add(expected, Money::from_minor(row?))?;
    }
    Ok(expected)
}

#[allow(clippy::too_many_arguments)]
fn insert_movement(
    conn: &Connection,
    shift_id: i64,
    movement_type: MovementType,
    amount: Money,
    category: Option<&str>,
    reason: &str,
    reconciliation_id: Option<i64>,
    by: i64,
    at: i64,
) -> Result<CashMovement> {
    guarded!(conn.execute(
        "INSERT INTO cash_movements
             (shift_id, movement_type, amount_minor, category, reason,
              reconciliation_id, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            shift_id,
            movement_type.as_str(),
            amount.minor(),
            category,
            reason,
            reconciliation_id,
            by,
            at,
        ],
    ))?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        &format!("SELECT {MOVEMENT_COLUMNS} FROM cash_movements WHERE id = ?1"),
        [id],
        read_movement,
    )
    .map_err(RepoError::Sqlite)
}

fn require_cashier(conn: &Connection, id: i64) -> Result<()> {
    let person = staff::find(conn, id)?;
    if !person.active || person.role != staff::Role::Cashier {
        return super::refuse("an active cashier must operate the money workflow");
    }
    Ok(())
}

fn require_waiter(conn: &Connection, id: i64) -> Result<()> {
    let person = staff::find(conn, id)?;
    if person.role != staff::Role::Waiter {
        return super::refuse("a waiter balance must belong to a waiter");
    }
    Ok(())
}

fn require_open_shift(conn: &Connection, id: i64) -> Result<shifts::Shift> {
    let shift = shifts::find(conn, id)?;
    if shift.status != shifts::Status::Open {
        return super::refuse("money operations require an open shift");
    }
    Ok(shift)
}

fn overflow() -> RepoError {
    RepoError::Refused("money arithmetic overflowed 64 bits".into())
}

fn checked_add(left: Money, right: Money) -> Result<Money> {
    left.checked_add(right).map_err(|_| overflow())
}

fn checked_sub(left: Money, right: Money) -> Result<Money> {
    left.checked_sub(right).map_err(|_| overflow())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, Bar, NOW};
    use crate::settings::{self, keys};

    const LATER: i64 = NOW + 1_000;
    const END: i64 = NOW + 8 * 60 * 60 * 1_000;

    fn open_shift(bar: &Bar, float: i64) -> shifts::Shift {
        shifts::open(
            &bar.conn,
            &shifts::NewShift {
                business_date: "2025-07-31",
                opened_at: NOW,
                opened_by: bar.cashier,
                opening_float: Money::from_minor(float),
                expected_end_at: END,
            },
        )
        .unwrap()
    }

    fn close_tab_with_payment(
        bar: &Bar,
        shift: i64,
        waiter: i64,
        suffix: &str,
        liability: i64,
    ) -> i64 {
        bar.conn
            .execute(
                "INSERT INTO tabs
                     (code, opened_shift_id, waiter_id, reference_mode, table_no,
                      display_label, opened_at, opened_by)
                 VALUES (?1, ?2, ?3, 'TABLE', ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    format!("TAB-{suffix}"),
                    shift,
                    waiter,
                    suffix,
                    format!("Table {suffix}"),
                    NOW,
                    bar.cashier,
                ],
            )
            .unwrap();
        let tab_id = bar.conn.last_insert_rowid();
        bar.conn
            .execute(
                "INSERT INTO orders
                     (tab_id, shift_id, waiter_id, cashier_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![tab_id, shift, waiter, bar.cashier, NOW],
            )
            .unwrap();
        let order_id = bar.conn.last_insert_rowid();
        let recipe_id: i64 = bar
            .conn
            .query_row(
                "SELECT id FROM recipes WHERE sale_item_id = ?1 AND effective_to IS NULL",
                [bar.beer_bottle],
                |row| row.get(0),
            )
            .unwrap();
        bar.conn
            .execute(
                "INSERT INTO order_lines
                     (order_id, sale_item_id, sale_item_name, recipe_id,
                      quantity_milli, unit_price_minor, line_total_minor)
                 VALUES (?1, ?2, 'Beer', ?3, 1000, ?4, ?4)",
                rusqlite::params![order_id, bar.beer_bottle, recipe_id, liability],
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
                rusqlite::params![order_id, LATER],
            )
            .unwrap();
        bar.conn
            .execute(
                "UPDATE tabs SET status = 'CLOSED', closed_shift_id = ?2,
                                 closed_at = ?3, closed_by = ?4
                  WHERE id = ?1",
                rusqlite::params![tab_id, shift, LATER, bar.cashier],
            )
            .unwrap();
        freeze_payment(
            &bar.conn,
            &NewTabPayment {
                tab_id,
                comp_reason: None,
                shift_id: shift,
                created_by: bar.cashier,
                created_at: LATER,
            },
        )
        .unwrap();
        tab_id
    }

    fn reconciliation<'a>(
        bar: &Bar,
        shift: i64,
        expected: i64,
        cash: i64,
        non_cash: i64,
    ) -> NewReconciliation<'a> {
        NewReconciliation {
            waiter_id: bar.sara,
            cashier_id: bar.cashier,
            expected: Money::from_minor(expected),
            cash: Money::from_minor(cash),
            non_cash: Money::from_minor(non_cash),
            written_off: Money::ZERO,
            write_off_reason: None,
            shift_id: shift,
            created_at: LATER + 1,
        }
    }

    fn disable_charges(bar: &Bar) {
        settings::put(&bar.conn, keys::SERVICE_ENABLED, "0", Some(bar.owner), NOW).unwrap();
        settings::put(&bar.conn, keys::TAX_ENABLED, "0", Some(bar.owner), NOW).unwrap();
    }

    #[test]
    fn a_frozen_bill_is_derived_in_all_four_charge_modes() {
        let modes = [
            (false, false, false, 10_000, 0, 0, 10_000),
            (false, false, true, 10_000, 1_000, 0, 11_000),
            (true, false, true, 10_000, 1_000, 1_650, 12_650),
            (true, true, true, 8_696, 870, 1_435, 11_001),
        ];
        for (index, (tax, inclusive, service, net, charge, vat, total)) in
            modes.into_iter().enumerate()
        {
            let bar = fixture::bar();
            settings::put(
                &bar.conn,
                keys::TAX_ENABLED,
                if tax { "1" } else { "0" },
                Some(bar.owner),
                NOW,
            )
            .unwrap();
            settings::put(
                &bar.conn,
                keys::TAX_INCLUSIVE,
                if inclusive { "1" } else { "0" },
                Some(bar.owner),
                NOW,
            )
            .unwrap();
            settings::put(&bar.conn, keys::TAX_RATE_BP, "1500", Some(bar.owner), NOW).unwrap();
            settings::put(
                &bar.conn,
                keys::SERVICE_ENABLED,
                if service { "1" } else { "0" },
                Some(bar.owner),
                NOW,
            )
            .unwrap();
            settings::put(
                &bar.conn,
                keys::SERVICE_RATE_BP,
                "1000",
                Some(bar.owner),
                NOW,
            )
            .unwrap();
            let shift = open_shift(&bar, 0);
            let tab =
                close_tab_with_payment(&bar, shift.id, bar.sara, &(index + 20).to_string(), 10_000);
            let frozen = payment_for_tab(&bar.conn, tab).unwrap();
            assert_eq!(frozen.subtotal.minor(), net);
            assert_eq!(frozen.service_charge.minor(), charge);
            assert_eq!(frozen.tax.minor(), vat);
            assert_eq!(frozen.total.minor(), total);
            assert_eq!(frozen.liability, frozen.total);
        }
    }

    #[test]
    fn a_tab_payment_is_read_back_and_database_frozen() {
        let bar = fixture::bar();
        disable_charges(&bar);
        let shift = open_shift(&bar, 0);
        let tab = close_tab_with_payment(&bar, shift.id, bar.sara, "1", 12_500);

        let payment = payment_for_tab(&bar.conn, tab).unwrap();
        assert_eq!(payment.waiter_id, bar.sara);
        assert_eq!(payment.total, Money::from_minor(12_500));
        assert_eq!(payment.liability, payment.total);
        assert!(payment.charge_rates_known);
        assert_eq!(held_balance(&bar.conn, bar.sara).unwrap(), payment.total);

        let error = bar
            .conn
            .execute(
                "UPDATE tab_payments SET liability_minor = 1 WHERE tab_id = ?1",
                [tab],
            )
            .unwrap_err();
        assert!(error.to_string().contains("append-only"), "got: {error}");
    }

    #[test]
    fn finalized_partial_cash_reduces_held_and_credits_only_cash() {
        let bar = fixture::bar();
        disable_charges(&bar);
        let shift = open_shift(&bar, 5_000);
        let first = close_tab_with_payment(&bar, shift.id, bar.sara, "1", 12_000);
        close_tab_with_payment(&bar, shift.id, bar.sara, "2", 8_000);

        let draft = create_reconciliation(
            &bar.conn,
            &reconciliation(&bar, shift.id, 12_000, 10_000, 0),
        )
        .unwrap();
        let allocation = allocate_tab(&bar.conn, draft.id, first, bar.cashier).unwrap();
        assert_eq!(allocation.amount, Money::from_minor(12_000));
        assert_eq!(
            held_balance(&bar.conn, bar.sara).unwrap(),
            Money::from_minor(20_000),
            "a draft reconciliation never changes held balance"
        );

        let finalised =
            finalize_reconciliation(&bar.conn, draft.id, bar.cashier, LATER + 2).unwrap();
        assert_eq!(finalised.shortfall, Money::from_minor(2_000));
        assert_eq!(
            held_balance(&bar.conn, bar.sara).unwrap(),
            Money::from_minor(10_000)
        );
        assert_eq!(
            expected_cash(&bar.conn, shift.id).unwrap(),
            Money::from_minor(15_000)
        );
        assert_eq!(
            tabs::find(&bar.conn, first).unwrap().status,
            tabs::Status::Reconciled
        );
    }

    #[test]
    fn non_cash_clears_liability_without_entering_the_drawer() {
        let bar = fixture::bar();
        disable_charges(&bar);
        let shift = open_shift(&bar, 2_000);
        let tab = close_tab_with_payment(&bar, shift.id, bar.sara, "1", 7_500);
        let draft =
            create_reconciliation(&bar.conn, &reconciliation(&bar, shift.id, 7_500, 0, 7_500))
                .unwrap();
        allocate_tab(&bar.conn, draft.id, tab, bar.cashier).unwrap();
        finalize_reconciliation(&bar.conn, draft.id, bar.cashier, LATER + 2).unwrap();

        assert_eq!(held_balance(&bar.conn, bar.sara).unwrap(), Money::ZERO);
        assert_eq!(
            expected_cash(&bar.conn, shift.id).unwrap(),
            Money::from_minor(2_000)
        );
        assert_eq!(movements(&bar.conn, shift.id).unwrap().len(), 1);
    }

    #[test]
    fn reconciliation_cash_cannot_be_committed_before_its_settlement_is_finalized() {
        let mut bar = fixture::bar();
        disable_charges(&bar);
        let shift = open_shift(&bar, 0);
        let tab = close_tab_with_payment(&bar, shift.id, bar.sara, "cash-ordering", 7_500);
        let draft =
            create_reconciliation(&bar.conn, &reconciliation(&bar, shift.id, 7_500, 7_500, 0))
                .unwrap();
        allocate_tab(&bar.conn, draft.id, tab, bar.cashier).unwrap();

        let finalized_at = LATER + 2;
        let premature =
            record_reconciliation_cash(&bar.conn, draft.id, bar.cashier, finalized_at).unwrap_err();
        assert!(
            premature.to_string().contains("finalized"),
            "got: {premature}"
        );
        let before: i64 = bar
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cash_movements WHERE reconciliation_id = ?1",
                [draft.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 0);

        // Prove the service-owned transaction rolls the finalized flag back
        // when the later drawer write fails. This trigger fires only after
        // the settlement has been sealed, so the test also proves ordering.
        bar.conn
            .execute_batch(
                "CREATE TEMP TRIGGER fail_finalized_reconciliation_cash
                 BEFORE INSERT ON cash_movements
                 WHEN NEW.movement_type = 'RECONCILIATION'
                  AND EXISTS (SELECT 1 FROM reconciliations
                               WHERE id = NEW.reconciliation_id
                                 AND finalized_at IS NOT NULL)
                 BEGIN SELECT RAISE(ABORT, 'injected drawer failure'); END;",
            )
            .unwrap();
        let tx = bar.conn.transaction().unwrap();
        let injected =
            finalize_reconciliation(&tx, draft.id, bar.cashier, finalized_at).unwrap_err();
        assert!(injected.to_string().contains("injected drawer failure"));
        drop(tx);
        assert_eq!(
            find_reconciliation(&bar.conn, draft.id)
                .unwrap()
                .finalized_at,
            None,
        );
        bar.conn
            .execute_batch("DROP TRIGGER fail_finalized_reconciliation_cash")
            .unwrap();

        // Finalization and the matching drawer entry now commit together.
        let tx = bar.conn.transaction().unwrap();
        let finalized = finalize_reconciliation(&tx, draft.id, bar.cashier, finalized_at).unwrap();
        assert_eq!(finalized.finalized_at, Some(finalized_at));
        tx.commit().unwrap();

        let violations: i64 = bar
            .conn
            .query_row(
                "SELECT COUNT(*)
                   FROM cash_movements movement
                   JOIN reconciliations settlement
                     ON settlement.id = movement.reconciliation_id
                  WHERE movement.movement_type = 'RECONCILIATION'
                    AND (settlement.finalized_at IS NULL
                         OR settlement.finalized_at <> movement.created_at)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(violations, 0);

        let wrong_time =
            record_reconciliation_cash(&bar.conn, draft.id, bar.cashier, finalized_at + 1)
                .unwrap_err();
        assert!(wrong_time.to_string().contains("exact finalization time"));
    }

    #[test]
    fn payouts_and_adjustments_are_itemised_checked_cash_movements() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 10_000);

        let payout = record_payout(
            &bar.conn,
            shift.id,
            Money::from_minor(2_500),
            "supplies",
            "ice",
            bar.cashier,
            LATER,
        )
        .unwrap();
        assert_eq!(payout.amount, Money::from_minor(-2_500));
        record_adjustment(
            &bar.conn,
            shift.id,
            Money::from_minor(300),
            "count correction",
            bar.cashier,
            LATER + 1,
        )
        .unwrap();
        assert_eq!(
            expected_cash(&bar.conn, shift.id).unwrap(),
            Money::from_minor(7_800)
        );

        let missing_category = record_payout(
            &bar.conn,
            shift.id,
            Money::from_minor(1),
            " ",
            "anything",
            bar.cashier,
            LATER,
        )
        .unwrap_err();
        assert!(missing_category.to_string().contains("category"));

        let owner = record_adjustment(
            &bar.conn,
            shift.id,
            Money::from_minor(1),
            "not an operator",
            bar.owner,
            LATER,
        )
        .unwrap_err();
        assert!(owner.to_string().contains("active cashier"), "got: {owner}");
    }

    #[test]
    fn expected_cash_refuses_overflow_instead_of_wrapping() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, 0);
        bar.conn
            .execute(
                "INSERT INTO cash_movements
                     (shift_id, movement_type, amount_minor, reason, created_by, created_at)
                 VALUES (?1, 'ADJUSTMENT', ?2, 'large test amount', ?3, ?4),
                        (?1, 'ADJUSTMENT', 1, 'overflow edge', ?3, ?4)",
                rusqlite::params![shift.id, i64::MAX, bar.cashier, LATER],
            )
            .unwrap();

        let error = expected_cash(&bar.conn, shift.id).unwrap_err();
        assert!(error.to_string().contains("overflowed"), "got: {error}");
    }

    #[test]
    fn reconciliation_rejects_split_tender_and_wrong_allocations() {
        let bar = fixture::bar();
        disable_charges(&bar);
        let shift = open_shift(&bar, 0);
        let sara = close_tab_with_payment(&bar, shift.id, bar.sara, "1", 1_000);
        let dawit = close_tab_with_payment(&bar, shift.id, bar.dawit, "2", 1_000);

        let split = create_reconciliation(
            &bar.conn,
            &NewReconciliation {
                cash: Money::from_minor(500),
                non_cash: Money::from_minor(500),
                ..reconciliation(&bar, shift.id, 1_000, 0, 0)
            },
        )
        .unwrap_err();
        assert!(split.to_string().contains("split tender"), "got: {split}");

        let draft =
            create_reconciliation(&bar.conn, &reconciliation(&bar, shift.id, 1_000, 1_000, 0))
                .unwrap();
        let wrong = allocate_tab(&bar.conn, draft.id, dawit, bar.cashier).unwrap_err();
        assert!(
            wrong.to_string().contains("different waiter"),
            "got: {wrong}"
        );
        allocate_tab(&bar.conn, draft.id, sara, bar.cashier).unwrap();
        finalize_reconciliation(&bar.conn, draft.id, bar.cashier, LATER + 2).unwrap();
    }
}
