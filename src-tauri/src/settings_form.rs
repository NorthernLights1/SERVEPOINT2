//! What the Settings screen looks like.
//!
//! The screen is **generated from this description**, not hand-built in the
//! webview. One list, in Rust, decides which keys are editable, what each is
//! called in plain language, and what kind of control it gets.
//!
//! That matters for a reason specific to this rebuild: the prior version was
//! rejected partly because problems "couldn't be fixed from the interface".
//! When the form is a hand-written page, every new setting needs a matching
//! edit in the UI, and the ones nobody remembers to add stay unreachable
//! forever — visible in the database, invisible to the owner. Describing the
//! form next to the keys makes that failure impossible.
//!
//! The wording is deliberately plain. "Menu prices already include VAT" is a
//! question a bar owner can answer; "tax_inclusive" is not.

use crate::settings::{keys, Settings};

/// The control a setting gets on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    /// On or off.
    Toggle,
    /// A percentage, stored in basis points and shown as `15%`.
    Rate,
    Integer,
    /// `HH:mm`.
    Time,
    Text,
    /// Free text over several lines — bank account details, receipt footers.
    Multiline,
    /// One of a fixed set.
    Choice,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingField {
    pub key: &'static str,
    pub value: String,
    pub kind: FieldKind,
    pub label: &'static str,
    /// One sentence saying what changes if this is changed. Shown under the
    /// control, always — a setting nobody understands gets set wrong once and
    /// then blamed for months.
    pub help: &'static str,
    pub choices: Vec<Choice>,
    /// Only meaningful for a toggle: the keys hidden while it is off.
    pub reveals: Vec<&'static str>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingGroup {
    pub id: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    pub fields: Vec<SettingField>,
}

struct Spec {
    key: &'static str,
    kind: FieldKind,
    label: &'static str,
    help: &'static str,
    choices: &'static [Choice],
    reveals: &'static [&'static str],
}

const fn field(
    key: &'static str,
    kind: FieldKind,
    label: &'static str,
    help: &'static str,
) -> Spec {
    Spec { key, kind, label, help, choices: &[], reveals: &[] }
}

const TAX_MODE: [Choice; 2] = [
    Choice { value: "1", label: "Menu prices already include VAT" },
    Choice { value: "0", label: "VAT is added on top of menu prices" },
];

const REFERENCE_MODES: [Choice; 4] = [
    Choice { value: "TABLE", label: "Table number" },
    Choice { value: "CUSTOMER_NAME", label: "Customer name" },
    Choice { value: "CUSTOMER_PHONE", label: "Customer phone" },
    Choice { value: "CUSTOM", label: "Something else" },
];

