//! Long-lived customer tabs (section 5).
//!
//! A tab keeps both forms of identity: a sequential internal code and the
//! reference mode/fields/label frozen when it opened.  Tabs deliberately
//! outlive shifts.  Transferring one changes only who is responsible for the
//! open tab; issued orders retain the waiter who actually sold them.
//!
//! [`open`] and [`transfer`] are multi-statement business operations and, like
//! every repository write, expect the caller to own the surrounding
//! transaction.

use rusqlite::Connection;

use super::{guarded, shifts, staff, RepoError, Result};
use crate::Money;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceMode {
    Table,
    CustomerName,
    CustomerPhone,
    Custom,
}

impl ReferenceMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            ReferenceMode::Table => "TABLE",
            ReferenceMode::CustomerName => "CUSTOMER_NAME",
            ReferenceMode::CustomerPhone => "CUSTOMER_PHONE",
            ReferenceMode::Custom => "CUSTOM",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "TABLE" => Ok(ReferenceMode::Table),
            "CUSTOMER_NAME" => Ok(ReferenceMode::CustomerName),
            "CUSTOMER_PHONE" => Ok(ReferenceMode::CustomerPhone),
            "CUSTOM" => Ok(ReferenceMode::Custom),
            _ => super::refuse(format!(
                "'{text}' is not a tab reference mode this build knows"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Closed,
    Reconciled,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Open => "OPEN",
            Status::Closed => "CLOSED",
            Status::Reconciled => "RECONCILED",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "OPEN" => Ok(Status::Open),
            "CLOSED" => Ok(Status::Closed),
            "RECONCILED" => Ok(Status::Reconciled),
            _ => super::refuse(format!("'{text}' is not a tab status this build knows")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: i64,
    pub code: String,
    pub opened_shift_id: i64,
    pub waiter_id: i64,
    pub reference_mode: ReferenceMode,
    pub table_no: Option<String>,
    pub customer_name: Option<String>,
    pub customer_phone: Option<String>,
    pub custom_ref: Option<String>,
    pub display_label: String,
    pub customer_tin: Option<String>,
    pub status: Status,
    pub opened_at: i64,
    pub opened_by: i64,
    pub closed_shift_id: Option<i64>,
    pub closed_at: Option<i64>,
    pub closed_by: Option<i64>,
    pub is_comped: bool,
}

/// The raw reference collected at open.  Separate fields are intentional:
/// searches never have to reinterpret an old tab using today's mode.
#[derive(Clone, Debug)]
pub struct Reference<'a> {
    pub mode: ReferenceMode,
    pub table_no: Option<&'a str>,
    pub customer_name: Option<&'a str>,
    pub customer_phone: Option<&'a str>,
    pub custom_ref: Option<&'a str>,
    pub customer_tin: Option<&'a str>,
}

impl<'a> Reference<'a> {
    pub const fn table(table_no: &'a str) -> Self {
        Self {
            mode: ReferenceMode::Table,
            table_no: Some(table_no),
            customer_name: None,
            customer_phone: None,
            custom_ref: None,
            customer_tin: None,
        }
    }

    pub const fn customer_name(name: &'a str) -> Self {
        Self {
            mode: ReferenceMode::CustomerName,
            table_no: None,
            customer_name: Some(name),
            customer_phone: None,
            custom_ref: None,
            customer_tin: None,
        }
    }

    pub const fn customer_phone(name: &'a str, phone: Option<&'a str>) -> Self {
        Self {
            mode: ReferenceMode::CustomerPhone,
            table_no: None,
            customer_name: Some(name),
            customer_phone: phone,
            custom_ref: None,
            customer_tin: None,
        }
    }

    pub const fn custom(custom_ref: &'a str) -> Self {
        Self {
            mode: ReferenceMode::Custom,
            table_no: None,
            customer_name: None,
            customer_phone: None,
            custom_ref: Some(custom_ref),
            customer_tin: None,
        }
    }

    pub const fn with_customer_tin(mut self, tin: &'a str) -> Self {
        self.customer_tin = Some(tin);
        self
    }
}

#[derive(Clone, Debug)]
pub struct NewTab<'a> {
    pub opened_shift_id: i64,
    pub waiter_id: i64,
    pub reference: Reference<'a>,
    pub opened_at: i64,
    pub opened_by: i64,
}

