//! Venue configuration (§12).
//!
//! ServePoint is sold to bars and clubs generally, not fitted to one of them.
//! **Nothing about a particular venue is compiled in.** The tax rate, the
//! service charge, the hour the trading day turns over, what a tab is
//! identified by, whether receipts print at all — every one of those is a row
//! in this table, and the schema ships them blank or neutral so that a club
//! which fills in nothing gets a visibly unconfigured till rather than
//! somebody else's business name on a fiscal document.
//!
//! # Reading is cheap, so it is done once per operation
//!
//! `Settings::load` pulls the whole table — twenty-six short rows — into a map.
//! Reading them one key at a time invites a half-configured snapshot, where a
//! bill is calculated with the new tax rate and the old service charge because
//! somebody pressed Save between two queries.
//!
//! # What is NOT here
//!
//! There is no key that permits negative stock. Insufficient stock always
//! blocks the sale (D9 as revised). The absence is deliberate and there is a
//! test that fails if such a key ever appears.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::bill::ChargeConfig;
use crate::calendar::{BusinessCalendar, CalendarError, TimeOfDay};
use crate::money::{BasisPoints, Money};

/// Every key the application reads. Spelling one wrong is a silent default,
/// so they are named once here and never typed as a literal anywhere else.
pub mod keys {
    pub const TAX_ENABLED: &str = "tax.enabled";
    pub const TAX_RATE_BP: &str = "tax.rate_bp";
    pub const TAX_INCLUSIVE: &str = "tax.inclusive";
    pub const SERVICE_ENABLED: &str = "service_charge.enabled";
    pub const SERVICE_RATE_BP: &str = "service_charge.rate_bp";
    pub const DAY_START: &str = "shift.day_start";
    pub const DAY_END: &str = "shift.day_end";
    pub const TABS_REFERENCE_MODE: &str = "tabs.reference_mode";
    pub const TABS_AGE_WARNING_DAYS: &str = "tabs.age_warning_days";
    pub const TABS_ASK_CUSTOMER_TIN: &str = "tabs.ask_customer_tin";
    pub const COMPS_ENABLED: &str = "payments.comps_enabled";
    pub const PARTIAL_ENABLED: &str = "payments.partial_enabled";
    pub const BANK_ACCOUNTS: &str = "payments.bank_accounts";
    pub const QR_ENABLED: &str = "payments.qr_enabled";
    pub const PRINT_REPORT: &str = "printing.report_enabled";
    pub const PRINT_CUSTOMER_RECEIPT: &str = "printing.customer_receipt_enabled";
    pub const SHOW_COST: &str = "reporting.show_cost";
    pub const BUSINESS_NAME: &str = "receipt.business_name";
    pub const ADDRESS: &str = "receipt.address";
    pub const PHONE: &str = "receipt.phone";
    pub const TIN: &str = "receipt.tin";
    pub const FOOTER: &str = "receipt.footer";
    pub const CHARS_PER_LINE: &str = "receipt.chars_per_line";
    pub const CURRENCY_CODE: &str = "locale.currency_code";
    pub const ROUNDING: &str = "locale.rounding";
    pub const SETUP_COMPLETED: &str = "setup.completed";
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("'{0}' is not a setting this version of ServePoint knows")]
    UnknownKey(String),

    #[error("{key} cannot be '{value}': {why}")]
    Invalid {
        key: String,
        value: String,
        why: String,
    },

    #[error("the trading hours are not readable: {0}")]
    BadCalendar(#[from] CalendarError),
}

type Result<T> = std::result::Result<T, SettingsError>;

/// What a tab is called on the floor. A club with numbered tables and a
/// members' bar that works by name need different labels on the same screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabReference {
    Table,
    CustomerName,
    CustomerPhone,
    Custom,
}

impl TabReference {
    fn parse(value: &str) -> Self {
        match value {
            "CUSTOMER_NAME" => Self::CustomerName,
            "CUSTOMER_PHONE" => Self::CustomerPhone,
            "CUSTOM" => Self::Custom,
            _ => Self::Table,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => "TABLE",
            Self::CustomerName => "CUSTOMER_NAME",
            Self::CustomerPhone => "CUSTOMER_PHONE",
            Self::Custom => "CUSTOM",
        }
    }

