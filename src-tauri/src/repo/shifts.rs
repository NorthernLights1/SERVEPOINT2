//! Business-night shifts (section 4).
//!
//! A shift owns one immutable business date.  It moves only
//! `OPEN -> CLOSING -> CLOSED`; the database triggers remain the final guard,
//! while this module refuses known mistakes before allocating a sequence
//! number.  As with the rest of `repo`, callers wrap multi-statement writes in
//! their transaction.  In particular, [`open`] writes both the shift and its
//! opening-float cash movement as one logical operation.

use rusqlite::Connection;

use super::{guarded, staff, RepoError, Result};
use crate::Money;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Closing,
    Closed,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Open => "OPEN",
            Status::Closing => "CLOSING",
            Status::Closed => "CLOSED",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "OPEN" => Ok(Status::Open),
            "CLOSING" => Ok(Status::Closing),
            "CLOSED" => Ok(Status::Closed),
            _ => super::refuse(format!("'{text}' is not a shift status this build knows")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shift {
    pub id: i64,
    pub code: String,
    pub business_date: String,
    pub status: Status,
    pub opened_at: i64,
    pub opened_by: i64,
    pub opening_float: Money,
    pub expected_end_at: i64,
    pub closed_at: Option<i64>,
    pub closed_by: Option<i64>,
    pub counted_cash: Option<Money>,
}

/// Facts snapshotted exactly once when a business night starts.
#[derive(Clone, Debug)]
pub struct NewShift<'a> {
    pub business_date: &'a str,
    pub opened_at: i64,
    pub opened_by: i64,
    pub opening_float: Money,
    pub expected_end_at: i64,
}

const COLUMNS: &str = "id, code, business_date, status, opened_at, opened_by,
     opening_float_minor, expected_end_at, closed_at, closed_by, counted_cash_minor";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Shift> {
    let status: String = row.get(3)?;
    let status = Status::parse(&status).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown shift status '{status}'"
            ))),
        )
    })?;
    Ok(Shift {
        id: row.get(0)?,
        code: row.get(1)?,
        business_date: row.get(2)?,
        status,
        opened_at: row.get(4)?,
        opened_by: row.get(5)?,
        opening_float: Money::from_minor(row.get(6)?),
        expected_end_at: row.get(7)?,
        closed_at: row.get(8)?,
        closed_by: row.get(9)?,
        counted_cash: row.get::<_, Option<i64>>(10)?.map(Money::from_minor),
    })
}

pub fn find(conn: &Connection, id: i64) -> Result<Shift> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM shifts WHERE id = ?1"),
        [id],
        read,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what: "shift" },
        other => RepoError::Sqlite(other),
    })
}

/// The one business night that has not yet finished, if there is one.
pub fn active(conn: &Connection) -> Result<Option<Shift>> {
    let found = conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM shifts
                  WHERE status IN ('OPEN','CLOSING')
                  ORDER BY opened_at DESC LIMIT 1"
            ),
            [],
            read,
        )
        .optional()?;
    Ok(found)
}

/// Open one business night and put a positive float onto the sole cash ledger.
///
/// `business_date` and `expected_end_at` are supplied by the calendar layer
/// and stored verbatim.  This function never asks the wall clock to derive
/// them again.
pub fn open(conn: &Connection, new: &NewShift<'_>) -> Result<Shift> {
    require_cashier(conn, new.opened_by)?;
    if new.opening_float.is_negative() {
        return super::refuse("an opening float cannot be negative");
    }

    let business_date = new.business_date.trim();
    if chrono::NaiveDate::parse_from_str(business_date, "%Y-%m-%d").is_err() {
        return super::refuse("a business date must be written as YYYY-MM-DD");
    }
    if active(conn)?.is_some() {
        return super::refuse("finish the active shift before opening another");
    }
    let already_traded: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM shifts WHERE business_date = ?1)",
        [business_date],
        |row| row.get(0),
    )?;
    if already_traded {
        return super::refuse("that business date has already traded");
    }

    let (_, code) = super::seq::next(conn, super::seq::Counter::Shift)?;
    guarded!(conn.execute(
        "INSERT INTO shifts
             (code, business_date, opened_at, opened_by,
              opening_float_minor, expected_end_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            code,
            business_date,
            new.opened_at,
            new.opened_by,
            new.opening_float.minor(),
            new.expected_end_at,
        ],
    ))?;
    let shift_id = conn.last_insert_rowid();

    if !new.opening_float.is_zero() {
        guarded!(conn.execute(
            "INSERT INTO cash_movements
                 (shift_id, movement_type, amount_minor, reason, created_by, created_at)
             VALUES (?1, 'OPENING_FLOAT', ?2, '', ?3, ?4)",
            rusqlite::params![
                shift_id,
                new.opening_float.minor(),
                new.opened_by,
                new.opened_at,
            ],
        ))?;
    }

    find(conn, shift_id)
}

