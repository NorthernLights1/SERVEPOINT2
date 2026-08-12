//! Staff records (§0.3, revised by D22).
//!
//! Four roles, but only two of them are *users*: OWNER and CASHIER carry a PIN
//! and log in. A WAITER is a master record — every tab belongs to exactly one,
//! and none of them ever touches the till. BARTENDER exists in the CHECK so
//! that modelling on-duty bartenders later is a data change rather than a
//! schema change; nothing writes it today.

use rusqlite::Connection;

use super::{guarded, RepoError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    Owner,
    Cashier,
    Waiter,
    Bartender,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "OWNER",
            Role::Cashier => "CASHIER",
            Role::Waiter => "WAITER",
            Role::Bartender => "BARTENDER",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "OWNER" => Ok(Role::Owner),
            "CASHIER" => Ok(Role::Cashier),
            "WAITER" => Ok(Role::Waiter),
            "BARTENDER" => Ok(Role::Bartender),
            _ => super::refuse(format!("'{text}' is not a role this build knows")),
        }
    }

    /// Whether this role has a PIN and can therefore be signed in as.
    pub const fn authenticates(self) -> bool {
        matches!(self, Role::Owner | Role::Cashier)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Person {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub role: Role,
    pub active: bool,
}

const COLUMNS: &str = "id, code, full_name, role, active";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Person> {
    let role: String = row.get(3)?;
    Ok(Person {
        id: row.get(0)?,
        code: row.get(1)?,
        name: row.get(2)?,
        // A role outside the CHECK cannot reach this point, so treating an
        // unknown one as a bartender would hide database corruption rather
        // than surface it.
        role: Role::parse(&role).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(format!("unknown role '{role}'"))),
            )
        })?,
        active: row.get::<_, i64>(4)? == 1,
    })
}

pub fn find(conn: &Connection, id: i64) -> Result<Person> {
    conn.query_row(&format!("SELECT {COLUMNS} FROM staff WHERE id = ?1"), [id], read).map_err(
        |err| match err {
            rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what: "person" },
            other => RepoError::Sqlite(other),
        },
    )
}

/// Everyone still on the books, owners first, then cashiers, then waiters —
/// which is the order they appear in every list in the interface.
pub fn active(conn: &Connection) -> Result<Vec<Person>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM staff
          WHERE active = 1
          ORDER BY CASE role WHEN 'OWNER' THEN 0 WHEN 'CASHIER' THEN 1
                             WHEN 'WAITER' THEN 2 ELSE 3 END, full_name"
    ))?;
    let rows = stmt.query_map([], read)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The waiters a tab can be opened against.
pub fn waiters(conn: &Connection) -> Result<Vec<Person>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM staff
          WHERE active = 1 AND role = 'WAITER' ORDER BY full_name"
    ))?;
    let rows = stmt.query_map([], read)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Add someone. Owners and cashiers arrive with a PIN; nobody else may have
/// one, which the schema enforces either way.
pub fn add(
    conn: &Connection,
    code: &str,
    name: &str,
    role: Role,
    pin: Option<(&str, &str)>,
    at: i64,
) -> Result<i64> {
    if role.authenticates() != pin.is_some() {
        return super::refuse(match role {
            Role::Owner | Role::Cashier => "an owner or cashier needs a PIN to sign in with",
            _ => "only owners and cashiers have a PIN",
        });
    }
    let (salt, hash) = pin.unzip();
    guarded!(conn.execute(
        "INSERT INTO staff (code, full_name, role, pin_salt, pin_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![code, name.trim(), role.as_str(), salt, hash, at],
    ))?;
    Ok(conn.last_insert_rowid())
}

/// Take somebody off the floor. Nobody is ever deleted: their name is on
/// receipts, orders and reconciliations that must stay readable forever.
pub fn set_active(conn: &Connection, id: i64, active: bool, at: i64) -> Result<()> {
    // `at` is taken but unused: adding a `deactivated_at` column later should
    // not have to touch every call site.
    let _ = at;
    let changed = guarded!(conn.execute(
        "UPDATE staff SET active = ?2 WHERE id = ?1",
        rusqlite::params![id, i64::from(active)],
    ))?;
    if changed == 0 {
        return Err(RepoError::Missing { what: "person" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture::{self, NOW};

    #[test]
    fn a_waiter_may_not_be_given_a_pin() {
        // A waiter with a PIN implies a login that does not exist, and would
        // put someone who never operates the till into the sign-in list.
        let bar = fixture::bar();
        let err =
            add(&bar.conn, "WTR-9", "Hana", Role::Waiter, Some(("salt", "hash")), NOW).unwrap_err();
        assert!(err.to_string().contains("only owners and cashiers"), "got: {err}");
    }

    #[test]
    fn a_cashier_without_a_pin_is_refused() {
        let bar = fixture::bar();
        let err = add(&bar.conn, "CSH-9", "Meron", Role::Cashier, None, NOW).unwrap_err();
        assert!(err.to_string().contains("needs a PIN"), "got: {err}");
    }

    #[test]
    fn the_last_owner_cannot_be_taken_off_the_books() {
        // D22 puts settings behind the owner role, so removing every owner
        // would lock an offline venue out of its own tax rate for good.
        let bar = fixture::bar();
        let err = set_active(&bar.conn, bar.owner, false, NOW).unwrap_err();
        assert!(err.to_string().contains("last active owner"), "got: {err}");
    }

    #[test]
    fn waiters_are_listed_for_opening_tabs() {
        let bar = fixture::bar();
        let found = waiters(&bar.conn).unwrap();
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Dawit", "Sara"]);
    }

    #[test]
    fn a_deactivated_waiter_disappears_from_the_list_but_not_the_records() {
        let bar = fixture::bar();
        set_active(&bar.conn, bar.sara, false, NOW).unwrap();
        assert_eq!(waiters(&bar.conn).unwrap().len(), 1);
        assert_eq!(find(&bar.conn, bar.sara).unwrap().name, "Sara");
    }
}
