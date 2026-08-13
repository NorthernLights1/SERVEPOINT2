//! The business calendar (§1.3).
//!
//! A club trades across midnight, so the calendar day is the wrong unit. **A
//! sale at 02:00 on Saturday belongs to Friday night's takings**, and every
//! report, every shift and every reconciliation has to agree about that.
//!
//! **Only `day_start` decides which business date an instant falls in.**
//! `day_end` is advisory: it is what makes a shift "overdue" and still open at
//! nine in the morning — the night somebody forgot to close and went home.
//!
//! Keeping date assignment to a single boundary means EVERY instant maps
//! somewhere, including the hours when the club is shut and someone is
//! entering a delivery or counting stock. A two-boundary rule would leave a
//! gap in the afternoon where a delivery belonged to no business date at all.
//!
//! The date is derived once, at write time, and **never recomputed**. A stored
//! date cannot drift when the setting changes; a recomputed one moves every
//! historical transaction the moment somebody adjusts the opening hour.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

/// A wall-clock time of day, as configured in settings (`"18:00"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeOfDay {
    pub hour: u32,
    pub minute: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalendarError {
    #[error("'{0}' is not a time of day in HH:mm form")]
    BadTime(String),
    #[error("timestamp {0} is not a valid instant")]
    BadInstant(i64),
}

impl TimeOfDay {
    pub const fn new(hour: u32, minute: u32) -> Self {
        Self { hour, minute }
    }

    /// Parse the `HH:mm` form stored in settings.
    pub fn parse(text: &str) -> Result<Self, CalendarError> {
        let bad = || CalendarError::BadTime(text.to_owned());
        let (h, m) = text.split_once(':').ok_or_else(bad)?;
        let hour: u32 = h.parse().map_err(|_| bad())?;
        let minute: u32 = m.parse().map_err(|_| bad())?;
        if hour > 23 || minute > 59 {
            return Err(bad());
        }
        Ok(Self { hour, minute })
    }

    fn to_naive(self) -> NaiveTime {
        NaiveTime::from_hms_opt(self.hour, self.minute, 0).expect("validated on construction")
    }
}

impl std::fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// The venue's trading day, as configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusinessCalendar {
    pub day_start: TimeOfDay,
    pub day_end: TimeOfDay,
}

impl Default for BusinessCalendar {
    fn default() -> Self {
        Self {
            day_start: TimeOfDay::new(18, 0),
            day_end: TimeOfDay::new(6, 0),
        }
    }
}

impl BusinessCalendar {
    /// The core rule, on a local wall-clock instant. Pure, so it can be tested
    /// without depending on the machine's timezone.
    pub fn business_date_of_local(&self, local: NaiveDateTime) -> NaiveDate {
        if local.time() < self.day_start.to_naive() {
            local.date() - Duration::days(1)
        } else {
            local.date()
        }
    }

    /// The business date for an instant, in the machine's local zone.
    ///
    /// Local time is a display concern everywhere else in this system, but
    /// here it is the substance: the boundary is 18:00 *where the club is*.
    pub fn business_date_for(&self, epoch_ms: i64) -> Result<NaiveDate, CalendarError> {
        let local = Local
            .timestamp_millis_opt(epoch_ms)
            .single()
            .ok_or(CalendarError::BadInstant(epoch_ms))?;
        Ok(self.business_date_of_local(local.naive_local()))
    }

    pub fn expected_start(&self, date: NaiveDate) -> NaiveDateTime {
        date.and_time(self.day_start.to_naive())
    }

    /// When the night should have finished. Rolls onto the following day
    /// whenever the closing hour is earlier than the opening one — which is
    /// the normal case for a club: 18:00 to 06:00.
    pub fn expected_end(&self, date: NaiveDate) -> NaiveDateTime {
        let date = if self.day_end > self.day_start {
            date
        } else {
            date + Duration::days(1)
        };
        date.and_time(self.day_end.to_naive())
    }

    /// §4.6. Required for the night somebody forgets to close and goes home.
    pub fn is_overdue(&self, date: NaiveDate, now_local: NaiveDateTime) -> bool {
        now_local > self.expected_end(date)
    }
}