    /// What the "new tab" screen should call the field it is asking for.
    pub fn prompt(self) -> &'static str {
        match self {
            Self::Table => "Table number",
            Self::CustomerName => "Customer name",
            Self::CustomerPhone => "Customer phone",
            Self::Custom => "Reference",
        }
    }
}

/// A snapshot of the whole settings table.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    values: BTreeMap<String, String>,
}

impl Settings {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut statement = conn.prepare("SELECT key, value FROM settings")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut values = BTreeMap::new();
        for row in rows {
            let (key, value) = row?;
            values.insert(key, value);
        }
        Ok(Self { values })
    }

    pub fn text(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.text(key) == "1"
    }

    pub fn number(&self, key: &str) -> i64 {
        self.text(key).parse().unwrap_or(0)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The charge rules as they stand right now. Snapshotted onto a
    /// transaction at the moment it is calculated and never re-read, so that
    /// changing the rate tonight cannot restate what a customer paid last
    /// night.
    pub fn charge_config(&self) -> ChargeConfig {
        ChargeConfig {
            tax_enabled: self.flag(keys::TAX_ENABLED),
            tax_rate: BasisPoints(self.number(keys::TAX_RATE_BP).clamp(0, 10_000) as u32),
            tax_inclusive: self.flag(keys::TAX_INCLUSIVE),
            service_enabled: self.flag(keys::SERVICE_ENABLED),
            service_rate: BasisPoints(self.number(keys::SERVICE_RATE_BP).clamp(0, 10_000) as u32),
        }
    }

    pub fn calendar(&self) -> Result<BusinessCalendar> {
        Ok(BusinessCalendar {
            day_start: TimeOfDay::parse(self.text(keys::DAY_START))?,
            day_end: TimeOfDay::parse(self.text(keys::DAY_END))?,
        })
    }

    pub fn reference_mode(&self) -> TabReference {
        TabReference::parse(self.text(keys::TABS_REFERENCE_MODE))
    }

    pub fn setup_completed(&self) -> bool {
        self.flag(keys::SETUP_COMPLETED)
    }

    pub fn currency_code(&self) -> &str {
        self.text(keys::CURRENCY_CODE)
    }

    /// Format an amount the way this venue writes it: `288200` becomes
    /// `"2,882.00 ETB"`, or just `"2,882.00"` before anybody has said what
    /// currency the place trades in.
    ///
    /// **This is the only place money becomes text.** The webview never
    /// formats an amount, because a figure formatted in two places is a figure
    /// that eventually disagrees with the receipt.
    pub fn format_money(&self, amount: Money) -> String {
        let plain = amount.to_display();
        let (sign, digits) = match plain.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", plain.as_str()),
        };
        let (whole, frac) = digits.split_once('.').unwrap_or((digits, "00"));
        let grouped = group_thousands(whole);
        let code = self.currency_code();
        if code.is_empty() {
            format!("{sign}{grouped}.{frac}")
        } else {
            format!("{sign}{grouped}.{frac} {code}")
        }
    }
}

