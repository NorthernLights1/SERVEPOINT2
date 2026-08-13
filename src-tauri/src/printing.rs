//! Deterministic receipt rendering and a testable printer boundary (D20).
//!
//! Business transactions freeze exact printable bytes before any device I/O.
//! This module deliberately knows nothing about order state transitions: it
//! turns authoritative snapshots into bytes and sends bytes through a small
//! synchronous trait whose outcome can be classified by the protocol layer.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::repo::receipts;
use crate::Money;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceStatus {
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    #[error("printer is unavailable")]
    Unavailable,

    #[error("printer outcome is unknown: {0}")]
    Unknown(String),

    #[error("printer I/O: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, PrintError>;

/// Minimal device seam. Implementations must return only after the device has
/// accepted all bytes or an explicit outcome is known.
pub trait Printer {
    fn status(&mut self) -> Result<DeviceStatus>;
    fn print(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Deterministic development printer. Each call creates one immutable spool
/// file; an existing name is never overwritten.
#[derive(Debug, Clone)]
pub struct FilePrinter {
    directory: PathBuf,
    next_document: u64,
}

impl FilePrinter {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            next_document: 1,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn next_path(&self) -> PathBuf {
        self.directory
            .join(format!("print-{:06}.bin", self.next_document))
    }

    fn advance(&mut self) -> Result<()> {
        self.next_document = self
            .next_document
            .checked_add(1)
            .ok_or_else(|| PrintError::Unknown("print sequence exhausted".into()))?;
        Ok(())
    }
}

impl Printer for FilePrinter {
    fn status(&mut self) -> Result<DeviceStatus> {
        fs::create_dir_all(&self.directory)?;
        Ok(DeviceStatus::Ready)
    }

    fn print(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Err(PrintError::Unknown("refusing an empty document".into()));
        }
        fs::create_dir_all(&self.directory)?;
        // A till is restarted every night and the counter starts at one again.
        // Step over what an earlier run already spooled rather than refusing a
        // print because the name is taken — `create_new` below still makes the
        // claim itself atomic.
        while self.next_path().exists() {
            self.advance()?;
        }
        let path = self.next_path();
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        self.advance()?;
        Ok(())
    }
}

/// Input already frozen by the order/tab repositories. The renderer accepts
/// no settings lookups, prices, rates, or names that could change mid-print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomerDocument<'a> {
    pub receipt: &'a receipts::Receipt,
    pub business_name: &'a str,
    pub address: &'a str,
    pub phone: &'a str,
    pub venue_tin: &'a str,
    pub tab_code: &'a str,
    pub tab_reference: &'a str,
    pub lines: &'a [CustomerLine],
    pub currency_code: &'a str,
    pub footer: &'a str,
    pub bank_accounts: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomerLine {
    pub name: String,
    pub quantity_milli: i64,
    pub line_total: Money,
}

/// Human-readable canonical text. ESC/POS framing is a separate step so the
/// same content is usable on screen, in tests, and through a file printer.
pub fn render_customer(document: &CustomerDocument<'_>) -> Result<String> {
    let receipt = document.receipt;
    if receipt.kind != receipts::Kind::Customer {
        return Err(PrintError::Unknown(
            "customer rendering requires a customer receipt snapshot".into(),
        ));
    }
    let subtotal = required_money(receipt.subtotal, "subtotal")?;
    let service = required_money(receipt.service_charge, "service charge")?;
    let tax = required_money(receipt.tax, "tax")?;
    let total = required_money(receipt.total, "total")?;
    let cashier = receipt
        .cashier_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| PrintError::Unknown("customer receipt has no frozen cashier".into()))?;
    for (name, value) in [
        ("receipt number", receipt.receipt_number.as_str()),
        ("tab code", document.tab_code),
        ("tab reference", document.tab_reference),
        ("waiter", receipt.waiter_name.as_str()),
        ("cashier", cashier),
    ] {
        require_single_line(name, value)?;
    }

    let mut out = String::new();
    push_nonblank(&mut out, document.business_name);
    push_label(&mut out, "Address", document.address);
    push_label(&mut out, "Phone", document.phone);
    push_label(&mut out, "TIN", document.venue_tin);
    out.push_str(&format!("CUSTOMER RECEIPT {}\n", receipt.receipt_number));
    out.push_str(&format!(
        "Tab: {} — {}\n",
        document.tab_code, document.tab_reference
    ));
    out.push_str(&format!(
        "Waiter: {}\nCashier: {cashier}\n",
        receipt.waiter_name
    ));
    if let Some(customer_tin) = receipt.customer_tin.as_deref() {
        push_label(&mut out, "Customer TIN", customer_tin);
    }
    out.push_str("--------------------------------\n");
    for line in document.lines {
        require_single_line("sale item", &line.name)?;
        if line.quantity_milli <= 0 || line.line_total.is_negative() {
            return Err(PrintError::Unknown(
                "customer receipt contains an invalid line".into(),
            ));
        }
        out.push_str(&format!(
            "{} x{}  {} {}\n",
            line.name.trim(),
            format_quantity(line.quantity_milli),
            line.line_total.to_display(),
            document.currency_code.trim()
        ));
    }
    money_line(&mut out, "Subtotal", subtotal, document.currency_code);
    if !service.is_zero() {
        let rate = receipt
            .service_rate
            .ok_or_else(|| PrintError::Unknown("service charge has no frozen rate".into()))?;
        money_line(
            &mut out,
            &format!("Service ({rate})"),
            service,
            document.currency_code,
        );
    }
    if !tax.is_zero() {
        let rate = receipt
            .tax_rate
            .ok_or_else(|| PrintError::Unknown("tax has no frozen rate".into()))?;
        money_line(
            &mut out,
            &format!("Tax ({rate})"),
            tax,
            document.currency_code,
        );
    }
    money_line(&mut out, "TOTAL", total, document.currency_code);
    if receipt.is_comped {
        out.push_str("*** COMPLIMENTARY ***\n");
    }
    push_nonblank(&mut out, document.bank_accounts);
    push_nonblank(&mut out, document.footer);
    Ok(out)
}

pub fn render_issue(
    receipt_number: &str,
    destination: receipts::Destination,
    tab_code: &str,
    tab_reference: &str,
    waiter_name: &str,
    lines: &[(String, i64)],
) -> Result<String> {
    if receipt_number.trim().is_empty()
        || tab_code.trim().is_empty()
        || tab_reference.trim().is_empty()
        || waiter_name.trim().is_empty()
        || lines.is_empty()
    {
        return Err(PrintError::Unknown(
            "issue receipt facts are incomplete".into(),
        ));
    }
    for (name, value) in [
        ("receipt number", receipt_number),
        ("tab code", tab_code),
        ("tab reference", tab_reference),
        ("waiter", waiter_name),
    ] {
        require_single_line(name, value)?;
    }
    let heading = match destination {
        receipts::Destination::Bar => "BAR ISSUE RECEIPT",
        receipts::Destination::Kitchen => "KITCHEN ISSUE RECEIPT",
    };
    let mut out = format!(
        "{heading}\nNOT A CUSTOMER RECEIPT\n{}\nTab: {} — {}\nWaiter: {}\n--------------------------------\n",
        receipt_number.trim(),
        tab_code.trim(),
        tab_reference.trim(),
        waiter_name.trim()
    );
    for (name, quantity_milli) in lines {
        require_single_line("sale item", name)?;
        if *quantity_milli <= 0 {
            return Err(PrintError::Unknown(
                "issue receipt contains an invalid line".into(),
            ));
        }
        out.push_str(&format!(
            "{} x{}\n",
            name.trim(),
            format_quantity(*quantity_milli)
        ));
    }
    out.push_str("Bartender: keep this slip\n");
    Ok(out)
}

/// Mark replacement paper so the bartender knows the earlier slip is dead.
pub fn mark_replacement(text: &str, previous_receipt: &str) -> Result<String> {
    require_single_line("previous receipt", previous_receipt)?;
    let marker = format!(
        "REPLACES {} — PREVIOUS SLIP IS VOID\n",
        previous_receipt.trim()
    );
    let split = text
        .find("--------------------------------\n")
        .ok_or_else(|| {
            PrintError::Unknown("replacement issue receipt has no body divider".into())
        })?;
    let insert_at = split + "--------------------------------\n".len();
    let mut marked = String::with_capacity(text.len() + marker.len());
    marked.push_str(&text[..insert_at]);
    marked.push_str(&marker);
    marked.push_str(&text[insert_at..]);
    Ok(marked)
}

/// Wrap canonical UTF-8 text for a common ESC/POS printer and append a cut.
/// Device-specific code pages remain a commissioning concern; no lossy
/// transliteration happens silently here.
pub fn escpos_text(text: &str) -> Result<Vec<u8>> {
    if text.trim().is_empty() {
        return Err(PrintError::Unknown(
            "cannot encode an empty document".into(),
        ));
    }
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(PrintError::Unknown(
            "document contains a printer control character".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(text.len() + 8);
    bytes.extend_from_slice(&[0x1b, 0x40]); // initialise
    bytes.extend_from_slice(text.as_bytes());
    if !text.ends_with('\n') {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(&[0x1d, 0x56, 0x00]); // full cut
    Ok(bytes)
}

/// Native ESC/POS model-2 QR commands (`GS ( k`). The caller supplies the
/// already-final payload, e.g. an EthSwitch amount request.
pub fn escpos_qr(payload: &[u8]) -> Result<Vec<u8>> {
    if payload.is_empty() {
        return Err(PrintError::Unknown("QR payload cannot be empty".into()));
    }
    let store_length = payload
        .len()
        .checked_add(3)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| PrintError::Unknown("QR payload is too large".into()))?;
    let [low, high] = store_length.to_le_bytes();
    let mut bytes = vec![
        0x1d, 0x28, 0x6b, 0x04, 0x00, 0x31, 0x41, 0x32, 0x00, // model 2
        0x1d, 0x28, 0x6b, 0x03, 0x00, 0x31, 0x43, 0x06, // size
        0x1d, 0x28, 0x6b, 0x03, 0x00, 0x31, 0x45, 0x31, // ECC M
        0x1d, 0x28, 0x6b, low, high, 0x31, 0x50, 0x30, // store
    ];
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&[0x1d, 0x28, 0x6b, 0x03, 0x00, 0x31, 0x51, 0x30]);
    Ok(bytes)
}

fn required_money(value: Option<Money>, name: &str) -> Result<Money> {
    value.ok_or_else(|| PrintError::Unknown(format!("customer receipt has no frozen {name}")))
}

fn require_single_line(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(PrintError::Unknown(format!(
            "{name} must be non-blank printable text on one line"
        )));
    }
    Ok(())
}

fn push_nonblank(out: &mut String, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        out.push_str(value);
        out.push('\n');
    }
}