/// Format an instant as the wall clock somebody in the venue was reading:
/// `23:41`. Used on the shift report, where "opened at" and "closed at" mean
/// the hours the staff actually worked, not UTC.
///
/// An instant the local zone cannot represent renders blank rather than
/// guessing an hour — a report is evidence, and a wrong time on it is worse
/// than a missing one.
pub fn clock_label(epoch_ms: i64) -> String {
    Local
        .timestamp_millis_opt(epoch_ms)
        .single()
        .map(|local| local.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Format a business date for a heading: `2026-08-14` reads as `Fri 14 Aug`.
pub fn describe(date: NaiveDate) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let day = DAYS[date.weekday().num_days_from_monday() as usize];
    let month = MONTHS[(date.month() - 1) as usize];
    format!("{day} {} {month}", date.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn two_in_the_morning_belongs_to_the_night_before() {
        // The whole reason this module exists.
        let cal = BusinessCalendar::default();
        assert_eq!(
            cal.business_date_of_local(at(2026, 8, 15, 2, 0)),
            date(2026, 8, 14)
        );
    }

    #[test]
    fn the_evening_belongs_to_its_own_date() {
        let cal = BusinessCalendar::default();
        assert_eq!(
            cal.business_date_of_local(at(2026, 8, 14, 21, 30)),
            date(2026, 8, 14)
        );
    }

    #[test]
    fn every_instant_maps_somewhere_including_the_shut_hours() {
        // A delivery at 11:00, when the club is closed, still has a business
        // date — it belongs to the night that has not started yet, minus one.
        let cal = BusinessCalendar::default();
        assert_eq!(
            cal.business_date_of_local(at(2026, 8, 14, 11, 0)),
            date(2026, 8, 13)
        );
    }

    #[test]
    fn the_boundary_itself_starts_the_new_night() {
        let cal = BusinessCalendar::default();
        assert_eq!(
            cal.business_date_of_local(at(2026, 8, 14, 18, 0)),
            date(2026, 8, 14)
        );
        assert_eq!(
            cal.business_date_of_local(at(2026, 8, 14, 17, 59)),
            date(2026, 8, 13)
        );
    }

    #[test]
    fn a_month_boundary_rolls_back_correctly() {
        let cal = BusinessCalendar::default();
        assert_eq!(
            cal.business_date_of_local(at(2026, 9, 1, 3, 0)),
            date(2026, 8, 31)
        );
    }

    #[test]
    fn a_daytime_venue_needs_no_special_case() {
        // 09:00 to 17:00: nothing crosses midnight and every instant after
        // opening belongs to its own date.
        let cal = BusinessCalendar {
            day_start: TimeOfDay::new(9, 0),
            day_end: TimeOfDay::new(17, 0),
        };
        assert_eq!(
            cal.business_date_of_local(at(2026, 8, 14, 12, 0)),
            date(2026, 8, 14)
        );
        assert_eq!(cal.expected_end(date(2026, 8, 14)), at(2026, 8, 14, 17, 0));
    }

    #[test]
    fn the_expected_end_crosses_midnight_for_a_club() {
        let cal = BusinessCalendar::default();
        assert_eq!(
            cal.expected_start(date(2026, 8, 14)),
            at(2026, 8, 14, 18, 0)
        );
        assert_eq!(cal.expected_end(date(2026, 8, 14)), at(2026, 8, 15, 6, 0));
    }

    #[test]
    fn a_shift_still_open_at_nine_in_the_morning_is_overdue() {
        let cal = BusinessCalendar::default();
        let night = date(2026, 8, 14);
        assert!(!cal.is_overdue(night, at(2026, 8, 15, 5, 0)));
        assert!(cal.is_overdue(night, at(2026, 8, 15, 9, 0)));
    }

    #[test]
    fn times_parse_and_round_trip() {
        assert_eq!(TimeOfDay::parse("18:00").unwrap(), TimeOfDay::new(18, 0));
        assert_eq!(TimeOfDay::parse("06:30").unwrap(), TimeOfDay::new(6, 30));
        assert_eq!(TimeOfDay::new(6, 30).to_string(), "06:30");
        assert!(TimeOfDay::parse("24:00").is_err());
        assert!(TimeOfDay::parse("18:60").is_err());
        assert!(TimeOfDay::parse("evening").is_err());
    }

    #[test]
    fn dates_describe_readably() {
        assert_eq!(describe(date(2026, 8, 14)), "Fri 14 Aug");
    }
}
