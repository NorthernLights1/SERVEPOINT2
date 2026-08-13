//! Opening the database and applying migrations (§11, §13).
//!
//! # Why the migration list is written out by hand
//!
//! The files are embedded with `include_str!` and listed explicitly below,
//! never discovered by scanning a directory. A packaged desktop build has no
//! `migrations/` directory to scan — the SQL has to be inside the binary — and
//! a scan that silently finds nothing would start the till against an empty
//! database. An explicit list also fails at COMPILE time when somebody adds a
//! file and forgets to register it, rather than on the night it matters.
//!
//! # Two pragmas that are not optional
//!
//! `foreign_keys` defaults to **OFF** in SQLite and silently does nothing if
//! forgotten, which would quietly turn every `REFERENCES` clause in this
//! schema into a comment. `journal_mode = WAL` keeps a reader from blocking
//! the writer, which matters when the reports screen queries a night that is
//! still trading.

use std::path::Path;

use rusqlite::Connection;

/// Every migration, in the order they must be applied. Adding a file here is
/// the only way it gets applied.
pub const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "0001_core.sql",
        include_str!("../migrations/0001_core.sql"),
    ),
    (
        2,
        "0002_catalogue.sql",
        include_str!("../migrations/0002_catalogue.sql"),
    ),
    (
        3,
        "0003_shifts_tabs.sql",
        include_str!("../migrations/0003_shifts_tabs.sql"),
    ),
    (
        4,
        "0004_orders.sql",
        include_str!("../migrations/0004_orders.sql"),
    ),
    (
        5,
        "0005_receipts.sql",
        include_str!("../migrations/0005_receipts.sql"),
    ),
    (
        6,
        "0006_purchasing.sql",
        include_str!("../migrations/0006_purchasing.sql"),
    ),
    (
        7,
        "0007_inventory.sql",
        include_str!("../migrations/0007_inventory.sql"),
    ),
    (
        8,
        "0008_money.sql",
        include_str!("../migrations/0008_money.sql"),
    ),
    (
        9,
        "0009_audit.sql",
        include_str!("../migrations/0009_audit.sql"),
    ),
    (
        10,
        "0010_repository_hardening.sql",
        include_str!("../migrations/0010_repository_hardening.sql"),
    ),
    (
        11,
        "0011_backup_reconciliation_hardening.sql",
        include_str!("../migrations/0011_backup_reconciliation_hardening.sql"),
    ),
    (
        12,
        "0012_generated_codes.sql",
        include_str!("../migrations/0012_generated_codes.sql"),
    ),
    (
        13,
        "0013_items.sql",
        include_str!("../migrations/0013_items.sql"),
    ),
    (
        14,
        "0014_measures.sql",
        include_str!("../migrations/0014_measures.sql"),
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error(
        "this database was created by a newer version of ServePoint \
         (schema {found}, this build knows {known}). Install the newer version \
         rather than downgrading — an older build would not understand the data."
    )]
    FromTheFuture { found: i64, known: i64 },

    #[error("migration {name} failed: {source}")]
    Migration {
        name: &'static str,
        #[source]
        source: rusqlite::Error,
    },
}

type Result<T> = std::result::Result<T, DbError>;

/// Open the venue's database, applying any migrations it has not seen.
pub fn open(path: impl AsRef<Path>) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// An in-memory database with the full schema — for tests, and for the
/// throwaway database the catalogue importer validates against.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    run_migrations(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL is a persistent property of the file, so this is a no-op after the
    // first open; setting it every time costs nothing and means a database
    // restored from a copy is never left in rollback-journal mode.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // NORMAL is the documented safe pairing with WAL: durable across an
    // application crash, and only at risk from a power cut mid-commit — which
    // the two-transaction print protocol is designed to survive anyway.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// Apply every migration this database has not seen, each in its own
/// transaction so a failure leaves the schema at the last good version rather
/// than half-applied.
pub fn run_migrations(conn: &Connection) -> Result<()> {
    let bootstrapped = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1;

    let applied: i64 = if bootstrapped {
        conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?
    } else {
        0
    };

    let known = MIGRATIONS.last().map_or(0, |(version, _, _)| *version);
    if applied > known {
        // Refuse rather than "upgrade" downwards. An older build writing to a
        // newer schema is how data gets silently mangled.
        return Err(DbError::FromTheFuture {
            found: applied,
            known,
        });
    }

    let now = chrono::Utc::now().timestamp_millis();
    for (version, name, sql) in MIGRATIONS {
        if *version <= applied {
            continue;
        }
        conn.execute_batch("BEGIN")?;
        let outcome = conn.execute_batch(sql).and_then(|()| {
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![version, name, now],
            )
            .map(|_| ())
        });
        match outcome {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(source) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(DbError::Migration { name, source });
            }
        }
    }
    Ok(())
}