#[derive(Clone, Debug)]
pub struct Transfer<'a> {
    pub tab_id: i64,
    pub to_waiter_id: i64,
    pub shift_id: i64,
    pub transferred_at: i64,
    pub transferred_by: i64,
    pub reason: &'a str,
}

const COLUMNS: &str = "id, code, opened_shift_id, waiter_id, reference_mode,
     table_no, customer_name, customer_phone, custom_ref, display_label, customer_tin,
     status, opened_at, opened_by, closed_shift_id, closed_at, closed_by, is_comped";
const QUALIFIED_COLUMNS: &str = "t.id, t.code, t.opened_shift_id, t.waiter_id, t.reference_mode,
     t.table_no, t.customer_name, t.customer_phone, t.custom_ref, t.display_label, t.customer_tin,
     t.status, t.opened_at, t.opened_by, t.closed_shift_id, t.closed_at, t.closed_by, t.is_comped";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tab> {
    let reference_mode: String = row.get(4)?;
    let reference_mode = ReferenceMode::parse(&reference_mode).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown tab reference mode '{reference_mode}'"
            ))),
        )
    })?;
    let status: String = row.get(11)?;
    let status = Status::parse(&status).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown tab status '{status}'"
            ))),
        )
    })?;
    Ok(Tab {
        id: row.get(0)?,
        code: row.get(1)?,
        opened_shift_id: row.get(2)?,
        waiter_id: row.get(3)?,
        reference_mode,
        table_no: row.get(5)?,
        customer_name: row.get(6)?,
        customer_phone: row.get(7)?,
        custom_ref: row.get(8)?,
        display_label: row.get(9)?,
        customer_tin: row.get(10)?,
        status,
        opened_at: row.get(12)?,
        opened_by: row.get(13)?,
        closed_shift_id: row.get(14)?,
        closed_at: row.get(15)?,
        closed_by: row.get(16)?,
        is_comped: row.get::<_, i64>(17)? == 1,
    })
}

pub fn find(conn: &Connection, id: i64) -> Result<Tab> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM tabs WHERE id = ?1"),
        [id],
        read,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => RepoError::Missing { what: "tab" },
        other => RepoError::Sqlite(other),
    })
}

/// Open a tab against the active business night, freezing its identity.
pub fn open(conn: &Connection, new: &NewTab<'_>) -> Result<Tab> {
    require_cashier(conn, new.opened_by)?;
    require_waiter(conn, new.waiter_id)?;
    let shift = shifts::find(conn, new.opened_shift_id)?;
    if shift.status != shifts::Status::Open {
        return super::refuse("a tab can only be opened in an open shift");
    }

    let reference = normalise(&new.reference)?;
    let duplicate: bool = conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM tabs
              WHERE status = 'OPEN' AND display_label = ?1 COLLATE NOCASE
         )",
        [&reference.display_label],
        |row| row.get(0),
    )?;
    if duplicate {
        return super::refuse(format!(
            "an open tab already uses the reference '{}'",
            reference.display_label
        ));
    }

    let (_, code) = super::seq::next(conn, super::seq::Counter::Tab)?;
    guarded!(conn.execute(
        "INSERT INTO tabs
             (code, opened_shift_id, waiter_id, reference_mode,
              table_no, customer_name, customer_phone, custom_ref,
              display_label, customer_tin, opened_at, opened_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            code,
            new.opened_shift_id,
            new.waiter_id,
            new.reference.mode.as_str(),
            reference.table_no,
            reference.customer_name,
            reference.customer_phone,
            reference.custom_ref,
            reference.display_label,
            reference.customer_tin,
            new.opened_at,
            new.opened_by,
        ],
    ))?;
    find(conn, conn.last_insert_rowid())
}

