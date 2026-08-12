//! Money and rates (§1, §1.1).
//!
//! Money is a signed 64-bit integer count of **minor units** — santim, cents,
//! whatever the venue's currency subdivides into. `12.50` is `1250`.
//!
//! Never float. `f64` cannot represent 0.10, so a night of order lines drifts
//! by amounts a customer can see and nobody at the till can explain. Never
//! decimal-as-string either: the arithmetic ends up scattered across parsing
//! code where nobody can audit the rounding.
//!
//! **All rounding is half-up on the absolute value**, so positive and negative
//! amounts round symmetrically. A refund of 12.345 and a charge of 12.345 must
//! not round in opposite directions, or a correction changes the total by a
//! santim for no reason anybody can articulate.

use std::fmt;

/// A rate in basis points. 15% is `1500`; 100% is `10_000`.
///
/// No floating percentages anywhere in this system. A rate that is 0.15 in one
/// place and 15.0 in another is a bug waiting for the day somebody changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BasisPoints(pub u32);

impl BasisPoints {
    pub const ZERO: Self = Self(0);
    pub const ONE_HUNDRED_PERCENT: Self = Self(10_000);

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for BasisPoints {
    /// `1500` renders as `15%`, `1250` as `12.5%`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0 / 100;
        let frac = self.0 % 100;
        if frac == 0 {
            write!(f, "{whole}%")
        } else if frac % 10 == 0 {
            write!(f, "{whole}.{}%", frac / 10)
        } else {
            write!(f, "{whole}.{frac:02}%")
        }
    }
}

/// An amount in minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(i64);

/// Arithmetic that would overflow.
///
/// §1.1: use checked arithmetic everywhere. Silently wrapping money is worse
/// than crashing — a wrapped total looks like a number and gets printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("money arithmetic overflowed 64 bits")]
    Overflow,
}

type Result<T> = std::result::Result<T, MoneyError>;

impl Money {
    pub const ZERO: Self = Self(0);

    pub const fn from_minor(minor: i64) -> Self {
        Self(minor)
    }

    pub const fn minor(self) -> i64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0.checked_add(other.0).map(Self).ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0.checked_sub(other.0).map(Self).ok_or(MoneyError::Overflow)
    }

    /// Multiply by a whole count — a quantity of units, not a fractional rate.
    pub fn checked_mul(self, factor: i64) -> Result<Self> {
        self.0.checked_mul(factor).map(Self).ok_or(MoneyError::Overflow)
    }

    /// `percentage_of(amount, bp)` — a service charge or an exclusive tax.
    ///
    /// ```text
    /// scaled  = |amount| * bp
    /// rounded = (scaled + 5000) / 10000        # integer division, half up
    /// return    sign(amount) * rounded
    /// ```
    pub fn percentage_of(self, rate: BasisPoints) -> Result<Self> {
        let magnitude = self.0.unsigned_abs();
        let scaled = magnitude
            .checked_mul(u64::from(rate.0))
            .ok_or(MoneyError::Overflow)?;
        let rounded = (scaled + 5_000) / 10_000;
        let rounded = i64::try_from(rounded).map_err(|_| MoneyError::Overflow)?;
        Ok(Self(if self.0 < 0 { -rounded } else { rounded }))
    }

    /// The amount before an *inclusive* tax was added: `gross / (1 + rate)`.
    ///
    /// ```text
    /// divisor = 10000 + bp
    /// scaled  = |gross| * 10000
    /// rounded = (scaled + divisor/2) / divisor
    /// ```
    pub fn net_of_tax_at(self, rate: BasisPoints) -> Result<Self> {
        let divisor = u64::from(10_000u32 + rate.0);
        let scaled = self
            .0
            .unsigned_abs()
            .checked_mul(10_000)
            .ok_or(MoneyError::Overflow)?;
        let rounded = (scaled + divisor / 2) / divisor;
        let rounded = i64::try_from(rounded).map_err(|_| MoneyError::Overflow)?;
        Ok(Self(if self.0 < 0 { -rounded } else { rounded }))
    }

    /// The tax already contained in a gross amount.
    ///
    /// **Defined by subtraction, never as `net * rate`.** Two independent
    /// roundings do not reconstruct the whole. This was a real shipped bug: a
    /// menu price of 1000.00 billed the customer 1000.01, and nobody standing
    /// at the till could answer why.
    pub fn tax_included_at(self, rate: BasisPoints) -> Result<Self> {
        self.checked_sub(self.net_of_tax_at(rate)?)
    }

    /// Render for display: `1250` becomes `"12.50"`.
    ///
    /// Grouping and the currency symbol are the caller's business — the code
    /// is configurable per venue and this crate has no opinion about it.
    pub fn to_display(self) -> String {
        let negative = self.0 < 0;
        let magnitude = self.0.unsigned_abs();
        let whole = magnitude / 100;
        let frac = magnitude % 100;
        format!("{}{whole}.{frac:02}", if negative { "-" } else { "" })
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_display())
    }
}

