//! Staff PINs (§0.3, D22).
//!
//! Only two roles authenticate: the CASHIER who operates the till and the
//! OWNER who reads reports and changes settings. Waiters never log in.
//!
//! # What this protects against, and what it does not
//!
//! A PIN is four to eight digits, which is at most a hundred million
//! possibilities — and realistically far fewer, because people choose badly.
//! **No hash function makes a short PIN strong.** Storing it hashed is still
//! worth doing, because the realistic threat is not brute force: it is the
//! owner's nephew opening the database file with a SQLite browser and reading
//! the cashier's PIN off the screen, then using it after hours. A salted,
//! iterated hash stops exactly that, which is the attack that actually
//! happens in a bar.
//!
//! The iteration count is deliberately high enough to make an offline sweep of
//! the whole keyspace tedious rather than instant, and low enough that
//! unlocking the till feels immediate.
//!
//! Guessing at the keyboard is a separate problem with a separate answer:
//! the login command rate-limits attempts. A hash cannot help with that.

use sha2::{Digest, Sha256};

/// Shorter than this and a shoulder-surfer has it on the first glance.
pub const MIN_PIN_LEN: usize = 4;
/// Longer than this and staff write it on the till, which is worse.
pub const MAX_PIN_LEN: usize = 8;

/// Chosen so a single verification costs a few tens of milliseconds on the
/// kind of machine a bar actually buys — unnoticeable at the keyboard,
/// tedious for anyone sweeping the keyspace with the database file in hand.
pub const ITERATIONS: u32 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    #[error("a PIN must be at least {MIN_PIN_LEN} digits")]
    TooShort,
    #[error("a PIN must be at most {MAX_PIN_LEN} digits")]
    TooLong,
    #[error("a PIN must be digits only")]
    NotDigits,
    #[error("that PIN is too easy to guess — avoid repeated or consecutive digits")]
    TooSimple,
}

/// Reject the PINs that get guessed first.
///
/// This runs when a PIN is *set*, never when one is checked: refusing to
/// verify a weak PIN would lock out staff whose PIN was accepted yesterday.
pub fn validate_pin(pin: &str) -> Result<(), PinError> {
    if pin.len() < MIN_PIN_LEN {
        return Err(PinError::TooShort);
    }
    if pin.len() > MAX_PIN_LEN {
        return Err(PinError::TooLong);
    }
    if !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PinError::NotDigits);
    }

    let digits: Vec<i16> = pin.bytes().map(|b| i16::from(b - b'0')).collect();
    let all_same = digits.windows(2).all(|w| w[0] == w[1]);
    let ascending = digits.windows(2).all(|w| w[1] - w[0] == 1);
    let descending = digits.windows(2).all(|w| w[1] - w[0] == -1);
    if all_same || ascending || descending {
        return Err(PinError::TooSimple);
    }
    Ok(())
}

/// `SHA256^n(salt || pin)`, hex encoded.
///
/// The salt is per-staff-member and comes from the caller — in practice from
/// SQLite's `randomblob`, so this module needs no source of randomness and
/// stays a pure function that tests can pin down exactly.
pub fn hash_pin(pin: &str, salt_hex: &str) -> String {
    let mut digest = {
        let mut hasher = Sha256::new();
        hasher.update(salt_hex.as_bytes());
        hasher.update([0x1e]); // a real separator, for the reason audit.rs explains
        hasher.update(pin.as_bytes());
        hasher.finalize()
    };
    // Each round folds the salt back in, so two staff with the same PIN never
    // share a chain of intermediate values.
    for _ in 1..ITERATIONS {
        let mut hasher = Sha256::new();
        hasher.update(digest);
        hasher.update(salt_hex.as_bytes());
        digest = hasher.finalize();
    }
    hex(&digest)
}

/// Check a PIN in constant time with respect to the stored hash.
///
/// A short-circuiting `==` leaks how many leading characters were right, which
/// is enough to recover a hash byte by byte given enough attempts. The cost of
/// avoiding that is one XOR per byte.
pub fn verify_pin(pin: &str, salt_hex: &str, expected_hash: &str) -> bool {
    let computed = hash_pin(pin, salt_hex);
    constant_time_eq(computed.as_bytes(), expected_hash.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &str = "9f2c4ab7d1e05836";

    #[test]
    fn a_reasonable_pin_is_accepted() {
        for pin in ["4071", "9382", "50318", "84061927"] {
            assert_eq!(validate_pin(pin), Ok(()), "{pin} should be allowed");
        }
    }

    #[test]
    fn the_obvious_pins_are_refused() {
        assert_eq!(validate_pin("0000"), Err(PinError::TooSimple));
        assert_eq!(validate_pin("1111"), Err(PinError::TooSimple));
        assert_eq!(validate_pin("1234"), Err(PinError::TooSimple));
        assert_eq!(validate_pin("4321"), Err(PinError::TooSimple));
        assert_eq!(validate_pin("123"), Err(PinError::TooShort));
        assert_eq!(validate_pin("123456789"), Err(PinError::TooLong));
        assert_eq!(validate_pin("12a4"), Err(PinError::NotDigits));
    }

    #[test]
    fn a_pin_that_merely_starts_ascending_is_fine() {
        // Only a run that ascends the whole way is refused. Over-rejecting
        // pushes staff toward writing the PIN on the till.
        assert_eq!(validate_pin("1235"), Ok(()));
        assert_eq!(validate_pin("1224"), Ok(()));
    }

    #[test]
    fn the_same_pin_and_salt_always_hash_the_same() {
        assert_eq!(hash_pin("4071", SALT), hash_pin("4071", SALT));
    }

    #[test]
    fn the_same_pin_under_different_salts_does_not_collide() {
        // Two staff who both picked 4071 must not look identical in the table,
        // or reading one row tells you about the other.
        assert_ne!(hash_pin("4071", SALT), hash_pin("4071", "0000000000000000"));
    }

    #[test]
    fn a_hash_is_sixty_four_lowercase_hex_characters() {
        let hash = hash_pin("4071", SALT);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn the_right_pin_verifies_and_a_near_miss_does_not() {
        let stored = hash_pin("4071", SALT);
        assert!(verify_pin("4071", SALT, &stored));
        assert!(!verify_pin("4072", SALT, &stored));
        assert!(!verify_pin("407", SALT, &stored));
        assert!(!verify_pin("", SALT, &stored));
    }

    #[test]
    fn a_pin_does_not_verify_against_the_wrong_salt() {
        let stored = hash_pin("4071", SALT);
        assert!(!verify_pin("4071", "0000000000000000", &stored));
    }

    #[test]
    fn comparison_rejects_a_truncated_hash() {
        // A stored value someone shortened by hand must not match a prefix.
        let stored = hash_pin("4071", SALT);
        assert!(!verify_pin("4071", SALT, &stored[..32]));
    }
}