/// Search every stored reference, the waiter, and the internal code.
///
/// `%`, `_`, and `\\` in the query are literals rather than accidental SQL
/// wildcards.  Closed results remain searchable for support/history, with open
/// tabs shown first.
pub fn search(conn: &Connection, query: &str) -> Result<Vec<Tab>> {
    let query = query.trim();
    if query.is_empty() {
        return super::refuse("type a tab reference, waiter, or tab code to search");
    }
    let pattern = like_pattern(query);
    let mut stmt = conn.prepare(&format!(
        "SELECT {QUALIFIED_COLUMNS}
           FROM tabs t
           JOIN staff w ON w.id = t.waiter_id
          WHERE t.code           LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR t.display_label  LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR t.table_no       LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR t.customer_name  LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR t.customer_phone LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR t.custom_ref     LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR w.code           LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             OR w.full_name      LIKE ?1 ESCAPE '\\' COLLATE NOCASE
          ORDER BY CASE t.status WHEN 'OPEN' THEN 0 WHEN 'CLOSED' THEN 1 ELSE 2 END,
                   t.opened_at DESC, t.id DESC"
    ))?;
    let rows = stmt.query_map([pattern], read)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Sum only issued order lines.  Drafts and every terminal correction state
/// are deliberately absent from the predicate.
pub fn running_total(conn: &Connection, tab_id: i64) -> Result<Money> {
    find(conn, tab_id)?;
    let total = conn.query_row(
        "SELECT COALESCE(SUM(l.line_total_minor), 0)
           FROM orders o
           JOIN order_lines l ON l.order_id = o.id
          WHERE o.tab_id = ?1 AND o.status = 'ISSUED'",
        [tab_id],
        |row| row.get(0),
    )?;
    Ok(Money::from_minor(total))
}

/// Move collection responsibility while preserving an append-only account of
/// who handed the tab to whom.  Existing orders are never updated.
pub fn transfer(conn: &Connection, transfer: &Transfer<'_>) -> Result<Tab> {
    require_cashier(conn, transfer.transferred_by)?;
    require_waiter(conn, transfer.to_waiter_id)?;
    let shift = shifts::find(conn, transfer.shift_id)?;
    if shift.status != shifts::Status::Open {
        return super::refuse("a tab can only be transferred during an open shift");
    }
    let tab = find(conn, transfer.tab_id)?;
    if tab.status != Status::Open {
        return super::refuse("only an open tab can be transferred");
    }
    if tab.waiter_id == transfer.to_waiter_id {
        return super::refuse("a tab is already assigned to that waiter");
    }

    guarded!(conn.execute(
        "INSERT INTO tab_transfers
             (tab_id, from_waiter_id, to_waiter_id, shift_id,
              transferred_at, transferred_by, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            transfer.tab_id,
            tab.waiter_id,
            transfer.to_waiter_id,
            transfer.shift_id,
            transfer.transferred_at,
            transfer.transferred_by,
            transfer.reason.trim(),
        ],
    ))?;
    let changed = guarded!(conn.execute(
        "UPDATE tabs
            SET waiter_id = ?2
          WHERE id = ?1 AND waiter_id = ?3 AND status = 'OPEN'",
        rusqlite::params![transfer.tab_id, transfer.to_waiter_id, tab.waiter_id],
    ))?;
    if changed != 1 {
        return super::refuse("the tab changed before the transfer could finish");
    }
    find(conn, transfer.tab_id)
}

#[derive(Debug)]
struct StoredReference {
    table_no: Option<String>,
    customer_name: Option<String>,
    customer_phone: Option<String>,
    custom_ref: Option<String>,
    customer_tin: Option<String>,
    display_label: String,
}

fn normalise(reference: &Reference<'_>) -> Result<StoredReference> {
    let table_no = clean(reference.table_no);
    let customer_name = clean(reference.customer_name);
    let customer_phone = clean(reference.customer_phone);
    let custom_ref = clean(reference.custom_ref);
    let customer_tin = clean(reference.customer_tin);

    let display_label = match reference.mode {
        ReferenceMode::Table => {
            let Some(value) = table_no.as_deref() else {
                return super::refuse("a table tab needs a table number");
            };
            format!("Table {value}")
        }
        ReferenceMode::CustomerName => {
            let Some(value) = customer_name.as_deref() else {
                return super::refuse("a customer-name tab needs a customer name");
            };
            value.to_owned()
        }
        ReferenceMode::CustomerPhone => {
            let Some(name) = customer_name.as_deref() else {
                return super::refuse("a customer-phone tab needs a customer name");
            };
            match customer_phone.as_deref() {
                Some(phone) => format!("{name} ({phone})"),
                None => name.to_owned(),
            }
        }
        ReferenceMode::Custom => {
            let Some(value) = custom_ref.as_deref() else {
                return super::refuse("a custom tab needs a reference");
            };
            value.to_owned()
        }
    };

    Ok(StoredReference {
        table_no,
        customer_name,
        customer_phone,
        custom_ref,
        customer_tin,
        display_label,
    })
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for character in query.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

fn require_cashier(conn: &Connection, id: i64) -> Result<()> {
    let person = staff::find(conn, id)?;
    if !person.active || person.role != staff::Role::Cashier {
        return super::refuse("an active cashier must operate tabs");
    }
    Ok(())
}

fn require_waiter(conn: &Connection, id: i64) -> Result<()> {
    let person = staff::find(conn, id)?;
    if !person.active || person.role != staff::Role::Waiter {
        return super::refuse("an open tab belongs to an active waiter");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{fixture, shifts};
    use fixture::NOW;

    const END: i64 = NOW + 8 * 60 * 60 * 1_000;

    fn open_shift(bar: &fixture::Bar, date: &str, at: i64) -> shifts::Shift {
        shifts::open(
            &bar.conn,
            &shifts::NewShift {
                business_date: date,
                opened_at: at,
                opened_by: bar.cashier,
                opening_float: Money::ZERO,
                expected_end_at: at + (END - NOW),
            },
        )
        .unwrap()
    }

    fn new_tab<'a>(shift: i64, waiter: i64, reference: Reference<'a>, cashier: i64) -> NewTab<'a> {
        NewTab {
            opened_shift_id: shift,
            waiter_id: waiter,
            reference,
            opened_at: NOW,
            opened_by: cashier,
        }
    }

    #[test]
    fn open_freezes_both_tab_identities_and_separate_reference_fields() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        let reference = Reference::customer_phone("  Marta  ", Some(" 0911 22 33 "))
            .with_customer_tin("  TIN-77  ");
        let tab = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, reference, bar.cashier),
        )
        .unwrap();

        assert_eq!(tab.code, "TAB-000001");
        assert_eq!(tab.reference_mode, ReferenceMode::CustomerPhone);
        assert_eq!(tab.customer_name.as_deref(), Some("Marta"));
        assert_eq!(tab.customer_phone.as_deref(), Some("0911 22 33"));
        assert_eq!(tab.customer_tin.as_deref(), Some("TIN-77"));
        assert_eq!(tab.display_label, "Marta (0911 22 33)");

        let err = bar
            .conn
            .execute(
                "UPDATE tabs SET display_label = 'Somebody else' WHERE id = ?1",
                [tab.id],
            )
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("identity and reference are frozen"),
            "got: {err}"
        );
    }

    #[test]
    fn each_reference_mode_builds_the_documented_label() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        let cases = [
            (Reference::table(" 7 "), "Table 7"),
            (Reference::customer_name(" Liya "), "Liya"),
            (Reference::customer_phone("Marta", None), "Marta"),
            (Reference::custom(" VIP East "), "VIP East"),
        ];
        for (reference, expected) in cases {
            let tab = open(
                &bar.conn,
                &new_tab(shift.id, bar.sara, reference, bar.cashier),
            )
            .unwrap();
            assert_eq!(tab.display_label, expected);
        }
    }

    #[test]
    fn open_labels_are_unique_and_bad_reference_or_roles_are_refused() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("4"), bar.cashier),
        )
        .unwrap();
        let duplicate = open(
            &bar.conn,
            &new_tab(shift.id, bar.dawit, Reference::table("4"), bar.cashier),
        )
        .unwrap_err();
        assert!(
            duplicate.to_string().contains("already uses"),
            "got: {duplicate}"
        );

        open(
            &bar.conn,
            &new_tab(
                shift.id,
                bar.sara,
                Reference::customer_name("Marta"),
                bar.cashier,
            ),
        )
        .unwrap();
        let case_only_duplicate = open(
            &bar.conn,
            &new_tab(
                shift.id,
                bar.dawit,
                Reference::customer_name("marta"),
                bar.cashier,
            ),
        )
        .unwrap_err();
        assert!(
            case_only_duplicate.to_string().contains("already uses"),
            "got: {case_only_duplicate}"
        );

        let bad_reference = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::custom("  "), bar.cashier),
        )
        .unwrap_err();
        assert!(
            bad_reference.to_string().contains("needs a reference"),
            "got: {bad_reference}"
        );

        let bad_waiter = open(
            &bar.conn,
            &new_tab(shift.id, bar.cashier, Reference::table("5"), bar.cashier),
        )
        .unwrap_err();
        assert!(
            bad_waiter.to_string().contains("active waiter"),
            "got: {bad_waiter}"
        );
        let owner_operating = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("6"), bar.owner),
        )
        .unwrap_err();
        assert!(
            owner_operating.to_string().contains("active cashier"),
            "got: {owner_operating}"
        );
    }

    #[test]
    fn search_covers_every_reference_waiter_and_internal_code() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        let table = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("12"), bar.cashier),
        )
        .unwrap();
        let name = open(
            &bar.conn,
            &new_tab(
                shift.id,
                bar.dawit,
                Reference::customer_name("Liya Gebre"),
                bar.cashier,
            ),
        )
        .unwrap();
        let phone = open(
            &bar.conn,
            &new_tab(
                shift.id,
                bar.sara,
                Reference::customer_phone("Marta", Some("0911223344")),
                bar.cashier,
            ),
        )
        .unwrap();
        let custom = open(
            &bar.conn,
            &new_tab(
                shift.id,
                bar.dawit,
                Reference::custom("VIP East"),
                bar.cashier,
            ),
        )
        .unwrap();

        for (query, expected) in [
            ("12", table.id),
            ("geb", name.id),
            ("091122", phone.id),
            ("east", custom.id),
            ("Sara", phone.id),
            (custom.code.as_str(), custom.id),
        ] {
            let found = search(&bar.conn, query).unwrap();
            assert!(found.iter().any(|tab| tab.id == expected), "query: {query}");
        }
        assert!(search(&bar.conn, "   ")
            .unwrap_err()
            .to_string()
            .contains("type a tab"));
    }

    #[test]
    fn running_total_counts_issued_lines_only() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        let tab = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("3"), bar.cashier),
        )
        .unwrap();
        let recipe_id: i64 = bar
            .conn
            .query_row(
                "SELECT id FROM recipes WHERE sale_item_id = ?1 AND effective_to IS NULL",
                [bar.beer_bottle],
                |row| row.get(0),
            )
            .unwrap();

        let add_order = |price: i64| {
            bar.conn
                .execute(
                    "INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![tab.id, shift.id, bar.sara, bar.cashier, NOW],
                )
                .unwrap();
            let order_id = bar.conn.last_insert_rowid();
            bar.conn
                .execute(
                    "INSERT INTO order_lines
                         (order_id, sale_item_id, sale_item_name, recipe_id,
                          quantity_milli, unit_price_minor, line_total_minor)
                     VALUES (?1, ?2, 'Beer', ?3, 1000, ?4, ?4)",
                    rusqlite::params![order_id, bar.beer_bottle, recipe_id, price],
                )
                .unwrap();
            order_id
        };

        let issued = add_order(5_000);
        let _draft = add_order(9_000);
        bar.conn
            .execute(
                "UPDATE orders SET status = 'PRINTING' WHERE id = ?1",
                [issued],
            )
            .unwrap();
        bar.conn
            .execute(
                "UPDATE orders SET status = 'ISSUED', issued_at = ?2 WHERE id = ?1",
                rusqlite::params![issued, NOW + 1],
            )
            .unwrap();

        assert_eq!(
            running_total(&bar.conn, tab.id).unwrap(),
            Money::from_minor(5_000)
        );
    }

    #[test]
    fn transfer_changes_responsibility_not_the_original_order() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        let tab = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("8"), bar.cashier),
        )
        .unwrap();
        bar.conn
            .execute(
                "INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![tab.id, shift.id, bar.sara, bar.cashier, NOW],
            )
            .unwrap();
        let order_id = bar.conn.last_insert_rowid();
        bar.conn
            .execute(
                "UPDATE orders SET status = 'PRINTING' WHERE id = ?1",
                [order_id],
            )
            .unwrap();
        bar.conn
            .execute(
                "UPDATE orders SET status = 'ISSUED', issued_at = ?2 WHERE id = ?1",
                rusqlite::params![order_id, NOW + 1],
            )
            .unwrap();

        let moved = transfer(
            &bar.conn,
            &Transfer {
                tab_id: tab.id,
                to_waiter_id: bar.dawit,
                shift_id: shift.id,
                transferred_at: NOW + 2,
                transferred_by: bar.cashier,
                reason: "shift handover",
            },
        )
        .unwrap();
        assert_eq!(moved.waiter_id, bar.dawit);
        let order_waiter: i64 = bar
            .conn
            .query_row(
                "SELECT waiter_id FROM orders WHERE id = ?1",
                [order_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(order_waiter, bar.sara);
        let logged: (i64, i64, String) = bar
            .conn
            .query_row(
                "SELECT from_waiter_id, to_waiter_id, reason
                   FROM tab_transfers WHERE tab_id = ?1",
                [tab.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(logged, (bar.sara, bar.dawit, "shift handover".into()));
    }

    #[test]
    fn transfer_refuses_wrong_roles_same_waiter_closed_tab_and_closed_shift() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        let tab = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("9"), bar.cashier),
        )
        .unwrap();
        let request = |to_waiter_id, transferred_by| Transfer {
            tab_id: tab.id,
            to_waiter_id,
            shift_id: shift.id,
            transferred_at: NOW + 1,
            transferred_by,
            reason: "",
        };
        assert!(transfer(&bar.conn, &request(bar.sara, bar.cashier))
            .unwrap_err()
            .to_string()
            .contains("already assigned"));
        assert!(transfer(&bar.conn, &request(bar.cashier, bar.cashier))
            .unwrap_err()
            .to_string()
            .contains("active waiter"));
        assert!(transfer(&bar.conn, &request(bar.dawit, bar.owner))
            .unwrap_err()
            .to_string()
            .contains("active cashier"));

        bar.conn
            .execute(
                "UPDATE tabs
                    SET status = 'CLOSED', closed_shift_id = ?2,
                        closed_at = ?3, closed_by = ?4
                  WHERE id = ?1",
                rusqlite::params![tab.id, shift.id, NOW + 2, bar.cashier],
            )
            .unwrap();
        assert!(transfer(&bar.conn, &request(bar.dawit, bar.cashier))
            .unwrap_err()
            .to_string()
            .contains("only an open tab"));
        let reopen = bar
            .conn
            .execute("UPDATE tabs SET status = 'OPEN' WHERE id = ?1", [tab.id])
            .unwrap_err();
        assert!(
            reopen.to_string().contains("never reopened"),
            "got: {reopen}"
        );
    }

    #[test]
    fn an_open_tab_crosses_nights_and_can_transfer_in_the_new_shift() {
        let bar = fixture::bar();
        let first = open_shift(&bar, "2025-07-31", NOW);
        let tab = open(
            &bar.conn,
            &new_tab(
                first.id,
                bar.sara,
                Reference::custom("Late party"),
                bar.cashier,
            ),
        )
        .unwrap();
        shifts::begin_closing(&bar.conn, first.id, bar.cashier).unwrap();
        bar.conn
            .execute(
                "UPDATE shifts
                    SET status = 'CLOSED', closed_at = ?2, closed_by = ?3,
                        counted_cash_minor = 0
                  WHERE id = ?1",
                rusqlite::params![first.id, NOW + 10, bar.cashier],
            )
            .unwrap();

        assert_eq!(find(&bar.conn, tab.id).unwrap().status, Status::Open);
        let second = open_shift(&bar, "2025-08-01", NOW + 24 * 60 * 60 * 1_000);
        let moved = transfer(
            &bar.conn,
            &Transfer {
                tab_id: tab.id,
                to_waiter_id: bar.dawit,
                shift_id: second.id,
                transferred_at: NOW + 24 * 60 * 60 * 1_000 + 1,
                transferred_by: bar.cashier,
                reason: "carried into the new night",
            },
        )
        .unwrap();

        assert_eq!(moved.opened_shift_id, first.id);
        assert_eq!(moved.waiter_id, bar.dawit);
        let transfer_shift: i64 = bar
            .conn
            .query_row(
                "SELECT shift_id FROM tab_transfers WHERE tab_id = ?1",
                [tab.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(transfer_shift, second.id);
        assert!(search(&bar.conn, "Late party")
            .unwrap()
            .iter()
            .any(|found| found.id == tab.id));
    }

    #[test]
    fn a_closing_shift_cannot_open_another_tab() {
        let bar = fixture::bar();
        let shift = open_shift(&bar, "2025-07-31", NOW);
        shifts::begin_closing(&bar.conn, shift.id, bar.cashier).unwrap();
        let err = open(
            &bar.conn,
            &new_tab(shift.id, bar.sara, Reference::table("10"), bar.cashier),
        )
        .unwrap_err();
        assert!(err.to_string().contains("open shift"), "got: {err}");
    }
}
