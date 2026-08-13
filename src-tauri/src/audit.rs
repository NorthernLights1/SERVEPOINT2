//! The audit hash chain (§10, §10.1).
//!
//! **Tamper-evidence, not tamper-proofing.** The database file sits on a
//! machine the owner controls and anyone with a SQLite browser can rewrite it.
//! Nothing here prevents that. What it buys is that an edit CANNOT BE HIDDEN:
//! each row hashes the one before it, so changing any row breaks every hash
//! after it and the integrity check names the first broken row.
//!
//! # The defect this module exists to fix
//!
//! The Java implementation joined the hashed fields with an **empty
//! separator**. That makes `"ab" + "c"` and `"a" + "bc"` hash identically, so
//! adjacent fields could be shuffled — an action of `ORDER_VOID` on entity
//! `ED_42` hashes the same as `ORDER_VOIDED` on `_42` — and the chain would
//! happily verify. A hash chain that can be fooled by moving a character is
//! worse than no hash chain, because it is trusted.
//!
//! The fix here is belt and braces: every field is **length-prefixed** as well
//! as separated, and `NULL` has its own encoding distinct from the empty
//! string. With the length written first, no two different field sequences can
//! produce the same byte stream.

use sha2::{Digest, Sha256};

/// The `prev_hash` of the first row: 64 zeroes.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const FIELD_SEPARATOR: u8 = 0x1e; // ASCII record separator
const NULL_MARKER: u8 = b'~';

/// One field going into the hash. `Null` is deliberately distinct from
/// `Text("")` — "no old value" and "the old value was blank" are different
/// facts, and a scheme that conflates them can be gamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditField<'a> {
    Text(&'a str),
    Int(i64),
    Null,
}

impl<'a> From<&'a str> for AuditField<'a> {
    fn from(value: &'a str) -> Self {
        AuditField::Text(value)
    }
}

impl<'a> From<Option<&'a str>> for AuditField<'a> {
    fn from(value: Option<&'a str>) -> Self {
        value.map_or(AuditField::Null, AuditField::Text)
    }
}

impl From<i64> for AuditField<'_> {
    fn from(value: i64) -> Self {
        AuditField::Int(value)
    }
}

impl From<Option<i64>> for AuditField<'_> {
    fn from(value: Option<i64>) -> Self {
        value.map_or(AuditField::Null, AuditField::Int)
    }
}

/// The fields hashed into a row, in the order §10.1 specifies.
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    pub sequence_no: i64,
    pub staff_id: Option<i64>,
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: Option<i64>,
    pub old_value: Option<&'a str>,
    pub new_value: Option<&'a str>,
    /// Which trading night this happened on; `None` when the club was shut.
    ///
    /// Hashed along with everything else. Leaving it out would let an entry be
    /// moved from one night to another without breaking the chain, and "which
    /// shift was this void on" is precisely the question the log exists to
    /// answer.
    pub shift_id: Option<i64>,
    pub created_at: i64,
    pub prev_hash: &'a str,
}

