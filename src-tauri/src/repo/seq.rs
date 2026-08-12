//! Human-facing numbers (§1.4).
//!
//! Four counters, four prefixes: `SHIFT-000001`, `TAB-000421`, `BR-000123`,
//! `CR-000045`. The number and the row that consumes it are allocated in the
//! same statement, inside the caller's transaction — read-then-write would
//! leave a window where two customers walk out holding paper with the same
//! receipt number on it.

use rusqlite::Connection;

use super::{guarded, Result};

/// Zero-padding width. A number that outgrows it simply gets wider — padding
/// is for reading, not for identity, and wrapping back to zero would reuse a
/// receipt number that a customer is still holding.
pub const WIDTH: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    Tab,
    Shift,
    IssueReceipt,
    CustomerReceipt,
}

impl Counter {
    /// The `sequences.name` this counter lives under. The column has a CHECK
    /// listing exactly these four, so a typo here fails loudly at the first
    /// allocation rather than silently creating a fifth counter.
    pub const fn row(self) -> &'static str {
        match self {
            Counter::Tab => "TAB",
            Counter::Shift => "SHIFT",
            Counter::IssueReceipt => "ISSUE_RECEIPT",
            Counter::CustomerReceipt => "CUSTOMER_RECEIPT",
        }
    }

    pub const fn prefix(self) -> &'static str {
        match self {
            Counter::Tab => "TAB-",
            Counter::Shift => "SHIFT-",
            Counter::IssueReceipt => "BR-",
            Counter::CustomerReceipt => "CR-",
        }
    }
}

/// Take the next number, returning both the raw sequence value and the printed
/// form. Both are stored: reports order by the integer, humans read the text.
pub fn next(conn: &Connection, counter: Counter) -> Result<(i64, String)> {
    let value: i64 = guarded!(conn.query_row(
        "UPDATE sequences
            SET next_value = next_value + 1
          WHERE name = ?1
      RETURNING next_value - 1",
        [counter.row()],
        |row| row.get(0),
    ))?;
    Ok((value, format(counter, value)))
}

pub fn format(counter: Counter, value: i64) -> String {
    format!("{}{:0width$}", counter.prefix(), value, width = WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn a_number_is_never_handed_out_twice() {
        let conn = db::open_in_memory().unwrap();
        let (first, _) = next(&conn, Counter::IssueReceipt).unwrap();
        let (second, _) = next(&conn, Counter::IssueReceipt).unwrap();
        assert_eq!((first, second), (1, 2));
    }

    #[test]
    fn the_four_counters_are_independent() {
        // A busy night must not push tab numbers along because receipts were
        // printed — they are different documents and get counted separately.
        let conn = db::open_in_memory().unwrap();
        next(&conn, Counter::IssueReceipt).unwrap();
        next(&conn, Counter::IssueReceipt).unwrap();
        let (tab, printed) = next(&conn, Counter::Tab).unwrap();
        assert_eq!((tab, printed.as_str()), (1, "TAB-000001"));
    }

    #[test]
    fn numbers_are_padded_for_reading() {
        assert_eq!(format(Counter::Shift, 1), "SHIFT-000001");
        assert_eq!(format(Counter::CustomerReceipt, 45), "CR-000045");
    }

    #[test]
    fn a_number_past_the_padding_gets_wider_rather_than_wrapping() {
        // After a million slips the prefix must still identify one unique
        // document. Truncating to six digits would reissue BR-000001.
        assert_eq!(format(Counter::IssueReceipt, 1_234_567), "BR-1234567");
    }
}
