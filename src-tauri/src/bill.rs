//! The bill calculator (§7.2).
//!
//! Four combinations, and the fiddliest one is the one most venues use.
//!
//! Two facts fix the order of operations:
//!
//! * **Service charge is taxable.** It is added before tax is worked out, not
//!   after, so it cannot be treated as a rounding afterthought.
//! * **Rounding happens once, on the accumulated line total, never per line.**
//!   Rounding each line and summing drifts against a bill the customer can add
//!   up on the back of the slip — and they do.
//!
//! In inclusive mode the receipt's "Subtotal" shows the **extracted net**, not
//! the menu line total: tax is pulled out of the lines while simultaneously
//! being added onto the service charge. §7.2 calls this the combination worth
//! testing hardest, and the tests below take that literally.
//!
//! Whether prices include tax is **a fact about the business, not a
//! preference**. Set it wrong and every total is off by the tax rate while
//! looking perfectly correct on screen, which is why it sits on the
//! commissioning checklist rather than in a settings screen someone skims.

use crate::money::{BasisPoints, Money, MoneyError};

/// The charge settings as they stood when a bill was calculated.
///
/// Snapshotted onto the transaction, never read back from current settings:
/// changing a rate tonight must not restate what a customer paid last night.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargeConfig {
    pub tax_enabled: bool,
    pub tax_rate: BasisPoints,
    /// True when menu prices already contain the tax.
    pub tax_inclusive: bool,
    pub service_enabled: bool,
    pub service_rate: BasisPoints,
}

impl ChargeConfig {
    /// Nothing added and nothing extracted — a venue with neither VAT nor a
    /// service charge, which is a perfectly ordinary configuration.
    pub const NONE: Self = Self {
        tax_enabled: false,
        tax_rate: BasisPoints::ZERO,
        tax_inclusive: false,
        service_enabled: false,
        service_rate: BasisPoints::ZERO,
    };

    /// The rate actually applied, which is zero whenever the charge is off.
    /// Stored on the transaction so an old bill can explain its own figures.
    pub fn effective_tax_rate(&self) -> BasisPoints {
        if self.tax_enabled {
            self.tax_rate
        } else {
            BasisPoints::ZERO
        }
    }

    pub fn effective_service_rate(&self) -> BasisPoints {
        if self.service_enabled {
            self.service_rate
        } else {
            BasisPoints::ZERO
        }
    }
}

/// A calculated bill. Every figure is already final; nothing downstream
/// recomputes any of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bill {
    /// The sum of the order lines as priced on the menu.
    pub line_total: Money,
    /// What the receipt prints as "Subtotal". In inclusive mode this is the
    /// extracted net and is therefore LESS than `line_total`.
    pub net: Money,
    pub service_charge: Money,
    pub tax: Money,
    pub total: Money,
    pub tax_rate: BasisPoints,
    pub service_rate: BasisPoints,
    pub tax_inclusive: bool,
}