fn group_thousands(whole: &str) -> String {
    let bytes = whole.as_bytes();
    let mut out = String::with_capacity(whole.len() + whole.len() / 3);
    for (index, byte) in bytes.iter().enumerate() {
        // A separator goes before every digit whose distance from the end is a
        // multiple of three, except at the very start.
        if index > 0 && (bytes.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

/// The value type the schema expects for a key, so a write cannot store `"on"`
/// where the trigger demands `'0'` or `'1'`.
fn value_type_of(key: &str) -> Option<&'static str> {
    let kind = match key {
        keys::TAX_ENABLED
        | keys::TAX_INCLUSIVE
        | keys::SERVICE_ENABLED
        | keys::TABS_ASK_CUSTOMER_TIN
        | keys::COMPS_ENABLED
        | keys::PARTIAL_ENABLED
        | keys::QR_ENABLED
        | keys::PRINT_REPORT
        | keys::PRINT_CUSTOMER_RECEIPT
        | keys::SHOW_COST
        | keys::SETUP_COMPLETED => "BOOLEAN",
        keys::TAX_RATE_BP | keys::SERVICE_RATE_BP => "RATE",
        keys::TABS_AGE_WARNING_DAYS | keys::CHARS_PER_LINE => "INTEGER",
        keys::DAY_START | keys::DAY_END => "TIME",
        keys::TABS_REFERENCE_MODE
        | keys::BANK_ACCOUNTS
        | keys::BUSINESS_NAME
        | keys::ADDRESS
        | keys::PHONE
        | keys::TIN
        | keys::FOOTER
        | keys::CURRENCY_CODE
        | keys::ROUNDING => "STRING",
        _ => return None,
    };
    Some(kind)
}

/// Reject a value before SQLite does, so the operator sees a sentence rather
/// than a constraint name.
///
/// The database still enforces all of this — this is the friendly half of a
/// belt-and-braces pair, never the only check.
fn check_value(key: &str, value: &str, kind: &str) -> Result<()> {
    let refuse = |why: &str| {
        Err(SettingsError::Invalid {
            key: key.to_owned(),
            value: value.to_owned(),
            why: why.to_owned(),
        })
    };

    match kind {
        "BOOLEAN" if !matches!(value, "0" | "1") => return refuse("expected on or off"),
        "RATE" | "INTEGER" if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) => {
            return refuse("expected a whole number");
        }
        "TIME" if TimeOfDay::parse(value).is_err() => return refuse("expected a time like 18:00"),
        _ => {}
    }

    if matches!(key, keys::TAX_RATE_BP | keys::SERVICE_RATE_BP) {
        let rate: i64 = value.parse().unwrap_or(-1);
        if !(0..=10_000).contains(&rate) {
            return refuse("a rate cannot be negative or above 100%");
        }
    }
    if key == keys::TABS_REFERENCE_MODE
        && !matches!(
            value,
            "TABLE" | "CUSTOMER_NAME" | "CUSTOMER_PHONE" | "CUSTOM"
        )
    {
        return refuse("expected TABLE, CUSTOMER_NAME, CUSTOMER_PHONE or CUSTOM");
    }
    if key == keys::CHARS_PER_LINE {
        let width: i64 = value.parse().unwrap_or(0);
        // Narrower than 24 and no receipt line fits; wider than 96 and no
        // thermal printer sold for this purpose can render it.
        if !(24..=96).contains(&width) {
            return refuse("a receipt is between 24 and 96 characters wide");
        }
    }
    if key == keys::CURRENCY_CODE
        && !value.is_empty()
        && !(value.len() == 3 && value.bytes().all(|b| b.is_ascii_uppercase()))
    {
        return refuse("expected a three-letter code such as ETB, or nothing");
    }
    Ok(())
}

/// Write one setting. Returns the value it replaced, so the caller can put the
/// before-and-after into the audit log.
///
/// Takes `&Connection` and does not open a transaction: settings changes are
/// written inside the caller's transaction alongside their audit entry, so a
/// change can never be recorded without its log line or the other way round.
pub fn put(
    conn: &Connection,
    key: &str,
    value: &str,
    staff_id: Option<i64>,
    at: i64,
) -> Result<String> {
    let kind = value_type_of(key).ok_or_else(|| SettingsError::UnknownKey(key.to_owned()))?;
    check_value(key, value, kind)?;

    let previous: String = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => SettingsError::UnknownKey(key.to_owned()),
            other => SettingsError::Sqlite(other),
        })?;

    conn.execute(
        "UPDATE settings SET value = ?2, updated_at = ?3, updated_by = ?4 WHERE key = ?1",
        rusqlite::params![key, value, at, staff_id],
    )?;
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn fresh() -> Connection {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn the_shipped_defaults_describe_no_particular_venue() {
        let settings = Settings::load(&fresh()).unwrap();
        assert_eq!(settings.text(keys::BUSINESS_NAME), "");
        assert_eq!(settings.text(keys::TIN), "");
        assert_eq!(settings.currency_code(), "");
        assert!(!settings.setup_completed());
    }

    #[test]
    fn printing_a_report_is_off_until_somebody_asks_for_it() {
        // The owner reads reports on screen. Thermal paper is opt-in.
        let settings = Settings::load(&fresh()).unwrap();
        assert!(!settings.flag(keys::PRINT_REPORT));
        assert!(settings.flag(keys::PRINT_CUSTOMER_RECEIPT));
    }

    #[test]
    fn the_default_charge_config_taxes_nothing() {
        let settings = Settings::load(&fresh()).unwrap();
        let config = settings.charge_config();
        assert!(!config.tax_enabled);
        assert_eq!(config.effective_tax_rate(), BasisPoints::ZERO);
        // ...but the rate is remembered, so switching tax on does not also ask
        // the owner to retype 15%.
        assert_eq!(config.tax_rate, BasisPoints(1500));
    }

    #[test]
    fn the_trading_day_reads_back_as_a_calendar() {
        let calendar = Settings::load(&fresh()).unwrap().calendar().unwrap();
        assert_eq!(calendar.day_start, TimeOfDay::new(18, 0));
        assert_eq!(calendar.day_end, TimeOfDay::new(6, 0));
    }

    #[test]
    fn money_is_grouped_and_carries_the_venues_currency() {
        let conn = fresh();
        put(&conn, keys::CURRENCY_CODE, "ETB", None, 0).unwrap();
        let settings = Settings::load(&conn).unwrap();
        assert_eq!(
            settings.format_money(Money::from_minor(288_200)),
            "2,882.00 ETB"
        );
        assert_eq!(settings.format_money(Money::from_minor(5)), "0.05 ETB");
        assert_eq!(
            settings.format_money(Money::from_minor(100_000_000)),
            "1,000,000.00 ETB"
        );
        assert_eq!(
            settings.format_money(Money::from_minor(-25_000)),
            "-250.00 ETB"
        );
    }

    #[test]
    fn money_formats_without_a_currency_before_setup() {
        let settings = Settings::load(&fresh()).unwrap();
        assert_eq!(
            settings.format_money(Money::from_minor(288_200)),
            "2,882.00"
        );
    }

    #[test]
    fn a_write_returns_what_it_replaced() {
        let conn = fresh();
        let previous = put(&conn, keys::TAX_ENABLED, "1", Some(1), 1_786_500_000_000).unwrap();
        assert_eq!(previous, "0");
        assert!(Settings::load(&conn).unwrap().flag(keys::TAX_ENABLED));
    }

    #[test]
    fn a_key_this_version_does_not_know_is_refused() {
        let conn = fresh();
        // Including the one that used to let stock go negative. D9 removed it,
        // and a stale client asking for it must be told no.
        let err = put(&conn, "inventory.stock_policy", "ALLOW_NEGATIVE", None, 0).unwrap_err();
        assert!(matches!(err, SettingsError::UnknownKey(_)), "got: {err}");
    }

    #[test]
    fn nonsense_values_are_refused_with_a_sentence() {
        let conn = fresh();
        for (key, value) in [
            (keys::TAX_ENABLED, "yes"),
            (keys::TAX_RATE_BP, "-100"),
            (keys::TAX_RATE_BP, "20000"),
            (keys::DAY_START, "half six"),
            (keys::TABS_REFERENCE_MODE, "SEAT"),
            (keys::CHARS_PER_LINE, "4"),
            (keys::CURRENCY_CODE, "birr"),
        ] {
            let err = put(&conn, key, value, None, 0).unwrap_err();
            assert!(
                matches!(err, SettingsError::Invalid { .. }),
                "{key}={value} should have been refused, got: {err}"
            );
        }
    }

    #[test]
    fn a_blank_currency_is_allowed_because_that_is_the_shipped_state() {
        let conn = fresh();
        assert!(put(&conn, keys::CURRENCY_CODE, "", None, 0).is_ok());
    }

    #[test]
    fn the_reference_mode_names_its_own_field() {
        assert_eq!(TabReference::Table.prompt(), "Table number");
        assert_eq!(TabReference::CustomerPhone.prompt(), "Customer phone");
        assert_eq!(
            TabReference::parse("CUSTOMER_NAME"),
            TabReference::CustomerName
        );
        // Anything unrecognised falls back to tables rather than panicking at
        // the till.
        assert_eq!(TabReference::parse("SEAT"), TabReference::Table);
    }

    #[test]
    fn every_shipped_key_is_writable() {
        // A key the schema seeds but `value_type_of` does not know would be
        // permanently unchangeable from the settings screen — invisible until
        // an owner tries to change it.
        let conn = fresh();
        let settings = Settings::load(&conn).unwrap();
        for (key, _) in settings.iter() {
            assert!(
                value_type_of(key).is_some(),
                "{key} is seeded but cannot be written"
            );
        }
    }
}
