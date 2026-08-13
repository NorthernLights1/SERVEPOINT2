//! The repository layer: every statement that touches a table lives here.
//!
//! # Why this layer exists at all
//!
//! The schema already refuses most wrong things — the triggers in
//! `migrations/` are the real enforcement, and nothing here weakens them. What
//! this layer adds is a *vocabulary*: `tabs::open`, `orders::issue`,
//! `stock::post`. The command layer above reads like the business it serves
//! rather than like SQL, and the SQL itself is written once, in one place,
//! where it can be read against the migration that defines the table.
//!
//! # Two rules that hold everywhere below
//!
//! **Nothing here opens a transaction.** Every function takes `&Connection`
//! and assumes the caller has already begun one. This is not laziness: the
//! three-transaction print protocol (§6.3) needs to control exactly where the
//! commits fall, and a repository that quietly committed would destroy it.
//! `ledger::append` follows the same rule for the same reason.
//!
//! **Nothing here formats money.** Functions return `Money`, never `String`.
//! The webview computes nothing, but it is `commands` that turns a figure into
//! text, using the venue's own settings, at the boundary — not here.

pub mod cash;
pub mod catalogue;
pub mod orders;
pub mod purchases;
pub mod receipts;
pub mod reports;
pub mod seq;
pub mod shifts;
pub mod staff;
pub mod stock;
pub mod tabs;

use rusqlite::Error as SqlError;

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] SqlError),

    #[error("settings: {0}")]
    Settings(#[from] crate::settings::SettingsError),

    /// A rule said no, in words meant to be shown to whoever is standing at
    /// the till. Both the schema's triggers and this layer's own checks
    /// produce these.
    #[error("{0}")]
    Refused(String),

    #[error("that {what} no longer exists")]
    Missing { what: &'static str },
}

pub type Result<T> = std::result::Result<T, RepoError>;

/// Refuse something, in a sentence.
pub fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(RepoError::Refused(message.into()))
}

/// Translate a database refusal into a message a person can read.
///
/// Every `RAISE(ABORT, '…')` in the schema is written as a sentence for
/// exactly this moment. SQLite hands the text back on a constraint failure, so
/// the trigger that knows *why* something is forbidden is also the thing that
/// explains it — rather than this layer maintaining a second, drifting copy of
/// the same rules in Rust.
///
/// Anything that is not a constraint failure (a corrupt file, a locked
/// database) stays a `Sqlite` error, because those are not the cashier's
/// problem and must not be dressed up as though they were.
pub fn humanise(err: SqlError) -> RepoError {
    match &err {
        SqlError::SqliteFailure(inner, Some(message))
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            RepoError::Refused(message.clone())
        }
        _ => RepoError::Sqlite(err),
    }
}

/// Run a statement, turning a trigger's refusal into a readable one.
macro_rules! guarded {
    ($expr:expr) => {
        $expr.map_err($crate::repo::humanise)
    };
}
pub(crate) use guarded;

#[cfg(test)]
pub(crate) mod fixture {
    //! A venue with just enough in it to trade.
    //!
    //! Every repository test starts from the same bar: one owner, one cashier,
    //! two waiters, and a small stocked catalogue including a cocktail, so the
    //! recipe expansion is exercised by default rather than in one special
    //! test that could be deleted without anybody noticing.

    use rusqlite::Connection;

    use crate::db;

    pub const NOW: i64 = 1_754_000_000_000; // 2025-08-01, mid-evening UTC

    pub struct Bar {
        pub conn: Connection,
        pub owner: i64,
        pub cashier: i64,
        pub sara: i64,
        pub dawit: i64,
        /// Beer: one base unit per pack, sold as a bottle.
        pub beer: i64,
        pub beer_bottle: i64,
        /// Gin: 24 shots per bottle, sold as a shot and inside a cocktail.
        pub gin: i64,
        pub gin_shot: i64,
        pub gin_tonic: i64,
        pub tonic: i64,
    }