impl Bill {
    pub fn calculate(line_total: Money, config: &ChargeConfig) -> Result<Self, MoneyError> {
        let service_rate = config.effective_service_rate();
        let tax_rate = config.effective_tax_rate();

        let (net, service_charge, tax, total) = if !config.tax_enabled {
            // No tax at all. The service charge is simply added.
            let service = line_total.percentage_of(service_rate)?;
            (line_total, service, Money::ZERO, line_total.checked_add(service)?)
        } else if !config.tax_inclusive {
            // Menu prices exclude tax. Service is charged on the lines, then
            // tax on the two together — because service is taxable.
            let service = line_total.percentage_of(service_rate)?;
            let taxable = line_total.checked_add(service)?;
            let tax = taxable.percentage_of(tax_rate)?;
            (line_total, service, tax, taxable.checked_add(tax)?)
        } else {
            // Menu prices already contain the tax. Extract it from the lines
            // BY SUBTRACTION, charge service on the extracted net, then add
            // the tax that the service charge itself attracts.
            let net = line_total.net_of_tax_at(tax_rate)?;
            let tax_on_lines = line_total.checked_sub(net)?;
            let service = net.percentage_of(service_rate)?;
            let tax_on_service = service.percentage_of(tax_rate)?;
            let tax = tax_on_lines.checked_add(tax_on_service)?;
            let total = net.checked_add(service)?.checked_add(tax)?;
            (net, service, tax, total)
        };

        Ok(Self {
            line_total,
            net,
            service_charge,
            tax,
            total,
            tax_rate,
            service_rate,
            tax_inclusive: config.tax_enabled && config.tax_inclusive,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(minor: i64) -> Money {
        Money::from_minor(minor)
    }

    const VAT_15_INCLUSIVE: ChargeConfig = ChargeConfig {
        tax_enabled: true,
        tax_rate: BasisPoints(1500),
        tax_inclusive: true,
        service_enabled: true,
        service_rate: BasisPoints(1000),
    };

    const VAT_15_EXCLUSIVE: ChargeConfig = ChargeConfig {
        tax_enabled: true,
        tax_rate: BasisPoints(1500),
        tax_inclusive: false,
        service_enabled: true,
        service_rate: BasisPoints(1000),
    };

    const SERVICE_ONLY: ChargeConfig = ChargeConfig {
        tax_enabled: false,
        tax_rate: BasisPoints(1500),
        tax_inclusive: true,
        service_enabled: true,
        service_rate: BasisPoints(1000),
    };

    #[test]
    fn no_tax_just_adds_the_service_charge() {
        let bill = Bill::calculate(m(100_000), &SERVICE_ONLY).unwrap();
        assert_eq!(bill.net, m(100_000));
        assert_eq!(bill.service_charge, m(10_000));
        assert_eq!(bill.tax, Money::ZERO);
        assert_eq!(bill.total, m(110_000));
    }

    #[test]
    fn nothing_enabled_leaves_the_total_alone() {
        let bill = Bill::calculate(m(100_000), &ChargeConfig::NONE).unwrap();
        assert_eq!(bill.total, m(100_000));
        assert_eq!(bill.service_charge, Money::ZERO);
        assert_eq!(bill.tax, Money::ZERO);
        assert!(!bill.tax_inclusive);
    }

    #[test]
    fn exclusive_tax_falls_on_the_service_charge_too() {
        // 1000.00 lines, 10% service = 100.00, 15% VAT on 1100.00 = 165.00.
        let bill = Bill::calculate(m(100_000), &VAT_15_EXCLUSIVE).unwrap();
        assert_eq!(bill.net, m(100_000));
        assert_eq!(bill.service_charge, m(10_000));
        assert_eq!(bill.tax, m(16_500));
        assert_eq!(bill.total, m(126_500));
    }

    #[test]
    fn inclusive_tax_is_extracted_then_service_is_taxed() {
        // The receipt shown in the UI mockup: 2620.00 of drinks.
        //   net            = 2620.00 / 1.15 = 2278.26
        //   tax on lines   = 2620.00 - 2278.26 = 341.74   (by subtraction)
        //   service        = 10% of 2278.26 = 227.83
        //   tax on service = 15% of 227.83 = 34.17
        //   tax            = 341.74 + 34.17 = 375.91
        //   total          = 2278.26 + 227.83 + 375.91 = 2882.00
        let bill = Bill::calculate(m(262_000), &VAT_15_INCLUSIVE).unwrap();
        assert_eq!(bill.net, m(227_826));
        assert_eq!(bill.service_charge, m(22_783));
        assert_eq!(bill.tax, m(37_591));
        assert_eq!(bill.total, m(288_200));
    }

    #[test]
    fn inclusive_with_no_service_returns_the_menu_price_exactly() {
        // The bug that made a 1000.00 menu price bill 1000.01: with no service
        // charge, an inclusive bill MUST come back to the menu total.
        let config = ChargeConfig { service_enabled: false, ..VAT_15_INCLUSIVE };
        for line_minor in [100_000i64, 262_000, 1, 99, 12_345] {
            let bill = Bill::calculate(m(line_minor), &config).unwrap();
            assert_eq!(
                bill.total,
                m(line_minor),
                "an inclusive bill with no service charge must equal the menu total"
            );
            assert_eq!(bill.net.checked_add(bill.tax).unwrap(), m(line_minor));
        }
    }

    #[test]
    fn every_bill_adds_up_to_its_own_total() {
        // The arithmetic a suspicious owner does by hand on the printed slip.
        let configs = [ChargeConfig::NONE, SERVICE_ONLY, VAT_15_EXCLUSIVE, VAT_15_INCLUSIVE];
        for config in configs {
            for line_minor in 0..1_500i64 {
                let bill = Bill::calculate(m(line_minor), &config).unwrap();
                let parts = bill
                    .net
                    .checked_add(bill.service_charge)
                    .unwrap()
                    .checked_add(bill.tax)
                    .unwrap();
                assert_eq!(
                    parts, bill.total,
                    "subtotal + service + tax must equal the total ({line_minor}, {config:?})"
                );
            }
        }
    }

    #[test]
    fn a_zero_bill_stays_zero_in_every_mode() {
        for config in [ChargeConfig::NONE, SERVICE_ONLY, VAT_15_EXCLUSIVE, VAT_15_INCLUSIVE] {
            let bill = Bill::calculate(Money::ZERO, &config).unwrap();
            assert_eq!(bill.total, Money::ZERO);
        }
    }

    #[test]
    fn disabled_charges_report_a_zero_rate_not_the_configured_one() {
        // The rate is snapshotted onto the transaction. Storing 15% on a bill
        // that was never taxed would make the receipt unexplainable.
        let config = ChargeConfig { tax_enabled: false, ..VAT_15_INCLUSIVE };
        let bill = Bill::calculate(m(100_000), &config).unwrap();
        assert_eq!(bill.tax_rate, BasisPoints::ZERO);
        assert_eq!(bill.service_rate, BasisPoints(1000));
    }
}
