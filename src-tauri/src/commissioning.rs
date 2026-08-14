//! Owner-authorised, atomic commissioning and master-data operations.
//!
//! Repository functions are statement-level by design. This service owns the
//! transaction that couples each mutable master fact to its audit entry.

use rusqlite::{Connection, TransactionBehavior};

use crate::ledger::{self, Event};
use crate::repo::{self, catalogue, seq, staff, stock};
use crate::{Milli, Money};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum BaseUnit {
    Bottle,
    Shot,
    Unit,
}

impl BaseUnit {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bottle => "BOTTLE",
            Self::Shot => "SHOT",
            Self::Unit => "UNIT",
        }
    }
}

/// What one counted unit is physically made of.
///
/// This is the industry model — Restaurant365 fixes a measure type per item,
/// and pour-cost tools cost recipes in ml or oz — and it exists because a pour
/// size belongs to the drink, not the bottle: a single is 30ml, a double 60,
/// a cocktail 45. `None` is anything handed over whole, which is most of a
/// club's list and stays one-tap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Measure {
    #[default]
    None,
    Ml,
    Gram,
}

impl Measure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Ml => "ML",
            Self::Gram => "GRAM",
        }
    }

    pub const fn is_measured(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Destination {
    Bar,
    Kitchen,
}

impl Destination {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bar => "BAR",
            Self::Kitchen => "KITCHEN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub entity_id: i64,
    pub audit_sequence: i64,
}

// Codes are allocated here, never supplied. `create_*` takes one from the
// sequence inside its own transaction; `update_*` leaves the existing code
// alone, because a code is identity and rewriting identity breaks anything
// already counted or printed against it.

#[derive(Clone, Debug)]
pub struct NewStaff {
    pub name: String,
    pub role: staff::Role,
    pub pin: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewProduct {
    /// Put it on the menu at this price as it is created, sold one for one.
    ///
    /// `None` means stock-only — a mixer, or something poured into other
    /// drinks and never ordered by name.
    pub sale_price: Option<Money>,
    /// `NONE`, `ML` or `GRAM`: what one counted unit is made of, so a recipe
    /// can be written as "30ml" rather than "0.04 of a bottle".
    pub content_measure: Measure,
    /// How much of that measure one counted unit holds — 750 for a 750ml
    /// bottle. Ignored when the measure is `None`.
    pub content_per_unit: Milli,
    pub name: String,
    pub category: String,
    pub base_unit: BaseUnit,
    pub base_units_per_pack: Milli,
    pub units_per_purchase_pack: i64,
    pub low_stock_threshold: Milli,
    pub tracks_inventory: bool,
    pub destination: Destination,
}

#[derive(Clone, Debug)]
pub struct ProductUpdate {
    pub content_measure: Measure,
    pub content_per_unit: Milli,
    pub name: String,
    pub category: String,
    pub base_unit: BaseUnit,
    pub base_units_per_pack: Milli,
    pub units_per_purchase_pack: i64,
    pub low_stock_threshold: Milli,
    pub tracks_inventory: bool,
    pub destination: Destination,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct NewSaleItem {
    pub name: String,
    pub category: String,
}

#[derive(Clone, Debug)]
pub struct SaleItemUpdate {
    pub name: String,
    pub category: String,
    pub active: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct RecipeLine {
    pub product_id: i64,
    /// Counted units when `in_measure` is false — one bottle of beer is
    /// `Milli::ONE`. Otherwise the product's own measure: 30 means 30ml.
    pub quantity: Milli,
    /// True when `quantity` was typed in the product's measure and still has
    /// to be converted. The screen never does that division itself.
    pub in_measure: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CommissioningError {
    #[error("{0}")]
    Repo(#[from] repo::RepoError),

    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("audit: {0}")]
    Audit(#[from] ledger::LedgerError),

    #[error("PIN: {0}")]
    Pin(#[from] crate::auth::PinError),
}

pub type Result<T> = std::result::Result<T, CommissioningError>;

fn require_owner(conn: &Connection, actor_id: i64) -> Result<()> {
    let actor = staff::find(conn, actor_id)?;
    if actor.role != staff::Role::Owner || !actor.active {
        return Err(repo::RepoError::Refused(
            "only an active owner may change commissioning or master data".into(),
        )
        .into());
    }
    Ok(())
}

fn audit(
    conn: &Connection,
    actor_id: i64,
    action: &str,
    entity_type: &str,
    entity_id: i64,
    facts: &str,
    at: i64,
) -> Result<i64> {
    let shift = ledger::open_shift_id(conn)?;
    Ok(ledger::append(
        conn,
        &Event::new(action, entity_type, at)
            .about(entity_id)
            .recording(facts)
            .by(actor_id)
            .during(shift),
    )?)
}

fn audit_change(
    conn: &Connection,
    actor_id: i64,
    action: &str,
    entity_type: &str,
    entity_id: i64,
    old: &str,
    new: &str,
    at: i64,
) -> Result<i64> {
    let shift = ledger::open_shift_id(conn)?;
    Ok(ledger::append(
        conn,
        &Event::new(action, entity_type, at)
            .about(entity_id)
            .changed(old, new)
            .by(actor_id)
            .during(shift),
    )?)
}

fn product_facts(product: &catalogue::Product) -> String {
    format!(
        "code={};name={};category={};base_unit={};base_units_per_pack_milli={};\
         units_per_purchase_pack={};low_stock_threshold_milli={};tracks_inventory={};\
         destination={};active={}",
        product.code,
        product.name,
        product.category,
        product.base_unit,
        product.base_units_per_pack,
        product.units_per_purchase_pack,
        product.low_stock_threshold.thousandths(),
        i64::from(product.tracks_inventory),
        product.destination,
        i64::from(product.active)
    )
}

fn sale_item_facts(item: &catalogue::SaleItem) -> String {
    format!(
        "code={};name={};category={};active={}",
        item.code,
        item.name,
        item.category,
        i64::from(item.active)
    )
}

pub fn create_staff(
    conn: &mut Connection,
    actor_id: i64,
    input: &NewStaff,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let pin = match input.pin.as_deref() {
        Some(pin) => {
            crate::auth::validate_pin(pin)?;
            let salt = ledger::random_hex(&tx, 16)?;
            let hash = crate::auth::hash_pin(pin, &salt);
            Some((salt, hash))
        }
        None => None,
    };
    let (_, code) = seq::next(&tx, seq::Counter::Staff)?;
    let id = staff::add(
        &tx,
        &code,
        &input.name,
        input.role,
        pin.as_ref()
            .map(|(salt, hash)| (salt.as_str(), hash.as_str())),
        at,
    )?;
    let facts = format!(
        "code={};name={};role={}",
        code,
        input.name.trim(),
        input.role.as_str()
    );
    let audit_sequence = audit(&tx, actor_id, "STAFF_CREATED", "staff", id, &facts, at)?;
    tx.commit()?;
    Ok(Change {
        entity_id: id,
        audit_sequence,
    })
}

pub fn set_staff_active(
    conn: &mut Connection,
    actor_id: i64,
    staff_id: i64,
    active: bool,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let before = staff::find(&tx, staff_id)?;
    staff::set_active(&tx, staff_id, active, at)?;
    let audit_sequence = audit_change(
        &tx,
        actor_id,
        if active {
            "STAFF_ACTIVATED"
        } else {
            "STAFF_DEACTIVATED"
        },
        "staff",
        staff_id,
        if before.active {
            "active=1"
        } else {
            "active=0"
        },
        if active { "active=1" } else { "active=0" },
        at,
    )?;
    tx.commit()?;
    Ok(Change {
        entity_id: staff_id,
        audit_sequence,
    })
}

/// Take a shelf item off the catalogue, or bring it back.
///
/// **Removal never deletes.** The schema refuses a `DELETE` outright, and it is
/// right to: an order line printed last year names this product, and a vanished
/// row would take that line's meaning with it. Removing sets `active = 0`.
///
/// Two things make removal refuse, and both are cases where going ahead would
/// quietly break something rather than fail loudly:
///
/// * **Stock still on the shelf.** `stock::levels` reads active products only,
///   so removing one with a count still on it drops both the count and its
///   value out of the inventory total with nothing said. Write it off first.
/// * **A live recipe still pours it.** The drink would stay on the menu and
///   fail at the shelf, mid-service.
///
/// Bringing something back is never refused — there is nothing to break.
pub fn set_product_active(
    conn: &mut Connection,
    actor_id: i64,
    product_id: i64,
    active: bool,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let before = catalogue::product(&tx, product_id)?;

    if !active {
        let on_hand = stock::on_hand(&tx, product_id)?;
        if !on_hand.is_zero() {
            return Err(repo::RepoError::Refused(format!(
                "there is still {on_hand} {} of {} on the shelf; write it off first",
                before.base_unit.to_lowercase(),
                before.name
            ))
            .into());
        }
        if let Some(drink) = catalogue::poured_into(&tx, product_id)? {
            return Err(repo::RepoError::Refused(format!(
                "{} is still poured into {drink}; change that recipe first",
                before.name
            ))
            .into());
        }
    }

    catalogue::set_product_active(&tx, product_id, active)?;
    let audit_sequence = audit_change(
        &tx,
        actor_id,
        if active {
            "PRODUCT_RESTORED"
        } else {
            "PRODUCT_REMOVED"
        },
        "product",
        product_id,
        if before.active {
            "active=1"
        } else {
            "active=0"
        },
        if active { "active=1" } else { "active=0" },
        at,
    )?;
    tx.commit()?;
    Ok(Change {
        entity_id: product_id,
        audit_sequence,
    })
}

/// Take a drink off the menu, or put it back. See [`set_product_active`].
///
/// The one refusal is a drink sitting on a tab nobody has settled: pulling it
/// mid-service leaves the floor holding a line the till no longer sells. Tabs
/// already settled are history, and hold it back from nothing.
pub fn set_sale_item_active(
    conn: &mut Connection,
    actor_id: i64,
    sale_item_id: i64,
    active: bool,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let before = catalogue::sale_item(&tx, sale_item_id)?;

    if !active && catalogue::on_an_open_tab(&tx, sale_item_id)? {
        return Err(repo::RepoError::Refused(format!(
            "{} is on a tab that is still open; settle it first",
            before.name
        ))
        .into());
    }

    catalogue::set_sale_item_active(&tx, sale_item_id, active)?;
    let audit_sequence = audit_change(
        &tx,
        actor_id,
        if active {
            "SALE_ITEM_RESTORED"
        } else {
            "SALE_ITEM_REMOVED"
        },
        "sale_item",
        sale_item_id,
        if before.active {
            "active=1"
        } else {
            "active=0"
        },
        if active { "active=1" } else { "active=0" },
        at,
    )?;
    tx.commit()?;
    Ok(Change {
        entity_id: sale_item_id,
        audit_sequence,
    })
}

/// Put an existing shelf item on the menu, sold one for one.
///
/// Writes the same three rows the long way round would — menu entry, recipe,
/// price — and the same three audit entries, so history cannot tell whether
/// the owner used one screen or three. The only extra fact is `from_product_id`,
/// which lets the screen show the pair as a single row.
fn sell_within(
    tx: &rusqlite::Transaction<'_>,
    actor_id: i64,
    product_id: i64,
    price: Money,
    at: i64,
) -> Result<(i64, i64)> {
    let product = catalogue::product(tx, product_id)?;
    let (_, code) = seq::next(tx, seq::Counter::SaleItem)?;
    let sale_item_id = catalogue::add_sale_item(
        tx,
        &catalogue::NewSaleItem {
            code: &code,
            name: &product.name,
            category: &product.category,
        },
        at,
    )?;
    catalogue::link_twin(tx, sale_item_id, product_id)?;
    audit(
        tx,
        actor_id,
        "SALE_ITEM_CREATED",
        "sale_item",
        sale_item_id,
        &format!(
            "code={code};name={};category={};from_product_id={product_id}",
            product.name.trim(),
            product.category.trim()
        ),
        at,
    )?;

    // One base unit off the shelf per sale. That is the whole meaning of the
    // pairing, written as an ordinary recipe so that everything downstream —
    // availability, issue slips, corrections — keeps working unchanged.
    let recipe_id =
        catalogue::revise_recipe(tx, sale_item_id, &[(product_id, Milli::ONE)], at, actor_id)?;
    audit(
        tx,
        actor_id,
        "RECIPE_CHANGED",
        "recipe",
        recipe_id,
        &format!("sale_item_id={sale_item_id};line_count=1"),
        at,
    )?;

    catalogue::reprice(tx, sale_item_id, price, at, actor_id)?;
    let price_id = tx.last_insert_rowid();
    let audit_sequence = audit(
        tx,
        actor_id,
        "PRICE_CHANGED",
        "price",
        price_id,
        &format!("sale_item_id={sale_item_id};price_minor={}", price.minor()),
        at,
    )?;
    Ok((sale_item_id, audit_sequence))
}

/// Start selling something that was created as stock only.
pub fn sell_product(
    conn: &mut Connection,
    actor_id: i64,
    product_id: i64,
    price: Money,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let (sale_item_id, audit_sequence) = sell_within(&tx, actor_id, product_id, price, at)?;
    tx.commit()?;
    Ok(Change {
        entity_id: sale_item_id,
        audit_sequence,
    })
}

/// Put a shelf item on the menu sold by the measure — a shot poured from a
/// bottle.
///
/// The same three rows and the same three audit entries as [`sell_within`],
/// differing only in what the recipe draws: a fraction of a counted unit
/// rather than a whole one.
///
/// It lives here, in one transaction, for a reason this venue proved the hard
/// way. Built from the screen as three separate commands, a shot could end up
/// with a menu entry and no recipe, or a recipe and no price — an item that
/// looks finished in the catalogue and is refused at the till, with nothing on
/// either screen saying why.
fn sell_measure_within(
    tx: &rusqlite::Transaction<'_>,
    actor_id: i64,
    product_id: i64,
    poured: Milli,
    price: Money,
    at: i64,
) -> Result<(i64, i64)> {
    let product = catalogue::product(tx, product_id)?;
    if product.content_measure == "NONE" || product.content_per_unit.thousandths() <= 0 {
        return Err(repo::RepoError::Refused(format!(
            "{} does not say how much one holds, so it cannot be sold by the measure.",
            product.name.trim()
        ))
        .into());
    }
    // Rust turns the poured amount into the fraction of a bottle a recipe line
    // stores. The screen sends "30", never 0.04.
    let draws = catalogue::measure_to_units(poured, product.content_per_unit)?;
    let unit = if product.content_measure == "ML" {
        "ml"
    } else {
        "g"
    };
    let name = format!("{} ({}{unit})", product.name.trim(), poured.to_display());

    let (_, code) = seq::next(tx, seq::Counter::SaleItem)?;
    let sale_item_id = catalogue::add_sale_item(
        tx,
        &catalogue::NewSaleItem {
            code: &code,
            name: &name,
            category: &product.category,
        },
        at,
    )?;
    audit(
        tx,
        actor_id,
        "SALE_ITEM_CREATED",
        "sale_item",
        sale_item_id,
        &format!(
            "code={code};name={name};category={};poured_from={product_id}",
            product.category.trim()
        ),
        at,
    )?;

    let recipe_id =
        catalogue::revise_recipe(tx, sale_item_id, &[(product_id, draws)], at, actor_id)?;
    audit(
        tx,
        actor_id,
        "RECIPE_CHANGED",
        "recipe",
        recipe_id,
        &format!("sale_item_id={sale_item_id};line_count=1"),
        at,
    )?;

    catalogue::reprice(tx, sale_item_id, price, at, actor_id)?;
    let price_id = tx.last_insert_rowid();
    let audit_sequence = audit(
        tx,
        actor_id,
        "PRICE_CHANGED",
        "price",
        price_id,
        &format!("sale_item_id={sale_item_id};price_minor={}", price.minor()),
        at,
    )?;
    Ok((sale_item_id, audit_sequence))
}

/// Start selling a measure of something already on the shelf.
pub fn sell_by_measure(
    conn: &mut Connection,
    actor_id: i64,
    product_id: i64,
    poured: Milli,
    price: Money,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let (sale_item_id, audit_sequence) =
        sell_measure_within(&tx, actor_id, product_id, poured, price, at)?;
    tx.commit()?;
    Ok(Change {
        entity_id: sale_item_id,
        audit_sequence,
    })
}

pub fn create_product(
    conn: &mut Connection,
    actor_id: i64,
    input: &NewProduct,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let (_, code) = seq::next(&tx, seq::Counter::Product)?;
    let id = catalogue::add_product(
        &tx,
        &catalogue::NewProduct {
            code: &code,
            name: &input.name,
            category: &input.category,
            base_unit: input.base_unit.as_str(),
            base_units_per_pack: input.base_units_per_pack,
            units_per_purchase_pack: input.units_per_purchase_pack,
            low_stock_threshold: input.low_stock_threshold,
            tracks_inventory: input.tracks_inventory,
            destination: input.destination.as_str(),
            content_measure: input.content_measure.as_str(),
            content_per_unit: input.content_per_unit,
        },
        at,
    )?;
    let facts = format!(
        "code={};name={};base_unit={};base_units_per_pack_milli={};tracks_inventory={};destination={}",
        code,
        input.name.trim(),
        input.base_unit.as_str(),
        input.base_units_per_pack.thousandths(),
        i64::from(input.tracks_inventory),
        input.destination.as_str()
    );
    let mut audit_sequence = audit(&tx, actor_id, "PRODUCT_CREATED", "product", id, &facts, at)?;

    // Shelf item and menu entry commit together or not at all. Splitting them
    // across two commands is what leaves a product that looks finished on the
    // screen and is refused at the till.
    if let Some(price) = input.sale_price {
        let (_, last) = sell_within(&tx, actor_id, id, price, at)?;
        audit_sequence = last;
    }

    tx.commit()?;
    Ok(Change {
        entity_id: id,
        audit_sequence,
    })
}

pub fn update_product(
    conn: &mut Connection,
    actor_id: i64,
    product_id: i64,
    input: &ProductUpdate,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let before = catalogue::product(&tx, product_id)?;
    catalogue::update_product(
        &tx,
        product_id,
        &catalogue::NewProduct {
            // Identity, carried through untouched.
            code: &before.code,
            name: &input.name,
            category: &input.category,
            base_unit: input.base_unit.as_str(),
            base_units_per_pack: input.base_units_per_pack,
            units_per_purchase_pack: input.units_per_purchase_pack,
            low_stock_threshold: input.low_stock_threshold,
            tracks_inventory: input.tracks_inventory,
            destination: input.destination.as_str(),
            content_measure: input.content_measure.as_str(),
            content_per_unit: input.content_per_unit,
        },
        input.active,
    )?;
    let after = catalogue::product(&tx, product_id)?;
    let old = product_facts(&before);
    let new = product_facts(&after);
    let mut audit_sequence = audit_change(
        &tx,
        actor_id,
        "PRODUCT_CHANGED",
        "product",
        product_id,
        &old,
        &new,
        at,
    )?;

    // A product sold one for one shares its name with its menu entry, copied
    // across when the pair was made. Renaming only the shelf would leave the
    // till still offering the old name — the rename would look done and be
    // half done. The entry's own active flag is left alone: what is on the
    // menu is a separate question from what is on the shelf.
    if let Some(sale_item_id) = catalogue::twin_of(&tx, product_id)? {
        let twin = catalogue::sale_item(&tx, sale_item_id)?;
        catalogue::update_sale_item(
            &tx,
            sale_item_id,
            &catalogue::NewSaleItem {
                code: &twin.code,
                name: &input.name,
                category: &input.category,
            },
            twin.active,
        )?;
        let renamed = catalogue::sale_item(&tx, sale_item_id)?;
        audit_sequence = audit_change(
            &tx,
            actor_id,
            "SALE_ITEM_CHANGED",
            "sale_item",
            sale_item_id,
            &sale_item_facts(&twin),
            &sale_item_facts(&renamed),
            at,
        )?;
    }

    tx.commit()?;
    Ok(Change {
        entity_id: product_id,
        audit_sequence,
    })
}

pub fn create_sale_item(
    conn: &mut Connection,
    actor_id: i64,
    input: &NewSaleItem,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let (_, code) = seq::next(&tx, seq::Counter::SaleItem)?;
    let id = catalogue::add_sale_item(
        &tx,
        &catalogue::NewSaleItem {
            code: &code,
            name: &input.name,
            category: &input.category,
        },
        at,
    )?;
    let facts = format!(
        "code={};name={};category={}",
        code,
        input.name.trim(),
        input.category.trim()
    );
    let audit_sequence = audit(
        &tx,
        actor_id,
        "SALE_ITEM_CREATED",
        "sale_item",
        id,
        &facts,
        at,
    )?;
    tx.commit()?;
    Ok(Change {
        entity_id: id,
        audit_sequence,
    })
}

pub fn update_sale_item(
    conn: &mut Connection,
    actor_id: i64,
    sale_item_id: i64,
    input: &SaleItemUpdate,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    let before = catalogue::sale_item(&tx, sale_item_id)?;
    catalogue::update_sale_item(
        &tx,
        sale_item_id,
        &catalogue::NewSaleItem {
            // Identity, carried through untouched.
            code: &before.code,
            name: &input.name,
            category: &input.category,
        },
        input.active,
    )?;
    let after = catalogue::sale_item(&tx, sale_item_id)?;
    let old = sale_item_facts(&before);
    let new = sale_item_facts(&after);
    let audit_sequence = audit_change(
        &tx,
        actor_id,
        "SALE_ITEM_CHANGED",
        "sale_item",
        sale_item_id,
        &old,
        &new,
        at,
    )?;
    tx.commit()?;
    Ok(Change {
        entity_id: sale_item_id,
        audit_sequence,
    })
}

pub fn revise_recipe(
    conn: &mut Connection,
    actor_id: i64,
    sale_item_id: i64,
    lines: &[RecipeLine],
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    // A line typed as "30ml" becomes the fraction of a bottle the ledger
    // holds. Converted here rather than on the screen, and stored converted,
    // so that everything downstream keeps reading one unit.
    let lines: Vec<(i64, Milli)> = lines
        .iter()
        .map(|line| {
            let quantity = if line.in_measure {
                let product = catalogue::product(&tx, line.product_id)?;
                catalogue::measure_to_units(line.quantity, product.content_per_unit)?
            } else {
                line.quantity
            };
            Ok((line.product_id, quantity))
        })
        .collect::<Result<_>>()?;
    let id = catalogue::revise_recipe(&tx, sale_item_id, &lines, at, actor_id)?;
    let facts = format!("sale_item_id={sale_item_id};line_count={}", lines.len());
    let audit_sequence = audit(&tx, actor_id, "RECIPE_CHANGED", "recipe", id, &facts, at)?;
    tx.commit()?;
    Ok(Change {
        entity_id: id,
        audit_sequence,
    })
}

pub fn reprice(
    conn: &mut Connection,
    actor_id: i64,
    sale_item_id: i64,
    price: Money,
    at: i64,
) -> Result<Change> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_owner(&tx, actor_id)?;
    catalogue::reprice(&tx, sale_item_id, price, at, actor_id)?;
    let id = tx.last_insert_rowid();
    let facts = format!("sale_item_id={sale_item_id};price_minor={}", price.minor());
    let audit_sequence = audit(&tx, actor_id, "PRICE_CHANGED", "price", id, &facts, at)?;
    tx.commit()?;
    Ok(Change {
        entity_id: id,
        audit_sequence,
    })
}
