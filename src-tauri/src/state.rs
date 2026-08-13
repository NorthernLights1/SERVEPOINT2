//! What the running application holds between commands.
//!
//! Three things, and no more: the open database, who is signed in, and how
//! many times somebody has just got the PIN wrong.
//!
//! # Why the session lives in memory and not in a table
//!
//! A till is one machine with one person at it. Persisting a session would
//! mean the app could start up already signed in as whoever used it last,
//! which is precisely wrong for a shared machine behind a bar: closing the lid
//! and walking away must end the session. Losing it on restart is the correct
//! behaviour, not a limitation.
//!
//! # Why there is a throttle
//!
//! A four-digit PIN is ten thousand guesses. Hashing it slowly makes an
//! offline attack tedious, but somebody standing at the keyboard is not doing
//! an offline attack — they are typing. The only defence there is to make each
//! wrong guess cost real time, and to make a run of them cost a lot.

use std::sync::Mutex;

use rusqlite::Connection;

/// Who is signed in. Serialised straight to the webview, so it deliberately
/// carries no hash, no salt and no PIN.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub staff_id: i64,
    pub code: String,
    pub name: String,
    /// `OWNER` or `CASHIER`. Waiters never sign in.
    pub role: String,
}

impl Session {
    pub fn is_owner(&self) -> bool {
        self.role == "OWNER"
    }
}

/// How long a wrong PIN costs. The first few mistakes are free, because a
/// bartender fat-fingering a PIN at two in the morning is the common case and
/// locking them out of the till mid-service is worse than the risk.
pub const FREE_ATTEMPTS: u32 = 3;
/// After the free ones, each further failure locks the keypad for this long,
/// doubling each time up to the ceiling below.
pub const FIRST_LOCKOUT_MS: i64 = 5_000;
pub const MAX_LOCKOUT_MS: i64 = 300_000;

/// Failed sign-in attempts and what they currently cost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Throttle {
    failures: u32,
    locked_until: i64,
}

impl Throttle {
    /// Milliseconds the keypad is still locked for, or zero if it is open.
    pub fn locked_for(&self, now: i64) -> i64 {
        (self.locked_until - now).max(0)
    }

    /// Record a wrong PIN and return how long the keypad is now locked.
    pub fn record_failure(&mut self, now: i64) -> i64 {
        self.failures = self.failures.saturating_add(1);
        if self.failures <= FREE_ATTEMPTS {
            self.locked_until = now;
            return 0;
        }
        // 5s, 10s, 20s, 40s ... capped. Doubling turns a patient attacker's
        // afternoon into a week without ever permanently locking out staff.
        let steps = self.failures - FREE_ATTEMPTS - 1;
        let penalty = FIRST_LOCKOUT_MS
            .saturating_mul(1i64.checked_shl(steps.min(16)).unwrap_or(i64::MAX))
            .min(MAX_LOCKOUT_MS);
        self.locked_until = now.saturating_add(penalty);
        penalty
    }

    /// A correct PIN wipes the slate.
    pub fn record_success(&mut self) {
        *self = Self::default();
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

/// Everything a command needs, handed to it by Tauri.
pub struct AppState {
    db: Mutex<Connection>,
    session: Mutex<Option<Session>>,
    throttle: Mutex<Throttle>,
}

impl AppState {
    pub fn new(db: Connection) -> Self {
        Self {
            db: Mutex::new(db),
            session: Mutex::new(None),
            throttle: Mutex::new(Throttle::default()),
        }
    }

    /// Run something against the database.
    ///
    /// Takes a closure rather than handing out the guard so the lock is always
    /// released, including when the closure returns an error — a till that
    /// deadlocks on its own database is a till that has to be restarted mid
    /// service.
    ///
    /// A poisoned lock is recovered rather than propagated: the panic that
    /// poisoned it has already been reported, and refusing every subsequent
    /// operation would turn one bad command into a dead application.
    pub fn with_db<T>(&self, work: impl FnOnce(&Connection) -> T) -> T {
        let guard = self
            .db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        work(&guard)
    }

    /// Run something against the database with `&mut`, for work that needs a
    /// transaction.
    pub fn with_db_mut<T>(&self, work: impl FnOnce(&mut Connection) -> T) -> T {
        let mut guard = self
            .db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        work(&mut guard)
    }

    pub fn session(&self) -> Option<Session> {
        self.session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_session(&self, session: Option<Session>) {
        *self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session;
    }

    pub fn with_throttle<T>(&self, work: impl FnOnce(&mut Throttle) -> T) -> T {
        let mut guard = self
            .throttle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        work(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_786_500_000_000;

    #[test]
    fn the_first_few_mistakes_are_free() {
        // A bartender mistyping at 2am must not be locked out of the till.
        let mut throttle = Throttle::default();
        for _ in 0..FREE_ATTEMPTS {
            assert_eq!(throttle.record_failure(NOW), 0);
            assert_eq!(throttle.locked_for(NOW), 0);
        }
    }

    #[test]
    fn persistent_guessing_gets_progressively_expensive() {
        let mut throttle = Throttle::default();
        for _ in 0..FREE_ATTEMPTS {
            throttle.record_failure(NOW);
        }
        assert_eq!(throttle.record_failure(NOW), 5_000);
        assert_eq!(throttle.record_failure(NOW), 10_000);
        assert_eq!(throttle.record_failure(NOW), 20_000);
        assert_eq!(throttle.record_failure(NOW), 40_000);
    }

    #[test]
    fn the_lockout_stops_growing_at_five_minutes() {
        // Never permanent: staff must always be able to get back in eventually
        // without an engineer.
        let mut throttle = Throttle::default();
        let mut last = 0;
        for _ in 0..40 {
            last = throttle.record_failure(NOW);
        }
        assert_eq!(last, MAX_LOCKOUT_MS);
    }

    #[test]
    fn the_lock_expires_on_its_own() {
        let mut throttle = Throttle::default();
        for _ in 0..=FREE_ATTEMPTS {
            throttle.record_failure(NOW);
        }
        assert_eq!(throttle.locked_for(NOW), 5_000);
        assert_eq!(throttle.locked_for(NOW + 2_000), 3_000);
        assert_eq!(throttle.locked_for(NOW + 5_000), 0);
        assert_eq!(throttle.locked_for(NOW + 60_000), 0);
    }

    #[test]
    fn getting_it_right_clears_the_record() {
        let mut throttle = Throttle::default();
        for _ in 0..6 {
            throttle.record_failure(NOW);
        }
        throttle.record_success();
        assert_eq!(throttle.failures(), 0);
        assert_eq!(throttle.locked_for(NOW), 0);
    }

    #[test]
    fn a_session_knows_whether_it_may_change_settings() {
        let owner = Session {
            staff_id: 7,
            code: "OWN-1".into(),
            name: "Selam".into(),
            role: "OWNER".into(),
        };
        let cashier = Session {
            role: "CASHIER".into(),
            ..owner.clone()
        };
        assert!(owner.is_owner());
        assert!(!cashier.is_owner());
    }
}