fn push_label(out: &mut String, label: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        out.push_str(&format!("{label}: {value}\n"));
    }
}

fn money_line(out: &mut String, label: &str, amount: Money, currency_code: &str) {
    out.push_str(&format!(
        "{label}: {} {}\n",
        amount.to_display(),
        currency_code.trim()
    ));
}

fn format_quantity(quantity_milli: i64) -> String {
    let whole = quantity_milli / 1_000;
    let fraction = quantity_milli % 1_000;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:03}")
            .trim_end_matches('0')
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BasisPoints, Money};

    fn customer_receipt() -> receipts::Receipt {
        receipts::Receipt {
            id: 1,
            kind: receipts::Kind::Customer,
            sequence_no: 7,
            receipt_number: "CR-000007".into(),
            order_id: None,
            tab_id: Some(3),
            destination: None,
            status: receipts::Status::Pending,
            rendered_text: None,
            waiter_name: "Sara".into(),
            cashier_name: Some("Abel".into()),
            customer_tin: Some("CUST-9".into()),
            subtotal: Some(Money::from_minor(10_000)),
            service_charge: Some(Money::from_minor(1_000)),
            tax: Some(Money::from_minor(1_650)),
            total: Some(Money::from_minor(12_650)),
            tax_rate: Some(BasisPoints(1_500)),
            service_rate: Some(BasisPoints(1_000)),
            tax_inclusive: Some(false),
            is_comped: false,
            shift_id: 2,
            created_at: 10,
            printed_at: None,
        }
    }

    #[test]
    fn customer_text_uses_only_frozen_fiscal_facts_and_itemises_vat() {
        let receipt = customer_receipt();
        let text = render_customer(&CustomerDocument {
            receipt: &receipt,
            business_name: "Test Bar",
            address: "",
            phone: "",
            venue_tin: "VENUE-1",
            tab_code: "TAB-000003",
            tab_reference: "Table 7",
            lines: &[CustomerLine {
                name: "Beer".into(),
                quantity_milli: 2_000,
                line_total: Money::from_minor(10_000),
            }],
            currency_code: "ETB",
            footer: "Thank you",
            bank_accounts: "Awash 1234",
        })
        .unwrap();
        assert!(text.contains("CUSTOMER RECEIPT CR-000007"));
        assert!(text.contains("Customer TIN: CUST-9"));
        assert!(text.contains("Service (10%): 10.00 ETB"));
        assert!(text.contains("Tax (15%): 16.50 ETB"));
        assert!(text.contains("TOTAL: 126.50 ETB"));
    }

    #[test]
    fn issue_text_never_contains_money() {
        let text = render_issue(
            "BR-000123",
            receipts::Destination::Bar,
            "TAB-000003",
            "Table 7",
            "Sara",
            &[("Beer".into(), 2_000)],
        )
        .unwrap();
        assert!(text.contains("NOT A CUSTOMER RECEIPT"));
        assert!(text.contains("Beer x2"));
        assert!(!text.contains("ETB"));
        assert!(!text.contains("price"));
    }

    #[test]
    fn escpos_encoding_initialises_prints_and_cuts_and_qr_is_length_exact() {
        let text = escpos_text("hello").unwrap();
        assert!(text.starts_with(&[0x1b, 0x40]));
        assert!(text.ends_with(&[0x1d, 0x56, 0x00]));

        let qr = escpos_qr(b"amount=1250").unwrap();
        let store = qr
            .windows(3)
            .position(|window| window == [0x31, 0x50, 0x30])
            .unwrap();
        assert_eq!(&qr[store + 3..store + 14], b"amount=1250");
        assert!(escpos_text("forged\u{1b}m").is_err());
    }

    #[test]
    fn file_printer_never_overwrites_an_existing_spool_document() {
        let directory =
            std::env::temp_dir().join(format!("servepoint-file-printer-{}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory).unwrap();
        }
        let mut printer = FilePrinter::new(&directory);
        assert_eq!(printer.status().unwrap(), DeviceStatus::Ready);
        printer.print(b"first").unwrap();
        printer.print(b"second").unwrap();
        assert_eq!(
            fs::read(directory.join("print-000001.bin")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(directory.join("print-000002.bin")).unwrap(),
            b"second"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
