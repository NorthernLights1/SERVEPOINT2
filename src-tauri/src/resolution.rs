//! Registry of ways out of durable, non-terminal states (D10 / INV-13).
//!
//! This is deliberately typed and centralized.  Recovery UI can enumerate
//! these entries, while an exhaustive `match` means adding a state without a
//! resolution is a compile error.  The identifiers name domain commands, not
//! Tauri wrappers; a later UI may rename buttons without changing the
//! recovery contract.

/// A durable state which can survive a crash or operator interruption and
/// therefore needs an explicit way forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReachableNonTerminalState {
    OpenShift,
    ClosingShift,
    OpenTab,
    ClosedUnreconciledTab,
    DraftOrder,
    PrintingOrder,
    PendingReceipt,
    FailedReceipt,
    UnknownPrintAttempt,
    PendingOrderCorrection,
    DraftStockCount,
    UnfinalizedReconciliation,
}

impl ReachableNonTerminalState {
    /// Kept beside the exhaustive registry so the invariant test and recovery
    /// screen share one inventory of states.
    pub const ALL: &'static [Self] = &[
        Self::OpenShift,
        Self::ClosingShift,
        Self::OpenTab,
        Self::ClosedUnreconciledTab,
        Self::DraftOrder,
        Self::PrintingOrder,
        Self::PendingReceipt,
        Self::FailedReceipt,
        Self::UnknownPrintAttempt,
        Self::PendingOrderCorrection,
        Self::DraftStockCount,
        Self::UnfinalizedReconciliation,
    ];
}

/// Stable identifiers understood by the command layer and recovery UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResolutionCommand {
    BeginShiftClose,
    ResumeShiftClose,
    CloseOrCompTab,
    ReconcileTab,
    RetryOrAbandonDraft,
    ResolveOrderPrint,
    ResolveReceiptPrint,
    RetryFailedReceipt,
    ResolvePrintAttempt,
    ResolvePendingCorrection,
    ApplyOrAbandonStockCount,
    FinalizeReconciliation,
}

impl ResolutionCommand {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeginShiftClose => "begin_shift_close",
            Self::ResumeShiftClose => "resume_shift_close",
            Self::CloseOrCompTab => "close_or_comp_tab",
            Self::ReconcileTab => "reconcile_tab",
            Self::RetryOrAbandonDraft => "retry_or_abandon_draft",
            Self::ResolveOrderPrint => "resolve_order_print",
            Self::ResolveReceiptPrint => "resolve_receipt_print",
            Self::RetryFailedReceipt => "retry_failed_receipt",
            Self::ResolvePrintAttempt => "resolve_print_attempt",
            Self::ResolvePendingCorrection => "resolve_pending_correction",
            Self::ApplyOrAbandonStockCount => "apply_or_abandon_stock_count",
            Self::FinalizeReconciliation => "finalize_reconciliation",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub command: ResolutionCommand,
    /// Human-readable intent for the recovery panel. This is not receipt text
    /// and may be localized when the UI is built.
    pub purpose: &'static str,
}

/// The primary way forward for each durable state.
///
/// The exhaustive match is the important part of INV-13: extending
/// [`ReachableNonTerminalState`] cannot compile until a resolution is chosen.
pub const fn resolution_for(state: ReachableNonTerminalState) -> Resolution {
    use ReachableNonTerminalState as State;
    use ResolutionCommand as Command;

    match state {
        State::OpenShift => Resolution {
            command: Command::BeginShiftClose,
            purpose: "finish trading and start end-of-day",
        },
        State::ClosingShift => Resolution {
            command: Command::ResumeShiftClose,
            purpose: "resume report, backup, and final close",
        },
        State::OpenTab => Resolution {
            command: Command::CloseOrCompTab,
            purpose: "freeze the bill or record an authorised comp",
        },
        State::ClosedUnreconciledTab => Resolution {
            command: Command::ReconcileTab,
            purpose: "allocate the frozen liability to a settlement",
        },
        State::DraftOrder => Resolution {
            command: Command::RetryOrAbandonDraft,
            purpose: "retry issue or abandon the unissued round",
        },
        State::PrintingOrder => Resolution {
            command: Command::ResolveOrderPrint,
            purpose: "confirm print, handwritten issue, or confirmed non-print",
        },
        State::PendingReceipt => Resolution {
            command: Command::ResolveReceiptPrint,
            purpose: "record the durable print outcome",
        },
        State::FailedReceipt => Resolution {
            command: Command::RetryFailedReceipt,
            purpose: "retry the same receipt identity when the printer recovers",
        },
        State::UnknownPrintAttempt => Resolution {
            command: Command::ResolvePrintAttempt,
            purpose: "answer whether paper emerged before another attempt",
        },
        State::PendingOrderCorrection => Resolution {
            command: Command::ResolvePendingCorrection,
            purpose: "complete or abandon the frozen correction intent",
        },
        State::DraftStockCount => Resolution {
            command: Command::ApplyOrAbandonStockCount,
            purpose: "apply the count between shifts or discard the draft",
        },
        State::UnfinalizedReconciliation => Resolution {
            command: Command::FinalizeReconciliation,
            purpose: "finish allocating and seal the settlement",
        },
    }
}
