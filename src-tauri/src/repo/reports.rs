//! Frozen shift reports (§9.3).
//!
//! `shift_reports` is append-only, and the schema says why in its own words:
//! a report is reprinted, never rebuilt. There is therefore no update here and
//! no delete — a store and two reads, and nothing else.
//!
//! Both text columns are written together on purpose. `report_json` is the
//! figures as the screen received them; `rendered_text` is the exact paper. A
//! reprint months later reproduces what was signed even if the venue has since
//! changed its name, its currency or its charge rates.

use rusqlite::{Connection, OptionalExtension};

use super::{guarded, RepoError, Result};

#[derive(Clone, Debug)]
pub struct Stored {
    pub id: i64,
    pub shift_id: i64,
    pub is_provisional: bool,
    pub report_json: String,
    pub rendered_text: String,
    pub generated_at: i64,
    pub generated_by: i64,
}

/// Facts frozen exactly once, at the moment a night is sealed.
#[derive(Clone, Debug)]
pub struct NewReport<'a> {
    pub shift_id: i64,
    pub is_provisional: bool,
    pub report_json: &'a str,
    pub rendered_text: &'a str,
    pub generated_at: i64,
    pub generated_by: i64,
}

const COLUMNS: &str =
    "id, shift_id, is_provisional, report_json, rendered_text, generated_at, generated_by";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    Ok(Stored {
        id: row.get(0)?,
        shift_id: row.get(1)?,
        is_provisional: row.get(2)?,
        report_json: row.get(3)?,
        rendered_text: row.get(4)?,
        generated_at: row.get(5)?,
        generated_by: row.get(6)?,
    })
}

/// Freeze a report against a shift, inside the caller's transaction.
///
/// §4.3 makes this and the close one all-or-nothing commit. A night that
/// closed while its report failed to store would have lost its only
/// fraud-control document, so the close has to fail with it.
pub fn store(conn: &Connection, new: &NewReport<'_>) -> Result<Stored> {
    let id: i64 = guarded!(conn.query_row(
        "INSERT INTO shift_reports
             (shift_id, is_provisional, report_json, rendered_text, generated_at, generated_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
      RETURNING id",
        rusqlite::params![
            new.shift_id,
            new.is_provisional,
            new.report_json,
            new.rendered_text,
            new.generated_at,
            new.generated_by,
        ],
        |row| row.get(0),
    ))?;
    find(conn, id)
}

pub fn find(conn: &Connection, id: i64) -> Result<Stored> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM shift_reports WHERE id = ?1"),
        [id],
        read,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing {
            what: "shift report",
        },
        other => RepoError::Sqlite(other),
    })
}

/// The one final report for a night. `None` until the night has closed — the
/// schema's partial unique index is what makes "the one" true.
pub fn final_for(conn: &Connection, shift_id: i64) -> Result<Option<Stored>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM shift_reports
                  WHERE shift_id = ?1 AND is_provisional = 0"
            ),
            [shift_id],
            read,
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, NOW};
    use crate::repo::shifts;
    use crate::Money;

    fn open_night(conn: &Connection, cashier: i64) -> i64 {
        shifts::open(
            conn,
            &shifts::NewShift {
                business_date: "2025-07-31",
                opened_at: NOW,
                opened_by: cashier,
                opening_float: Money::ZERO,
                expected_end_at: NOW + 8 * 60 * 60 * 1_000,
            },
        )
        .unwrap()
        .id
    }

    fn closed_night(conn: &Connection, cashier: i64) -> i64 {
        let id = open_night(conn, cashier);
        shifts::begin_closing(conn, id, cashier).unwrap();
        shifts::close(conn, id, Money::ZERO, cashier, NOW + 1).unwrap();
        id
    }

    fn stored(conn: &Connection, shift_id: i64, by: i64, provisional: bool) -> Result<Stored> {
        store(
            conn,
            &NewReport {
                shift_id,
                is_provisional: provisional,
                report_json: r#"{"totalBilled":"55.00"}"#,
                rendered_text: "SHIFT REPORT\nTOTAL: 55.00\n",
                generated_at: NOW,
                generated_by: by,
            },
        )
    }

    #[test]
    fn a_stored_report_reads_back_byte_for_byte() {
        let bar = fixture::bar();
        let shift = closed_night(&bar.conn, bar.cashier);
        let written = stored(&bar.conn, shift, bar.cashier, false).unwrap();

        let found = final_for(&bar.conn, shift).unwrap().unwrap();
        assert_eq!(found.id, written.id);
        assert_eq!(found.rendered_text, "SHIFT REPORT\nTOTAL: 55.00\n");
        assert_eq!(found.report_json, r#"{"totalBilled":"55.00"}"#);
        assert!(!found.is_provisional);
    }

    #[test]
    fn a_night_that_never_closed_has_no_report_to_read() {
        let bar = fixture::bar();
        let shift = open_night(&bar.conn, bar.cashier);
        assert!(final_for(&bar.conn, shift).unwrap().is_none());
    }

    #[test]
    fn a_second_final_report_for_one_night_is_refused() {
        // The paper the owner signed is the report. Two of them for one night
        // means one is wrong and nobody can say which.
        let bar = fixture::bar();
        let shift = closed_night(&bar.conn, bar.cashier);
        stored(&bar.conn, shift, bar.cashier, false).unwrap();
        assert!(stored(&bar.conn, shift, bar.cashier, false).is_err());
    }

    #[test]
    fn a_final_report_cannot_describe_a_night_still_trading() {
        let bar = fixture::bar();
        let shift = open_night(&bar.conn, bar.cashier);
        let refused = stored(&bar.conn, shift, bar.cashier, false).unwrap_err();
        assert!(
            matches!(&refused, RepoError::Refused(message) if message.contains("closed shift")),
            "{refused:?}"
        );
    }

    #[test]
    fn several_provisional_reports_may_exist_for_one_night() {
        // §9.3: the X-report is this same document run mid-shift. Reading the
        // takings twice before closing must not be an error.
        let bar = fixture::bar();
        let shift = open_night(&bar.conn, bar.cashier);
        stored(&bar.conn, shift, bar.cashier, true).unwrap();
        stored(&bar.conn, shift, bar.cashier, true).unwrap();
        assert!(final_for(&bar.conn, shift).unwrap().is_none());
    }

    #[test]
    fn a_stored_report_can_never_be_edited_or_deleted() {
        let bar = fixture::bar();
        let shift = closed_night(&bar.conn, bar.cashier);
        let written = stored(&bar.conn, shift, bar.cashier, false).unwrap();

        assert!(bar
            .conn
            .execute(
                "UPDATE shift_reports SET rendered_text = 'tampered' WHERE id = ?1",
                [written.id],
            )
            .is_err());
        assert!(bar
            .conn
            .execute("DELETE FROM shift_reports WHERE id = ?1", [written.id])
            .is_err());
    }
}