impl AuditEntry<'_> {
    /// `SHA256(seq | staff | action | type | id | old | new | shift | at | prev)`,
    /// where `|` is a real separator and every field carries its length.
    pub fn row_hash(&self) -> String {
        let mut hasher = Sha256::new();
        let fields: [AuditField<'_>; 10] = [
            AuditField::Int(self.sequence_no),
            AuditField::from(self.staff_id),
            AuditField::Text(self.action),
            AuditField::Text(self.entity_type),
            AuditField::from(self.entity_id),
            AuditField::from(self.old_value),
            AuditField::from(self.new_value),
            AuditField::from(self.shift_id),
            AuditField::Int(self.created_at),
            AuditField::Text(self.prev_hash),
        ];
        for field in fields {
            hasher.update([FIELD_SEPARATOR]);
            match field {
                AuditField::Null => hasher.update([NULL_MARKER]),
                AuditField::Int(value) => {
                    let text = value.to_string();
                    hasher.update(text.len().to_string().as_bytes());
                    hasher.update(b":");
                    hasher.update(text.as_bytes());
                }
                AuditField::Text(text) => {
                    hasher.update(text.len().to_string().as_bytes());
                    hasher.update(b":");
                    hasher.update(text.as_bytes());
                }
            }
        }
        hex(&hasher.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What an integrity check found. It names the first broken row rather than
/// reporting a bare pass/fail — "the log is broken" is not actionable; "row
/// 4471 was altered" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStatus {
    Intact {
        rows: usize,
    },
    Broken {
        sequence_no: i64,
        reason: BreakReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakReason {
    /// A row is missing — somebody deleted history.
    SequenceGap { expected: i64 },
    /// The row does not point at the row before it.
    PrevHashMismatch,
    /// The row's own contents no longer hash to its stored hash.
    ContentAltered,
}

impl std::fmt::Display for BreakReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BreakReason::SequenceGap { expected } => {
                write!(f, "a row is missing — expected sequence {expected}")
            }
            BreakReason::PrevHashMismatch => {
                f.write_str("this row does not follow the one before it")
            }
            BreakReason::ContentAltered => {
                f.write_str("this row's contents no longer match its hash")
            }
        }
    }
}

