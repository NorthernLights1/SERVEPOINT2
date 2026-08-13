//! The cashier-facing order correction and void surface.

use crate::commands::CommandError;
use crate::correction::{self, ReplacementLine, ReturnedProduct};
use crate::floor::{self, FloorView, SlipLine};
use crate::repo::{catalogue, orders, receipts, tabs};
use crate::settings::Settings;
use crate::state::AppState;
use crate::{Milli, Money};

type Result<T> = std::result::Result<T, CommandError>;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectableOrderLine {
    pub line_id: i64,
    pub sale_item_id: i64,
    pub name: String,
    pub quantity_milli: i64,
    pub quantity: String,
    pub unit_price: String,
    pub line_total: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductChange {
    pub product_id: i64,
    pub name: String,
    pub unit: String,
    pub before: String,
    pub after: String,
    pub maximum_return_milli: i64,
    pub maximum_return: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectableOrderView {
    pub order_id: i64,
    pub original_total: String,
    pub lines: Vec<CorrectableOrderLine>,
    pub void_changes: Vec<ProductChange>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionPreview {
    pub original_total: String,
    pub replacement_total: String,
    pub product_changes: Vec<ProductChange>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionLineInput {
    pub original_line_id: Option<i64>,
    pub sale_item_id: i64,
    pub quantity_milli: i64,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnInput {
    pub product_id: i64,
    pub returned_milli: i64,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionResult {
    pub view: FloorView,
    pub slips: Vec<SlipLine>,
}

fn order_total(conn: &rusqlite::Connection, order_id: i64) -> Result<Money> {
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(line_total_minor), 0) FROM order_lines WHERE order_id = ?1",
        [order_id],
        |row| row.get(0),
    )?;
    Ok(Money::from_minor(total))
}

fn product_changes(
    conn: &rusqlite::Connection,
    original_order_id: i64,
    replacement_order_id: Option<i64>,
) -> Result<Vec<ProductChange>> {
    correction::deltas(conn, original_order_id, replacement_order_id, &[])?
        .into_iter()
        .map(|line| {
            let product = catalogue::product(conn, line.product_id)?;
            let maximum = if line.delta.is_negative() {
                line.delta.abs()
            } else {
                Milli::ZERO
            };
            Ok(ProductChange {
                product_id: line.product_id,
                name: product.name,
                unit: product.base_unit,
                before: line.before.to_display(),
                after: line.after.to_display(),
                maximum_return_milli: maximum.thousandths(),
                maximum_return: maximum.to_display(),
            })
        })
        .collect()
}

fn find_original(
    conn: &rusqlite::Connection,
    tab_id: i64,
    receipt_number: &str,
) -> Result<orders::Order> {
    let typed = receipt_number.trim();
    if typed.is_empty() {
        return Err(CommandError::refused(
            "Type the BR number from the returned slip.",
        ));
    }
    let receipt = receipts::find_by_number(conn, typed).map_err(|_| {
        CommandError::refused("That BR number is not a printed issue slip for this tab.")
    })?;
    if receipt.kind != receipts::Kind::Issue
        || !matches!(
            receipt.status,
            receipts::Status::Printed | receipts::Status::Failed
        )
    {
        return Err(CommandError::refused(
            "That BR number is not a printed issue slip for this tab.",
        ));
    }
    let order = orders::find(conn, receipt.order_id.unwrap_or_default())?;
    if order.tab_id != tab_id || order.status != orders::Status::Issued {
        return Err(CommandError::refused(
            "That BR number does not belong to a correctable round on this tab.",
        ));
    }
    let tab = tabs::find(conn, tab_id)?;
    if tab.status != tabs::Status::Open {
        return Err(CommandError::refused(
            "Corrections require a tab that is still open.",
        ));
    }
    Ok(order)
}

fn order_view(conn: &rusqlite::Connection, order: &orders::Order) -> Result<CorrectableOrderView> {
    let settings = Settings::load(conn)?;
    let lines = orders::lines(conn, order.id)?
        .into_iter()
        .map(|line| CorrectableOrderLine {
            line_id: line.id,
            sale_item_id: line.sale_item_id,
            name: line.sale_item_name,
            quantity_milli: line.quantity.thousandths(),
            quantity: line.quantity.to_display(),
            unit_price: settings.format_money(line.unit_price),
            line_total: settings.format_money(line.line_total),
        })
        .collect();
    Ok(CorrectableOrderView {
        order_id: order.id,
        original_total: settings.format_money(order_total(conn, order.id)?),
        lines,
        void_changes: product_changes(conn, order.id, None)?,
    })
}

pub fn correction_order(
    state: &AppState,
    tab_id: i64,
    receipt_number: &str,
) -> Result<CorrectableOrderView> {
    floor::require_till(state)?;
    state.with_db(|conn| {
        let order = find_original(conn, tab_id, receipt_number)?;
        order_view(conn, &order)
    })
}

fn replacement_lines(lines: &[CorrectionLineInput]) -> Result<Vec<ReplacementLine>> {
    if lines.is_empty() || lines.iter().any(|line| line.quantity_milli <= 0) {
        return Err(CommandError::refused(
            "A corrected order needs at least one line with a quantity above zero.",
        ));
    }
    Ok(lines
        .iter()
        .map(|line| ReplacementLine {
            original_line_id: line.original_line_id,
            sale_item_id: line.sale_item_id,
            quantity: Milli::from_thousandths(line.quantity_milli),
        })
        .collect())
}

fn returns(inputs: &[ReturnInput]) -> Result<Vec<ReturnedProduct>> {
    if inputs.iter().any(|item| item.returned_milli < 0) {
        return Err(CommandError::refused(
            "A returned quantity cannot be negative.",
        ));
    }
    Ok(inputs
        .iter()
        .filter(|item| item.returned_milli != 0 || !item.note.trim().is_empty())
        .map(|item| ReturnedProduct {
            product_id: item.product_id,
            quantity: Milli::from_thousandths(item.returned_milli),
            note: item.note.trim().to_owned(),
        })
        .collect())
}

/// Preview in a rollback-only transaction so no draft row can leak out.
pub fn preview_correction(
    state: &AppState,
    order_id: i64,
    lines: &[CorrectionLineInput],
) -> Result<CorrectionPreview> {
    let session = floor::require_till(state)?;
    let prepared = replacement_lines(lines)?;
    state.with_db_mut(|conn| {
        let transaction = conn.transaction()?;
        let original = orders::find(&transaction, order_id)?;
        let replacement =
            orders::create_replacement(&transaction, order_id, session.staff_id, now_ms())?;
        for line in &prepared {
            if let Some(line_id) = line.original_line_id {
                orders::add_line_from_original(
                    &transaction,
                    replacement.id,
                    line_id,
                    line.quantity,
                )?;
            } else {
                orders::add_line(
                    &transaction,
                    replacement.id,
                    line.sale_item_id,
                    line.quantity,
                )?;
            }
        }
        let settings = Settings::load(&transaction)?;
        let preview = CorrectionPreview {
            original_total: settings.format_money(order_total(&transaction, original.id)?),
            replacement_total: settings.format_money(order_total(&transaction, replacement.id)?),
            product_changes: product_changes(&transaction, original.id, Some(replacement.id))?,
        };
        transaction.rollback()?;
        Ok(preview)
    })
}

fn slips_of(items: &[receipts::Receipt]) -> Result<Vec<SlipLine>> {
    items
        .iter()
        .map(|receipt| {
            Ok(SlipLine {
                receipt_number: receipt.receipt_number.clone(),
                destination: receipt
                    .destination
                    .map(|destination| destination.as_str().to_owned())
                    .unwrap_or_default(),
                text: receipt.rendered_text.clone().ok_or_else(|| {
                    CommandError::of("DATABASE", "a reserved correction slip has no frozen text")
                })?,
            })
        })
        .collect()
}

pub fn correct_order(
    state: &AppState,
    order_id: i64,
    receipt_number: &str,
    reason: &str,
    lines: &[CorrectionLineInput],
    returned: &[ReturnInput],
) -> Result<CorrectionResult> {
    let session = floor::require_till(state)?;
    let lines = replacement_lines(lines)?;
    let returned = returns(returned)?;
    let now = now_ms();
    let prepared = state.with_db_mut(|conn| {
        let original = orders::find(conn, order_id)?;
        find_original(conn, original.tab_id, receipt_number)?;
        correction::prepare(
            conn,
            correction::NewCorrection {
                original_order_id: order_id,
                issue_receipt_number: receipt_number,
                reason,
                cashier_id: session.staff_id,
                at: now,
            },
            &lines,
            &returned,
        )
        .map_err(CommandError::from)
    })?;
    let slips = slips_of(&prepared.receipts)?;
    let directory = state.with_db(floor::slip_directory)?;
    if let Err(error) = floor::spool(&directory, &slips) {
        return Err(CommandError::of(
            "PRINT_PENDING",
            format!(
                "The replacement slips did not print: {error}. Their numbers and the correction are reserved; resolve them before trading continues."
            ),
        ));
    }
    state.with_db_mut(|conn| {
        correction::complete(
            conn,
            prepared.pending.id,
            session.staff_id,
            now,
            correction::PrintOutcome::Printed,
        )
    })?;
    Ok(CorrectionResult {
        view: state.with_db(|conn| floor::view_of(conn, now))?,
        slips,
    })
}

pub fn void_order(
    state: &AppState,
    order_id: i64,
    receipt_number: &str,
    reason: &str,
    returned: &[ReturnInput],
) -> Result<FloorView> {
    let session = floor::require_till(state)?;
    let returned = returns(returned)?;
    let now = now_ms();
    state.with_db_mut(|conn| -> Result<()> {
        let original = orders::find(conn, order_id)?;
        find_original(conn, original.tab_id, receipt_number)?;
        correction::void(
            conn,
            correction::NewVoid {
                original_order_id: order_id,
                issue_receipt_number: receipt_number,
                reason,
                cashier_id: session.staff_id,
                at: now,
            },
            &returned,
        )?;
        Ok(())
    })?;
    state.with_db(|conn| floor::view_of(conn, now))
}

#[tauri::command]
pub fn cmd_correction_order(
    state: tauri::State<'_, AppState>,
    tab_id: i64,
    receipt_number: String,
) -> Result<CorrectableOrderView> {
    correction_order(&state, tab_id, &receipt_number)
}

#[tauri::command]
pub fn cmd_preview_correction(
    state: tauri::State<'_, AppState>,
    order_id: i64,
    lines: Vec<CorrectionLineInput>,
) -> Result<CorrectionPreview> {
    preview_correction(&state, order_id, &lines)
}

#[tauri::command]
pub fn cmd_correct_order(
    state: tauri::State<'_, AppState>,
    order_id: i64,
    receipt_number: String,
    reason: String,
    lines: Vec<CorrectionLineInput>,
    returns: Vec<ReturnInput>,
) -> Result<CorrectionResult> {
    correct_order(&state, order_id, &receipt_number, &reason, &lines, &returns)
}

#[tauri::command]
pub fn cmd_void_order(
    state: tauri::State<'_, AppState>,
    order_id: i64,
    receipt_number: String,
    reason: String,
    returns: Vec<ReturnInput>,
) -> Result<FloorView> {
    void_order(&state, order_id, &receipt_number, &reason, &returns)
}
