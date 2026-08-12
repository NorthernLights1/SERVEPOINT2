//! Writing and checking the audit log (§10).
//!
//! [`crate::audit`] knows how to hash a row and how to walk a chain. This
//! module is the half that touches the database: it reads the tail of the
//! chain, computes the next link, and inserts it.
//!
//! # Append inside the caller's transaction, always
//!
//! Nothing here opens a transaction. An audit entry is written in the same
//! commit as the thing it describes, so it is impossible to end up with a void
//! that was never logged, or a log line for a void that rolled back. If this
//! module opened its own transaction that guarantee would quietly disappear.
//!
//! # Two writers cannot race
//!
//! The next sequence number is read and written in one transaction, and the
//! schema's `audit_log_chain_intact` trigger rejects any row that does not
//! extend the tail exactly. On the second writer the insert aborts and the
//! whole operation rolls back rather than forking the chain. A till has one
//! user at a time, so this costs nothing in practice and removes a whole class
//! of "the log verifies on my machine" bug.

use rusqlite::Connection;

use crate::audit::{self, AuditEntry, ChainStatus, GENESIS_HASH};

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

type Result<T> = std::result::Result<T, LedgerError>;

/// Something worth remembering happened.
///
/// Built with the chained setters below so a call site reads as a sentence and
/// an omitted field is visibly absent rather than a stray `None` in a long
/// argument list.
#[derive(Debug, Clone)]
pub struct Event<'a> {
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: Option<i64>,
    pub old_value: Option<&'a str>,
    pub new_value: Option<&'a str>,
    pub staff_id: Option<i64>,
    pub shift_id: Option<i64>,
    pub at: i64,
}

impl<'a> Event<'a> {
    pub fn new(action: &'a str, entity_type: &'a str, at: i64) -> Self {
        Self {
            action,
            entity_type,
            entity_id: None,
            old_value: None,
            new_value: None,
            staff_id: None,
            shift_id: None,
            at,
        }
    }

    pub fn about(mut self, entity_id: i64) -> Self {
        self.entity_id = Some(entity_id);
        self
    }

    pub fn changed(mut self, old: &'a str, new: &'a str) -> Self {
        self.old_value = Some(old);
        self.new_value = Some(new);
        self
    }

    pub fn recording(mut self, new: &'a str) -> Self {
        self.new_value = Some(new);
        self
    }

    pub fn by(mut self, staff_id: i64) -> Self {
        self.staff_id = Some(staff_id);
        self
    }

    pub fn during(mut self, shift_id: Option<i64>) -> Self {
        self.shift_id = shift_id;
        self
    }
}