/// Seal the night with what was actually counted out of the drawer.
///
/// `counted_cash` is the physical count, not a figure derived from the ledger.
/// The two are deliberately kept apart: `cash::expected_cash` says what should
/// be there and this says what is, and the difference between them is the
/// whole point of counting. A close never adjusts one to match the other.
///
/// The schema's own trigger refuses a `CLOSED` row missing any of the three
/// close-time facts; the checks here exist to refuse the mistake in words
/// first.
pub fn close(
    conn: &Connection,
    shift_id: i64,
    counted_cash: Money,
    by: i64,
    at: i64,
) -> Result<Shift> {
    require_cashier(conn, by)?;
    if counted_cash.is_negative() {
        return super::refuse("a counted drawer cannot hold less than nothing");
    }
    let shift = find(conn, shift_id)?;
    if shift.status != Status::Closing {
        return super::refuse("only a shift that has begun closing can be closed");
    }
    if at < shift.opened_at {
        return super::refuse("a night cannot close before it opened");
    }
    if !recovery_complete(conn)? {
        return super::refuse("resolve every outstanding print attempt before closing the shift");
    }

    // A tab still open, or closed but never settled, means money is
    // unaccounted for. Sealing the night over it would bury the discrepancy
    // in a report nobody can later explain.
    let unsettled: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tabs WHERE status IN ('OPEN','CLOSED'))",
        [],
        |row| row.get(0),
    )?;
    if unsettled {
        return super::refuse("every tab must be settled and reconciled before the night closes");
    }

    let changed = guarded!(conn.execute(
        "UPDATE shifts
            SET status = 'CLOSED', closed_at = ?2, closed_by = ?3, counted_cash_minor = ?4
          WHERE id = ?1 AND status = 'CLOSING'",
        rusqlite::params![shift_id, at, by, counted_cash.minor()],
    ))?;
    if changed != 1 {
        return super::refuse("the shift changed before it could close");
    }
    find(conn, shift_id)
}

/// Whether all crash-sensitive print work has been resolved.
pub fn recovery_complete(conn: &Connection) -> Result<bool> {
    let complete = conn.query_row(
        "SELECT NOT EXISTS (
             SELECT 1 FROM orders WHERE status = 'PRINTING'
             UNION ALL
             SELECT 1
               FROM orders o
               JOIN receipts r ON r.order_id = o.id AND r.receipt_type = 'ISSUE'
              WHERE o.status = 'DRAFT' AND r.status = 'VOID'
             UNION ALL
             SELECT 1 FROM receipt_prints WHERE outcome = 'UNKNOWN'
         )",
        [],
        |row| row.get(0),
    )?;
    Ok(complete)
}

/// Stop new trading only after the print-recovery gate is clear.
pub fn begin_closing(conn: &Connection, shift_id: i64, by: i64) -> Result<Shift> {
    require_cashier(conn, by)?;
    let shift = find(conn, shift_id)?;
    if shift.status != Status::Open {
        return super::refuse("only an open shift can begin closing");
    }
    if !recovery_complete(conn)? {
        return super::refuse("resolve every outstanding print attempt before closing the shift");
    }

    let changed = guarded!(conn.execute(
        "UPDATE shifts SET status = 'CLOSING' WHERE id = ?1 AND status = 'OPEN'",
        [shift_id],
    ))?;
    if changed != 1 {
        return super::refuse("the shift stopped trading before closing could begin");
    }
    find(conn, shift_id)
}

/// The warning is based on the expected end frozen at open, never today's
/// calendar setting.
pub fn is_overdue(shift: &Shift, now: i64) -> bool {
    shift.status != Status::Closed && now > shift.expected_end_at
}