/// Walk the chain in `sequence_no` order. Fails on a gap, a `prev_hash` that
/// does not match the running chain, or a recomputed hash that does not match
/// what is stored.
pub fn verify_chain<'a>(rows: impl IntoIterator<Item = (AuditEntry<'a>, &'a str)>) -> ChainStatus {
    let mut expected_seq = 1i64;
    let mut running = GENESIS_HASH.to_owned();
    let mut count = 0usize;

    for (entry, stored_hash) in rows {
        if entry.sequence_no != expected_seq {
            return ChainStatus::Broken {
                sequence_no: entry.sequence_no,
                reason: BreakReason::SequenceGap {
                    expected: expected_seq,
                },
            };
        }
        if entry.prev_hash != running {
            return ChainStatus::Broken {
                sequence_no: entry.sequence_no,
                reason: BreakReason::PrevHashMismatch,
            };
        }
        let recomputed = entry.row_hash();
        if recomputed != stored_hash {
            return ChainStatus::Broken {
                sequence_no: entry.sequence_no,
                reason: BreakReason::ContentAltered,
            };
        }
        running = recomputed;
        expected_seq += 1;
        count += 1;
    }

    ChainStatus::Intact { rows: count }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(seq: i64, action: &'a str, entity: &'a str, prev: &'a str) -> AuditEntry<'a> {
        AuditEntry {
            sequence_no: seq,
            staff_id: Some(1),
            action,
            entity_type: entity,
            entity_id: Some(42),
            old_value: None,
            new_value: Some("{}"),
            shift_id: Some(3),
            created_at: 1_700_000_000_000,
            prev_hash: prev,
        }
    }

    #[test]
    fn moving_an_entry_to_another_night_breaks_its_hash() {
        // "Which shift was this void on" is the question the log exists to
        // answer, so the answer has to be inside the hash.
        let mut friday = entry(1, "ORDER_VOIDED", "order", GENESIS_HASH);
        friday.shift_id = Some(3);
        let mut saturday = entry(1, "ORDER_VOIDED", "order", GENESIS_HASH);
        saturday.shift_id = Some(4);
        assert_ne!(friday.row_hash(), saturday.row_hash());
    }

    #[test]
    fn a_hash_is_sixty_four_lowercase_hex_characters() {
        // The schema CHECKs exactly this, so a mismatch here is a failed
        // INSERT at the till rather than a test failure.
        let hash = entry(1, "SHIFT_OPENED", "shift", GENESIS_HASH).row_hash();
        assert_eq!(hash.len(), 64);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn adjacent_fields_cannot_be_shuffled() {
        // THE JAVA BUG. With an empty separator these two hash identically,
        // because "ORDER_VOID" + "ED_42" is the same byte stream as
        // "ORDER_VOIDED" + "_42".
        let a = entry(1, "ORDER_VOID", "ED_42", GENESIS_HASH).row_hash();
        let b = entry(1, "ORDER_VOIDED", "_42", GENESIS_HASH).row_hash();
        assert_ne!(
            a, b,
            "shuffling a character between fields must change the hash"
        );
    }

    #[test]
    fn a_null_is_not_an_empty_string() {
        let mut null_old = entry(1, "SETTING_CHANGED", "settings", GENESIS_HASH);
        null_old.old_value = None;
        let mut blank_old = entry(1, "SETTING_CHANGED", "settings", GENESIS_HASH);
        blank_old.old_value = Some("");
        assert_ne!(
            null_old.row_hash(),
            blank_old.row_hash(),
            "'no previous value' and 'the previous value was blank' are different facts"
        );
    }

    #[test]
    fn the_same_entry_always_hashes_the_same() {
        let e = entry(7, "ORDER_ISSUED", "order", GENESIS_HASH);
        assert_eq!(e.row_hash(), e.row_hash());
    }

    #[test]
    fn a_chain_verifies_end_to_end() {
        let first = entry(1, "SHIFT_OPENED", "shift", GENESIS_HASH);
        let first_hash = first.row_hash();
        let second = entry(2, "ORDER_ISSUED", "order", &first_hash);
        let second_hash = second.row_hash();

        let status = verify_chain([(first, first_hash.as_str()), (second, second_hash.as_str())]);
        assert_eq!(status, ChainStatus::Intact { rows: 2 });
    }

    #[test]
    fn an_altered_row_is_named() {
        let first = entry(1, "SHIFT_OPENED", "shift", GENESIS_HASH);
        let first_hash = first.row_hash();
        let second = entry(2, "ORDER_ISSUED", "order", &first_hash);
        let second_hash = second.row_hash();

        // Somebody edits the action of row 2 in a SQLite browser but cannot
        // recompute the hash without rewriting the rest of the chain.
        let tampered = entry(2, "ORDER_VOIDED", "order", &first_hash);
        let status = verify_chain([
            (first, first_hash.as_str()),
            (tampered, second_hash.as_str()),
        ]);
        assert_eq!(
            status,
            ChainStatus::Broken {
                sequence_no: 2,
                reason: BreakReason::ContentAltered
            }
        );
    }

    #[test]
    fn a_deleted_row_is_named() {
        let first = entry(1, "SHIFT_OPENED", "shift", GENESIS_HASH);
        let first_hash = first.row_hash();
        let third = entry(3, "ORDER_ISSUED", "order", &first_hash);
        let third_hash = third.row_hash();

        let status = verify_chain([(first, first_hash.as_str()), (third, third_hash.as_str())]);
        assert_eq!(
            status,
            ChainStatus::Broken {
                sequence_no: 3,
                reason: BreakReason::SequenceGap { expected: 2 }
            }
        );
    }

    #[test]
    fn a_row_spliced_from_elsewhere_is_named() {
        let first = entry(1, "SHIFT_OPENED", "shift", GENESIS_HASH);
        let first_hash = first.row_hash();
        // Row 2 was lifted from a different chain, so its prev_hash is wrong
        // even though the row hashes to its own stored value correctly.
        let foreign = entry(2, "ORDER_ISSUED", "order", GENESIS_HASH);
        let foreign_hash = foreign.row_hash();

        let status = verify_chain([
            (first, first_hash.as_str()),
            (foreign, foreign_hash.as_str()),
        ]);
        assert_eq!(
            status,
            ChainStatus::Broken {
                sequence_no: 2,
                reason: BreakReason::PrevHashMismatch
            }
        );
    }

    #[test]
    fn an_empty_log_is_intact() {
        assert_eq!(verify_chain([]), ChainStatus::Intact { rows: 0 });
    }
}