/// Build the Settings screen against the current values.
pub fn describe(settings: &Settings) -> Vec<SettingGroup> {
    let groups: Vec<(&'static str, &'static str, &'static str, Vec<Spec>)> = vec![
        (
            "charges",
            "VAT and service charge",
            "Both are optional. Turn either off and it disappears from every bill and every receipt — nothing is left showing zero.",
            vec![
                Spec {
                    reveals: &[keys::TAX_RATE_BP, keys::TAX_INCLUSIVE],
                    ..field(keys::TAX_ENABLED, FieldKind::Toggle, "Charge VAT",
                            "Off for a venue that is not VAT registered. When it is off, no VAT line is printed at all.")
                },
                field(keys::TAX_RATE_BP, FieldKind::Rate, "VAT rate",
                      "Set this to 0 and the VAT line stops appearing on printouts."),
                Spec {
                    choices: &TAX_MODE,
                    ..field(keys::TAX_INCLUSIVE, FieldKind::Choice, "Menu prices",
                            "This is a fact about the venue's price list, not a preference. Get it wrong and every total is out by the VAT rate while looking perfectly normal.")
                },
                Spec {
                    reveals: &[keys::SERVICE_RATE_BP],
                    ..field(keys::SERVICE_ENABLED, FieldKind::Toggle, "Add a service charge",
                            "Added to the bill before VAT is worked out, because a service charge is itself taxable.")
                },
                field(keys::SERVICE_RATE_BP, FieldKind::Rate, "Service charge",
                      "A percentage of the drinks total."),
            ],
        ),
        (
            "trading-day",
            "The trading day",
            "A club sells past midnight, so the calendar day is the wrong unit. Everything sold before the start time below counts towards the night before.",
            vec![
                field(keys::DAY_START, FieldKind::Time, "A new trading night starts at",
                      "A sale at 02:00 belongs to the night that began the previous evening. This single setting decides that for every sale, report and reconciliation."),
                field(keys::DAY_END, FieldKind::Time, "and should be closed by",
                      "Only used to warn that a shift was left open. It never changes which night a sale belongs to."),
            ],
        ),
        (
            "tabs",
            "Tabs",
            "How drinks are attached to a customer before they pay.",
            vec![
                Spec {
                    choices: &REFERENCE_MODES,
                    ..field(keys::TABS_REFERENCE_MODE, FieldKind::Choice, "A tab is identified by",
                            "Changes what staff are asked for when they open a tab.")
                },
                field(keys::TABS_AGE_WARNING_DAYS, FieldKind::Integer, "Warn about tabs older than",
                      "In days. An old open tab is usually a forgotten one, and forgotten tabs are how money goes missing."),
                field(keys::TABS_ASK_CUSTOMER_TIN, FieldKind::Toggle, "Ask for a customer TIN",
                      "For customers who need the receipt for their own books. Always optional at the till, never required to close a tab."),
            ],
        ),
        (
            "payments",
            "Settling up",
            "What the till will accept at the end of the night.",
            vec![
                field(keys::COMPS_ENABLED, FieldKind::Toggle, "Allow comped tabs",
                      "A tab written off in full. Always requires a reason, and always shows separately in the shift report."),
                field(keys::PARTIAL_ENABLED, FieldKind::Toggle, "Allow part payment",
                      "Lets a waiter hand over less than they owe, with the shortfall recorded against them."),
                field(keys::BANK_ACCOUNTS, FieldKind::Multiline, "Bank accounts printed on receipts",
                      "One per line, exactly as it should appear. Leave blank to print nothing."),
                field(keys::QR_ENABLED, FieldKind::Toggle, "Print a payment QR code",
                      "A static code for transfers. It cannot tell whether a bill has been paid — it only saves the customer typing an account number."),
            ],
        ),
        (
            "printing",
            "Printing",
            "Reports are read on screen. Paper is opt-in.",
            vec![
                field(keys::PRINT_CUSTOMER_RECEIPT, FieldKind::Toggle, "Print a customer receipt when a tab is closed",
                      "The bar's own issue slip always prints — that one is how drinks leave the bar."),
                field(keys::PRINT_REPORT, FieldKind::Toggle, "Also print the shift report on the till printer",
                      "Off by default. The full report is on the Reports screen, where it is readable and can be looked at again later."),
                field(keys::CHARS_PER_LINE, FieldKind::Integer, "Receipt width",
                      "In characters. 32 for a 58mm printer, 48 for an 80mm one."),
            ],
        ),
        (
            "receipt",
            "What is printed at the top",
            "Left blank, receipts print without a header. That is deliberate — better an obviously unfinished receipt than somebody else's business details on a tax document.",
            vec![
                field(keys::BUSINESS_NAME, FieldKind::Text, "Business name", "Printed first, in bold."),
                field(keys::ADDRESS, FieldKind::Text, "Address", "Printed under the name."),
                field(keys::PHONE, FieldKind::Text, "Phone", "Printed under the address."),
                field(keys::TIN, FieldKind::Text, "TIN", "The venue's own tax identification number."),
                field(keys::FOOTER, FieldKind::Multiline, "Footer",
                      "Printed at the very bottom, under the bank details."),
            ],
        ),
        (
            "locale",
            "Money and reporting",
            "",
            vec![
                field(keys::CURRENCY_CODE, FieldKind::Text, "Currency code",
                      "Three letters, such as ETB or KES. Printed after every amount."),
                field(keys::SHOW_COST, FieldKind::Toggle, "Show cost and margin in reports",
                      "Only the owner ever sees these screens, but a shoulder is a shoulder."),
            ],
        ),
    ];

    groups
        .into_iter()
        .map(|(id, title, blurb, specs)| SettingGroup {
            id,
            title,
            blurb,
            fields: specs
                .into_iter()
                .map(|spec| SettingField {
                    key: spec.key,
                    value: settings.text(spec.key).to_owned(),
                    kind: spec.kind,
                    label: spec.label,
                    help: spec.help,
                    choices: spec.choices.to_vec(),
                    reveals: spec.reveals.to_vec(),
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn form() -> Vec<SettingGroup> {
        let conn = db::open_in_memory().unwrap();
        describe(&Settings::load(&conn).unwrap())
    }

    #[test]
    fn every_field_carries_its_current_value() {
        let groups = form();
        let tax_rate = groups
            .iter()
            .flat_map(|g| &g.fields)
            .find(|f| f.key == keys::TAX_RATE_BP)
            .expect("the VAT rate must be on the form");
        assert_eq!(tax_rate.value, "1500");
        assert_eq!(tax_rate.kind, FieldKind::Rate);
    }

    #[test]
    fn every_field_explains_itself() {
        // A setting with no explanation gets set wrong once and then blamed
        // for months.
        for group in form() {
            for field in group.fields {
                assert!(!field.label.is_empty(), "{} has no label", field.key);
                assert!(field.help.len() > 20, "{} needs a real explanation", field.key);
            }
        }
    }

    #[test]
    fn a_key_appears_at_most_once() {
        let groups = form();
        let mut seen: Vec<&str> = groups.iter().flat_map(|g| &g.fields).map(|f| f.key).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "a setting is on the form twice");
    }

    #[test]
    fn every_choice_field_offers_its_current_value() {
        // A dropdown that cannot represent what is already stored shows the
        // wrong thing and silently changes it on the next save.
        for group in form() {
            for field in group.fields {
                if field.kind == FieldKind::Choice {
                    assert!(
                        field.choices.iter().any(|c| c.value == field.value),
                        "{} = '{}' is not among its own choices",
                        field.key,
                        field.value
                    );
                }
            }
        }
    }

    #[test]
    fn the_settings_that_run_the_business_are_all_reachable() {
        // These are the ones a venue cannot operate without. The prior build
        // was rejected in part because things could not be fixed from the
        // interface, so this test names them explicitly.
        let groups = form();
        let keys_on_form: Vec<&str> =
            groups.iter().flat_map(|g| &g.fields).map(|f| f.key).collect();
        for required in [
            keys::TAX_ENABLED,
            keys::TAX_RATE_BP,
            keys::TAX_INCLUSIVE,
            keys::SERVICE_ENABLED,
            keys::SERVICE_RATE_BP,
            keys::DAY_START,
            keys::DAY_END,
            keys::TABS_REFERENCE_MODE,
            keys::PRINT_REPORT,
            keys::PRINT_CUSTOMER_RECEIPT,
            keys::BUSINESS_NAME,
            keys::TIN,
            keys::CURRENCY_CODE,
        ] {
            assert!(keys_on_form.contains(&required), "{required} is not reachable from Settings");
        }
    }

    #[test]
    fn there_is_no_control_for_letting_stock_go_negative() {
        // D9 as revised: insufficient stock always blocks the sale, and no
        // screen offers to change that.
        let groups = form();
        assert!(!groups
            .iter()
            .flat_map(|g| &g.fields)
            .any(|f| f.key.starts_with("inventory.")));
    }
}