impl std::iter::Sum for Money {
    /// Panics on overflow rather than wrapping. A summed total that silently
    /// wrapped would be printed on a receipt as though it were real.
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Self {
        iter.fold(Money::ZERO, |acc, m| {
            acc.checked_add(m).expect("money total overflowed 64 bits")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(minor: i64) -> Money {
        Money::from_minor(minor)
    }

    #[test]
    fn renders_minor_units_with_two_decimals() {
        assert_eq!(m(1250).to_display(), "12.50");
        assert_eq!(m(5).to_display(), "0.05");
        assert_eq!(m(0).to_display(), "0.00");
        assert_eq!(m(-1250).to_display(), "-12.50");
        assert_eq!(m(10_000_000).to_display(), "100000.00");
    }

    #[test]
    fn percentage_rounds_half_up() {
        // 10% of 12.34 is 1.234 -> 1.23; of 12.35 is 1.235 -> 1.24 (half up).
        assert_eq!(m(1234).percentage_of(BasisPoints(1000)).unwrap(), m(123));
        assert_eq!(m(1235).percentage_of(BasisPoints(1000)).unwrap(), m(124));
    }

    #[test]
    fn percentage_rounds_symmetrically_around_zero() {
        // A refund and a charge of the same magnitude must not round in
        // opposite directions.
        let charge = m(1235).percentage_of(BasisPoints(1000)).unwrap();
        let refund = m(-1235).percentage_of(BasisPoints(1000)).unwrap();
        assert_eq!(charge.minor(), -refund.minor());
    }

    #[test]
    fn zero_rate_and_zero_amount_are_zero() {
        assert_eq!(m(9999).percentage_of(BasisPoints::ZERO).unwrap(), Money::ZERO);
        assert_eq!(Money::ZERO.percentage_of(BasisPoints(1500)).unwrap(), Money::ZERO);
    }

    #[test]
    fn inclusive_tax_is_extracted_by_subtraction() {
        // The bug this exists to prevent: a menu price of 1000.00 at 15%
        // inclusive must total exactly 1000.00 again, not 1000.01.
        let gross = m(100_000);
        let rate = BasisPoints(1500);
        let net = gross.net_of_tax_at(rate).unwrap();
        let tax = gross.tax_included_at(rate).unwrap();
        assert_eq!(net.checked_add(tax).unwrap(), gross);
    }

    #[test]
    fn inclusive_extraction_never_loses_a_unit_across_a_range() {
        let rate = BasisPoints(1500);
        for gross_minor in 0..5_000i64 {
            let gross = m(gross_minor);
            let net = gross.net_of_tax_at(rate).unwrap();
            let tax = gross.tax_included_at(rate).unwrap();
            assert_eq!(
                net.checked_add(tax).unwrap(),
                gross,
                "net + tax must reconstruct {gross_minor} exactly"
            );
        }
    }

    #[test]
    fn overflow_is_an_error_not_a_wrap() {
        assert_eq!(m(i64::MAX).checked_add(m(1)), Err(MoneyError::Overflow));
        assert_eq!(m(i64::MAX).percentage_of(BasisPoints(10_000)), Err(MoneyError::Overflow));
    }

    #[test]
    fn rates_render_readably() {
        assert_eq!(BasisPoints(1500).to_string(), "15%");
        assert_eq!(BasisPoints(1250).to_string(), "12.5%");
        assert_eq!(BasisPoints(1234).to_string(), "12.34%");
        assert_eq!(BasisPoints(0).to_string(), "0%");
    }
}