    pub fn bar() -> Bar {
        let conn = db::open_in_memory().unwrap();

        let owner = staff(&conn, "OWN-1", "Selam", "OWNER");
        let cashier = staff(&conn, "CSH-1", "Abel", "CASHIER");
        let sara = staff(&conn, "WTR-1", "Sara", "WAITER");
        let dawit = staff(&conn, "WTR-2", "Dawit", "WAITER");

        let beer = product(&conn, "P-BEER", "Beer", "BOTTLE", 1_000);
        let gin = product(&conn, "P-GIN", "Gin", "SHOT", 24_000);
        let tonic = product(&conn, "P-TONIC", "Tonic", "BOTTLE", 1_000);

        let beer_bottle = sale_item(&conn, "S-BEER", "Beer", "Bottles", 5_000);
        let gin_shot = sale_item(&conn, "S-GIN", "Gin shot", "Shots", 8_000);
        let gin_tonic = sale_item(&conn, "S-GT", "Gin & Tonic", "Cocktails", 15_000);

        recipe(&conn, beer_bottle, &[(beer, 1_000)]);
        recipe(&conn, gin_shot, &[(gin, 1_000)]);
        // Two measures written as two lines, deliberately: §2.5 requires
        // expansion to sum rather than overwrite, and the fixture keeps a case
        // of it permanently in front of the tests.
        recipe(
            &conn,
            gin_tonic,
            &[(gin, 1_000), (gin, 1_000), (tonic, 500)],
        );

        Bar {
            conn,
            owner,
            cashier,
            sara,
            dawit,
            beer,
            beer_bottle,
            gin,
            gin_shot,
            gin_tonic,
            tonic,
        }
    }

    pub fn staff(conn: &Connection, code: &str, name: &str, role: &str) -> i64 {
        let authenticates = matches!(role, "OWNER" | "CASHIER");
        conn.execute(
            "INSERT INTO staff (code, full_name, role, pin_hash, pin_salt, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                code,
                name,
                role,
                authenticates.then_some("hash"),
                authenticates.then_some("salt"),
                NOW
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    pub fn product(conn: &Connection, code: &str, name: &str, unit: &str, per_pack: i64) -> i64 {
        conn.execute(
            "INSERT INTO products (code, name, base_unit, base_units_per_pack,
                                   low_stock_threshold_milli, avg_cost_minor, created_at)
             VALUES (?1, ?2, ?3, ?4, 3000, 10000, ?5)",
            rusqlite::params![code, name, unit, per_pack, NOW],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    pub fn sale_item(conn: &Connection, code: &str, name: &str, category: &str, price: i64) -> i64 {
        conn.execute(
            "INSERT INTO sale_items (code, name, category, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![code, name, category, NOW],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO prices (sale_item_id, price_minor, effective_from) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, price, NOW],
        )
        .unwrap();
        id
    }

    pub fn recipe(conn: &Connection, sale_item_id: i64, lines: &[(i64, i64)]) -> i64 {
        conn.execute(
            "INSERT INTO recipes (sale_item_id, version, effective_from) VALUES (?1, 1, ?2)",
            rusqlite::params![sale_item_id, NOW],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        for (product_id, quantity) in lines {
            conn.execute(
                "INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id, product_id, quantity],
            )
            .unwrap();
        }
        id
    }

    /// Put stock on the shelf without going through a purchase — the
    /// equivalent of the opening count on the day the till is installed.
    pub fn stock_up(conn: &Connection, product_id: i64, milli: i64, by: i64) {
        conn.execute(
            "INSERT INTO stock_movements
                 (product_id, movement_type, quantity_milli, reason, created_at, created_by)
             VALUES (?1, 'STOCK_CORRECTION', ?2, 'opening count', ?3, ?4)",
            rusqlite::params![product_id, milli, NOW, by],
        )
        .unwrap();
    }
}
