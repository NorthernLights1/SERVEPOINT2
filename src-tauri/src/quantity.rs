//! Quantities (§1, §1.2).
//!
//! A signed 64-bit count of **thousandths of a base unit**. `2.5 shots` is
//! `2500`; `1 bottle` is `1000`.
//!
//! Three decimal places because recipes need fractions — half a bottle of
//! tonic in a gin and tonic — and because three places is already far finer
//! than anyone can physically pour. Integers for the same reason money is
//! integer: a ledger that sums floats does not sum to what it should.

use std::fmt;

use crate::money::MoneyError;

/// A quantity in thousandths of a base unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Milli(i64);

type Result<T> = std::result::Result<T, MoneyError>;

impl Milli {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1_000);

    pub const fn from_thousandths(value: i64) -> Self {
        Self(value)
    }

    /// Whole base units: `Milli::from_units(3)` is three bottles.
    pub const fn from_units(units: i64) -> Self {
        Self(units * 1_000)
    }

    pub const fn thousandths(self) -> i64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    /// Scale by a whole count — a recipe line times the number ordered.
    pub fn checked_mul(self, factor: i64) -> Result<Self> {
        self.0
            .checked_mul(factor)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// §1.2: read a raw quantity back the way somebody at the shelf counts it.
    ///
    /// 75 000 milli-shots of gin at 24 000 per bottle is "3 bottles + 3 shots",
    /// not "75 shots". Returns `(whole_packs, remainder)`.
    ///
    /// Truncates toward zero, so a negative quantity reads as a negative
    /// number of packs and a negative remainder — which is what somebody
    /// looking at a shortfall expects to see.
    pub fn split_by_pack(self, pack: Milli) -> (i64, Milli) {
        if pack.0 == 0 {
            return (0, self);
        }
        (self.0 / pack.0, Self(self.0 % pack.0))
    }

    /// Render for display, trimming trailing zeroes: `2500` is `"2.5"`,
    /// `1000` is `"1"`, `1005` is `"1.005"`.
    pub fn to_display(self) -> String {
        let negative = self.0 < 0;
        let magnitude = self.0.unsigned_abs();
        let whole = magnitude / 1_000;
        let frac = magnitude % 1_000;
        let sign = if negative { "-" } else { "" };
        if frac == 0 {
            format!("{sign}{whole}")
        } else {
            let text = format!("{frac:03}");
            format!("{sign}{whole}.{}", text.trim_end_matches('0'))
        }
    }
}

impl fmt::Display for Milli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_display())
    }
}

impl std::iter::Sum for Milli {
    fn sum<I: Iterator<Item = Milli>>(iter: I) -> Self {
        iter.fold(Milli::ZERO, |acc, q| {
            acc.checked_add(q)
                .expect("quantity total overflowed 64 bits")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_way_a_person_would_write_it() {
        assert_eq!(Milli::from_thousandths(2_500).to_display(), "2.5");
        assert_eq!(Milli::from_thousandths(1_000).to_display(), "1");
        assert_eq!(Milli::from_thousandths(1_005).to_display(), "1.005");
        assert_eq!(Milli::from_thousandths(500).to_display(), "0.5");
        assert_eq!(Milli::from_thousandths(-2_500).to_display(), "-2.5");
        assert_eq!(Milli::ZERO.to_display(), "0");
    }

    #[test]
    fn splits_into_packs_the_way_the_shelf_reads() {
        // §1.2's worked example: gin at 24 shots per bottle.
        let bottle = Milli::from_units(24);
        let (packs, rest) = Milli::from_units(75).split_by_pack(bottle);
        assert_eq!(packs, 3);
        assert_eq!(rest, Milli::from_units(3));
    }

    #[test]
    fn splits_exactly_when_there_is_no_remainder() {
        let (packs, rest) = Milli::from_units(48).split_by_pack(Milli::from_units(24));
        assert_eq!((packs, rest), (2, Milli::ZERO));
    }

    #[test]
    fn a_shortfall_reads_as_a_shortfall() {
        let (packs, rest) = Milli::from_units(-27).split_by_pack(Milli::from_units(24));
        assert_eq!(packs, -1);
        assert_eq!(rest, Milli::from_units(-3));
    }

    #[test]
    fn a_zero_pack_size_cannot_divide_by_zero() {
        // products.base_units_per_pack is CHECKed > 0, but a helper that
        // panics on bad data is a crash at the till.
        let (packs, rest) = Milli::from_units(5).split_by_pack(Milli::ZERO);
        assert_eq!((packs, rest), (0, Milli::from_units(5)));
    }

    #[test]
    fn recipe_expansion_sums_rather_than_overwrites() {
        // §2.5: a double measure may be written as two lines of the same
        // product, and expansion must add them.
        let lines = [Milli::ONE, Milli::ONE, Milli::from_thousandths(500)];
        let total: Milli = lines.into_iter().sum();
        assert_eq!(total, Milli::from_thousandths(2_500));
    }
}