fn require_cashier(conn: &Connection, id: i64) -> Result<()> {
    let person = staff::find(conn, id)?;
    if !person.active || person.role != staff::Role::Cashier {
        return super::refuse("an active cashier must operate the shift");
    }
    Ok(())
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, NOW};

    const END: i64 = NOW + 8 * 60 * 60 * 1_000;

    fn request<'a>(business_date: &'a str, cashier: i64, float: i64) -> NewShift<'a> {
        NewShift {
            business_date,
            opened_at: NOW,
            opened_by: cashier,
            opening_float: Money::from_minor(float),
            expected_end_at: END,
        }
    }

    #[test]
    fn open_freezes_the_business_date_end_and_opening_float() {
        let bar = fixture::bar();
        let shift = open(&bar.conn, &request("2025-07-31", bar.cashier, 25_000)).unwrap();

        assert_eq!(shift.code, "SHIFT-000001");
        assert_eq!(shift.business_date, "2025-07-31");
        assert_eq!(shift.expected_end_at, END);
        assert_eq!(shift.opening_float, Money::from_minor(25_000));
        assert_eq!(shift.status, Status::Open);

        let movement: (String, i64, i64, i64) = bar
            .conn
            .query_row(
                "SELECT movement_type, amount_minor, created_by, created_at
                   FROM cash_movements WHERE shift_id = ?1",
                [shift.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(movement, ("OPENING_FLOAT".into(), 25_000, bar.cashier, NOW));
        assert!(!is_overdue(&shift, END));
        assert!(is_overdue(&shift, END + 1));
    }

    #[test]
    fn a_zero_float_does_not_invent_a_cash_movement() {
        let bar = fixture::bar();
        let shift = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap();
        let count: i64 = bar
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cash_movements WHERE shift_id = ?1",
                [shift.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn only_an_active_cashier_can_operate_a_shift() {
        let bar = fixture::bar();
        for actor in [bar.owner, bar.sara] {
            let err = open(&bar.conn, &request("2025-07-31", actor, 0)).unwrap_err();
            assert!(err.to_string().contains("active cashier"), "got: {err}");
        }

        staff::set_active(&bar.conn, bar.cashier, false, NOW).unwrap();
        let err = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap_err();
        assert!(err.to_string().contains("active cashier"), "got: {err}");
    }

    #[test]
    fn invalid_or_negative_opening_facts_do_not_consume_a_number() {
        let bar = fixture::bar();
        for date in ["31-07-2025", "2025-19-39", "2025-02-29"] {
            let bad_date = open(&bar.conn, &request(date, bar.cashier, 0)).unwrap_err();
            assert!(
                bad_date.to_string().contains("YYYY-MM-DD"),
                "got: {bad_date}"
            );
        }
        let negative = open(&bar.conn, &request("2025-07-31", bar.cashier, -1)).unwrap_err();
        assert!(
            negative.to_string().contains("cannot be negative"),
            "got: {negative}"
        );

        let shift = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap();
        assert_eq!(shift.code, "SHIFT-000001");
    }

    #[test]
    fn a_closing_shift_is_still_active_and_a_business_date_never_repeats() {
        let bar = fixture::bar();
        let first = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap();
        begin_closing(&bar.conn, first.id, bar.cashier).unwrap();

        let err = open(&bar.conn, &request("2025-08-01", bar.cashier, 0)).unwrap_err();
        assert!(err.to_string().contains("active shift"), "got: {err}");

        bar.conn
            .execute(
                "UPDATE shifts
                    SET status = 'CLOSED', closed_at = ?2, closed_by = ?3,
                        counted_cash_minor = 0
                  WHERE id = ?1",
                rusqlite::params![first.id, NOW + 1, bar.cashier],
            )
            .unwrap();
        let duplicate = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap_err();
        assert!(
            duplicate.to_string().contains("already traded"),
            "got: {duplicate}"
        );

        let second = open(&bar.conn, &request("2025-08-01", bar.cashier, 0)).unwrap();
        assert_eq!(second.code, "SHIFT-000002");
    }

    #[test]
    fn unresolved_printing_blocks_closing_before_trading_stops() {
        let bar = fixture::bar();
        let shift = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap();
        bar.conn
            .execute(
                "INSERT INTO tabs
                     (code, opened_shift_id, waiter_id, reference_mode, table_no,
                      display_label, opened_at, opened_by)
                 VALUES ('TAB-TEST', ?1, ?2, 'TABLE', '1', 'Table 1', ?3, ?4)",
                rusqlite::params![shift.id, bar.sara, NOW, bar.cashier],
            )
            .unwrap();
        let tab_id = bar.conn.last_insert_rowid();
        bar.conn
            .execute(
                "INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![tab_id, shift.id, bar.sara, bar.cashier, NOW],
            )
            .unwrap();
        let order_id = bar.conn.last_insert_rowid();
        bar.conn
            .execute(
                "UPDATE orders SET status = 'PRINTING' WHERE id = ?1",
                [order_id],
            )
            .unwrap();

        let err = begin_closing(&bar.conn, shift.id, bar.cashier).unwrap_err();
        assert!(err.to_string().contains("print attempt"), "got: {err}");
        assert_eq!(find(&bar.conn, shift.id).unwrap().status, Status::Open);

        bar.conn
            .execute(
                "UPDATE orders SET status = 'DRAFT' WHERE id = ?1",
                [order_id],
            )
            .unwrap();
        assert_eq!(
            begin_closing(&bar.conn, shift.id, bar.cashier)
                .unwrap()
                .status,
            Status::Closing
        );
        let again = begin_closing(&bar.conn, shift.id, bar.cashier).unwrap_err();
        assert!(
            again.to_string().contains("only an open shift"),
            "got: {again}"
        );
    }

    #[test]
    fn owner_cannot_begin_end_of_day() {
        let bar = fixture::bar();
        let shift = open(&bar.conn, &request("2025-07-31", bar.cashier, 0)).unwrap();
        let err = begin_closing(&bar.conn, shift.id, bar.owner).unwrap_err();
        assert!(err.to_string().contains("active cashier"), "got: {err}");
        assert_eq!(find(&bar.conn, shift.id).unwrap().status, Status::Open);
    }
}