/// The schema version currently applied.
pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Derived rather than written out, so adding a migration does not mean
    /// hunting down five assertions that hardcoded the previous number.
    const LATEST: i64 = MIGRATIONS[MIGRATIONS.len() - 1].0;

    #[test]
    fn a_fresh_database_gets_the_whole_schema() {
        let conn = open_in_memory().expect("migrations should apply cleanly");
        assert_eq!(schema_version(&conn).unwrap(), LATEST);
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        // The till applies migrations on every start; the second start must be
        // a no-op rather than a re-run that trips a UNIQUE constraint.
        let conn = open_in_memory().unwrap();
        run_migrations(&conn).expect("a second run must be a no-op");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64);
    }

    #[test]
    fn a_version_nine_database_upgrades_in_place_without_rewriting_venue_data() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        for (version, name, sql) in &MIGRATIONS[..9] {
            conn.execute_batch("BEGIN").unwrap();
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at)
                 VALUES (?1, ?2, 0)",
                rusqlite::params![version, name],
            )
            .unwrap();
            conn.execute_batch("COMMIT").unwrap();
        }
        conn.execute(
            "UPDATE settings SET value = 'Upgrade Test Venue'
              WHERE key = 'receipt.business_name'",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), LATEST);
        let venue: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'receipt.business_name'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(venue, "Upgrade Test Venue");
        let has_customer_tin = conn
            .prepare("PRAGMA table_info(receipts)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .iter()
            .any(|column| column == "customer_tin");
        assert!(has_customer_tin);
    }

    /// The renumber has to survive a venue that already typed its own codes,
    /// including one that happens to look like a generated one — `code` is
    /// UNIQUE, so a single-pass rewrite could collide with a row it had not
    /// reached yet.
    #[test]
    fn a_version_eleven_database_has_its_hand_typed_codes_renumbered() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        for (version, name, sql) in &MIGRATIONS[..11] {
            conn.execute_batch("BEGIN").unwrap();
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at)
                 VALUES (?1, ?2, 0)",
                rusqlite::params![version, name],
            )
            .unwrap();
            conn.execute_batch("COMMIT").unwrap();
        }
        // The second product already holds the code the first one is about to
        // be given.
        conn.execute_batch(
            "INSERT INTO products (code, name, category, base_unit, base_units_per_pack,
                                   units_per_purchase_pack, low_stock_threshold_milli,
                                   tracks_inventory, destination, created_at)
             VALUES ('W-001', 'Johnnie Walker', 'Whiskey', 'BOTTLE', 1000, 24, 3000, 1, 'BAR', 1),
                    ('PRD-000001', 'Gin', 'Spirits', 'BOTTLE', 1000, 12, 3000, 1, 'BAR', 2);
             INSERT INTO staff (code, full_name, role, pin_hash, pin_salt, created_at)
             VALUES ('OWNER-1', 'Selam', 'OWNER', 'hash', 'salt', 1);",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let codes: Vec<String> = conn
            .prepare("SELECT code FROM products ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(codes, ["PRD-000001", "PRD-000002"]);
        let owner: String = conn
            .query_row("SELECT code FROM staff", [], |row| row.get(0))
            .unwrap();
        assert_eq!(owner, "STF-000001");

        // The counters continue past what was just assigned rather than
        // handing a live row's code to the next one added.
        let next: Vec<(String, i64)> = conn
            .prepare("SELECT name, next_value FROM sequences WHERE name IN ('PRODUCT','STAFF') ORDER BY name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(next, [("PRODUCT".to_owned(), 3), ("STAFF".to_owned(), 2)]);
    }

    #[test]
    fn a_version_ten_database_keeps_backup_history_while_accepting_honest_failures() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        for (version, name, sql) in &MIGRATIONS[..10] {
            conn.execute_batch("BEGIN").unwrap();
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at)
                 VALUES (?1, ?2, 0)",
                rusqlite::params![version, name],
            )
            .unwrap();
            conn.execute_batch("COMMIT").unwrap();
        }
        conn.execute(
            "INSERT INTO backups
                 (target_path, size_bytes, outcome, detail, created_at)
             VALUES ('/media/usb/old.db', 4096, 'VERIFIED', '', 1)",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), LATEST);
        let old: (String, i64, String) = conn
            .query_row(
                "SELECT target_path, size_bytes, outcome FROM backups WHERE created_at = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(old, ("/media/usb/old.db".into(), 4096, "VERIFIED".into()));
        conn.execute(
            "INSERT INTO backups
                 (target_path, size_bytes, outcome, detail, created_at)
             VALUES ('/media/usb/missing.db', 0, 'FAILED', 'not mounted', 2)",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO backups
                     (target_path, size_bytes, outcome, detail, created_at)
                 VALUES ('/media/usb/empty.db', 0, 'VERIFIED', '', 3)",
                [],
            )
            .is_err());
    }

    #[test]
    fn foreign_keys_are_actually_on() {
        // They default to OFF and silently do nothing if forgotten, which
        // would turn every REFERENCES clause in the schema into a comment.
        let conn = open_in_memory().unwrap();
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(on, 1);
    }

    #[test]
    fn the_settings_ship_configured_for_no_particular_venue() {
        // D8: a club that never fills these in prints a headerless receipt,
        // which is visibly wrong — better than silently printing somebody
        // else's name and TIN onto a fiscal document.
        let conn = open_in_memory().unwrap();
        let name: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'receipt.business_name'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "");
    }

    #[test]
    fn there_is_no_setting_that_allows_negative_stock() {
        // D9, revised: insufficient stock ALWAYS blocks the sale, and there is
        // deliberately no key to turn that off.
        let conn = open_in_memory().unwrap();
        let strays: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM settings WHERE key LIKE 'inventory.%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(strays, 0);
    }

    #[test]
    fn the_append_only_triggers_are_live() {
        // A spot check that the schema's guarantees survive the round trip
        // through include_str! and the migration runner.
        let conn = open_in_memory().unwrap();
        let err = conn
            .execute("DELETE FROM settings WHERE key = 'tax.enabled'", [])
            .unwrap_err();
        assert!(err.to_string().contains("never deleted"), "got: {err}");
    }

    #[test]
    fn a_newer_database_is_refused_rather_than_downgraded() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'future', 0)",
            [],
        )
        .unwrap();
        match run_migrations(&conn) {
            Err(DbError::FromTheFuture { found, known }) => {
                assert_eq!((found, known), (99, LATEST));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
