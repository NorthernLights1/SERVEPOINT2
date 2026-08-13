//! ServePoint — offline-first licensed POS for bars and clubs.
//!
//! The organising idea of the whole system (§0.1): **what you count is not
//! what you sell**. One bottle of gin on the shelf is sold as a bottle, as a
//! shot, and inside a cocktail; a recipe converts between the two. Everything
//! else follows from that separation.
//!
//! Two rules govern this crate's design and are worth stating before any code:
//!
//! 1. **Derived state is never stored.** Stock on hand and waiter held
//!    balances are always a `SUM()` over their ledgers. A cached figure is a
//!    number that drifts away from the transactions that produced it, and that
//!    drift was the prior prototype's core failure.
//!
//! 2. **The webview computes nothing.** Every money figure crosses the
//!    boundary already calculated and already formatted. A total that is
//!    rendered in JavaScript is a total that can disagree with the receipt.

pub mod audit;
pub mod auth;
pub mod backup;
pub mod bill;
pub mod bills;
pub mod calendar;
pub mod commands;
pub mod commissioning;
pub mod correction;
pub mod corrections;
pub mod db;
pub mod floor;
pub mod ledger;
pub mod money;
pub mod overview;
pub mod printing;
pub mod quantity;
pub mod reconcile;
pub mod recovery;
pub mod repo;
pub mod resolution;
pub mod settings;
pub mod settings_form;
pub mod settlement;
pub mod state;
pub mod trading;
pub mod venue;

pub use money::{BasisPoints, Money};
pub use quantity::Milli;
pub use settings::Settings;

#[cfg(test)]
mod invariant_tests;