/// Add one link to the chain. Returns its sequence number.
pub fn append(conn: &Connection, event: &Event<'_>) -> Result<i64> {
    let (last_seq, prev_hash): (i64, String) = conn.query_row(
        "SELECT COALESCE(MAX(sequence_no), 0),
                COALESCE((SELECT row_hash FROM audit_log ORDER BY sequence_no DESC LIMIT 1), ?1)
           FROM audit_log",
        [GENESIS_HASH],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let sequence_no = last_seq + 1;
    let entry = AuditEntry {
        sequence_no,
        staff_id: event.staff_id,
        action: event.action,
        entity_type: event.entity_type,
        entity_id: event.entity_id,
        old_value: event.old_value,
        new_value: event.new_value,
        shift_id: event.shift_id,
        created_at: event.at,
        prev_hash: &prev_hash,
    };
    let row_hash = entry.row_hash();

    conn.execute(
        "INSERT INTO audit_log
             (sequence_no, staff_id, action, entity_type, entity_id,
              old_value, new_value, shift_id, created_at, prev_hash, row_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            sequence_no,
            event.staff_id,
            event.action,
            event.entity_type,
            event.entity_id,
            event.old_value,
            event.new_value,
            event.shift_id,
            event.at,
            prev_hash,
            row_hash,
        ],
    )?;
    Ok(sequence_no)
}

/// Re-hash the whole log and report the first row that does not add up.
///
/// Reads in `sequence_no` order and holds one row at a time, so a log with a
/// year of trading in it verifies without loading the year into memory.
pub fn verify(conn: &Connection) -> Result<ChainStatus> {
    let mut statement = conn.prepare(
        "SELECT sequence_no, staff_id, action, entity_type, entity_id,
                old_value, new_value, shift_id, created_at, prev_hash, row_hash
           FROM audit_log ORDER BY sequence_no",
    )?;
    let mut rows = statement.query([])?;

    let mut expected_seq = 1i64;
    let mut running = GENESIS_HASH.to_owned();
    let mut count = 0usize;

    while let Some(row) = rows.next()? {
        let sequence_no: i64 = row.get(0)?;
        let action: String = row.get(2)?;
        let entity_type: String = row.get(3)?;
        let old_value: Option<String> = row.get(5)?;
        let new_value: Option<String> = row.get(6)?;
        let prev_hash: String = row.get(9)?;
        let stored_hash: String = row.get(10)?;

        if sequence_no != expected_seq {
            return Ok(ChainStatus::Broken {
                sequence_no,
                reason: audit::BreakReason::SequenceGap { expected: expected_seq },
            });
        }
        if prev_hash != running {
            return Ok(ChainStatus::Broken {
                sequence_no,
                reason: audit::BreakReason::PrevHashMismatch,
            });
        }

        let entry = AuditEntry {
            sequence_no,
            staff_id: row.get(1)?,
            action: &action,
            entity_type: &entity_type,
            entity_id: row.get(4)?,
            old_value: old_value.as_deref(),
            new_value: new_value.as_deref(),
            shift_id: row.get(7)?,
            created_at: row.get(8)?,
            prev_hash: &prev_hash,
        };
        let recomputed = entry.row_hash();
        if recomputed != stored_hash {
            return Ok(ChainStatus::Broken {
                sequence_no,
                reason: audit::BreakReason::ContentAltered,
            });
        }

        running = recomputed;
        expected_seq += 1;
        count += 1;
    }

    Ok(ChainStatus::Intact { rows: count })
}

/// The shift currently trading, if any.
///
/// Every audit entry wants it and forgetting it silently detaches an event
/// from the night it happened on, so it is one call rather than a fragment of
/// SQL copied into each caller.
pub fn open_shift_id(conn: &Connection) -> Result<Option<i64>> {
    let mut statement = conn.prepare("SELECT id FROM shifts WHERE status = 'OPEN' LIMIT 1")?;
    let mut rows = statement.query([])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Cryptographic random bytes, hex encoded — used for PIN salts.
///
/// Taken from SQLite's own `randomblob`, which is seeded from the operating
/// system. Doing it this way keeps a random-number crate out of the dependency
/// tree of a till that is otherwise entirely offline.
pub fn random_hex(conn: &Connection, bytes: u32) -> Result<String> {
    Ok(conn.query_row("SELECT LOWER(HEX(randomblob(?1)))", [bytes], |row| row.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const T0: i64 = 1_786_500_000_000;

    fn fresh() -> Connection {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn an_empty_log_verifies() {
        assert_eq!(verify(&fresh()).unwrap(), ChainStatus::Intact { rows: 0 });
    }

    #[test]
    fn entries_chain_and_verify() {
        let conn = fresh();
        append(&conn, &Event::new("SETTING_CHANGED", "settings", T0).changed("0", "1")).unwrap();
        append(&conn, &Event::new("SETTING_CHANGED", "settings", T0 + 1).changed("1500", "1000"))
            .unwrap();
        append(&conn, &Event::new("SHIFT_OPENED", "shift", T0 + 2).about(1)).unwrap();
        assert_eq!(verify(&conn).unwrap(), ChainStatus::Intact { rows: 3 });
    }

    #[test]
    fn sequence_numbers_are_dense_and_start_at_one() {
        let conn = fresh();
        let first = append(&conn, &Event::new("A", "thing", T0)).unwrap();
        let second = append(&conn, &Event::new("B", "thing", T0)).unwrap();
        assert_eq!((first, second), (1, 2));
    }

    #[test]
    fn the_first_row_points_at_genesis() {
        let conn = fresh();
        append(&conn, &Event::new("SHIFT_OPENED", "shift", T0)).unwrap();
        let prev: String = conn
            .query_row("SELECT prev_hash FROM audit_log WHERE sequence_no = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(prev, GENESIS_HASH);
    }

    #[test]
    fn an_edit_made_behind_the_applications_back_is_caught() {
        // The whole point. The triggers stop ServePoint rewriting history; the
        // chain catches everyone else, including a SQLite browser. The UPDATE
        // trigger has to be dropped to simulate that, which is exactly what
        // somebody with the file would do.
        let conn = fresh();
        append(&conn, &Event::new("SHIFT_OPENED", "shift", T0).about(1)).unwrap();
        append(&conn, &Event::new("ORDER_VOIDED", "order", T0 + 1).about(7)).unwrap();
        append(&conn, &Event::new("SHIFT_CLOSED", "shift", T0 + 2).about(1)).unwrap();

        conn.execute_batch("DROP TRIGGER audit_log_no_update").unwrap();
        conn.execute("UPDATE audit_log SET action = 'ORDER_ISSUED' WHERE sequence_no = 2", [])
            .unwrap();

        assert_eq!(
            verify(&conn).unwrap(),
            ChainStatus::Broken {
                sequence_no: 2,
                reason: audit::BreakReason::ContentAltered
            }
        );
    }

    #[test]
    fn a_deleted_row_leaves_a_gap_that_verification_names() {
        let conn = fresh();
        append(&conn, &Event::new("A", "thing", T0)).unwrap();
        append(&conn, &Event::new("B", "thing", T0 + 1)).unwrap();
        append(&conn, &Event::new("C", "thing", T0 + 2)).unwrap();

        conn.execute_batch("DROP TRIGGER audit_log_no_delete").unwrap();
        conn.execute("DELETE FROM audit_log WHERE sequence_no = 2", []).unwrap();

        assert_eq!(
            verify(&conn).unwrap(),
            ChainStatus::Broken {
                sequence_no: 3,
                reason: audit::BreakReason::SequenceGap { expected: 2 }
            }
        );
    }

    #[test]
    fn the_schema_refuses_a_row_that_does_not_extend_the_tail() {
        // Belt and braces: even if this module computed the wrong link, the
        // database would not store it.
        let conn = fresh();
        append(&conn, &Event::new("A", "thing", T0)).unwrap();
        let err = conn
            .execute(
                "INSERT INTO audit_log
                     (sequence_no, action, entity_type, created_at, prev_hash, row_hash)
                 VALUES (5, 'FORGED', 'thing', ?1, ?2, ?2)",
                rusqlite::params![T0, "a".repeat(64)],
            )
            .unwrap_err();
        assert!(err.to_string().contains("chain must be extended"), "got: {err}");
    }

    #[test]
    fn a_shift_is_recorded_when_one_is_trading() {
        let conn = fresh();
        assert_eq!(open_shift_id(&conn).unwrap(), None);
    }

    #[test]
    fn salts_are_random_and_the_right_length() {
        let conn = fresh();
        let a = random_hex(&conn, 16).unwrap();
        let b = random_hex(&conn, 16).unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert_ne!(a, b, "two salts in a row must not be identical");
    }
}
