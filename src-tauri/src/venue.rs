//! What the venue is made of — the owner's side of the counter.
//!
//! Staff, products, the menu, recipes and prices. Every rule
//! and every audit entry belongs to `commissioning`; this module is the window
//! onto it, exactly as `floor` is the window onto `trading`.
//!
//! A till cannot sell anything until this screen has been used at least once:
//! a fresh install has no waiters and an empty catalogue.

use rusqlite::Connection;

use crate::commands::{require_owner, require_session, CommandError};
use crate::commissioning::{self, BaseUnit, Destination, Measure};
use crate::repo::{catalogue, staff, stock};
use crate::settings::Settings;
use crate::state::AppState;
use crate::{Milli, Money};

type Result<T> = std::result::Result<T, CommandError>;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Quantities cross this boundary as thousandths, the same unit the ledger
/// stores, so nothing is rounded on the way in or the way out.
fn milli(thousandths: i64, what: &str) -> Result<Milli> {
    if thousandths <= 0 {
        return Err(CommandError::refused(format!("{what} must be above zero.")));
    }
    Ok(Milli::from_thousandths(thousandths))
}

fn money(text: &str, what: &str) -> Result<Money> {
    Money::parse(text).map_err(|error| CommandError::refused(format!("{what}: {error}")))
}

// ---------------------------------------------------------------------------
// What the frontend sends
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffForm {
    pub name: String,
    /// `OWNER`, `CASHIER` or `WAITER`.
    pub role: String,
    /// Required for roles that sign in, meaningless for a waiter.
    pub pin: Option<String>,
}

const fn yes() -> bool {
    true
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductForm {
    /// What it sells for, as typed: `"120.00"`. Absent or blank means stock
    /// only — it is on the shelf but never ordered by name.
    #[serde(default)]
    pub sale_price: Option<String>,
    /// `NONE`, `ML` or `GRAM` — what one counted unit holds, so recipes can be
    /// written as "30ml" rather than as a fraction of a bottle.
    #[serde(default)]
    pub content_measure: Measure,
    /// How much of that measure is in one counted unit, as thousandths:
    /// 750000 for a 750ml bottle. Ignored when the measure is NONE.
    #[serde(default)]
    pub content_per_unit_milli: i64,
    pub name: String,
    pub category: String,
    pub base_unit: BaseUnit,
    pub base_units_per_pack: i64,
    pub units_per_purchase_pack: i64,
    pub low_stock_threshold: i64,
    pub tracks_inventory: bool,
    pub destination: Destination,
    /// Ignored when creating; a new product is always active.
    #[serde(default = "yes")]
    pub active: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleItemForm {
    pub name: String,
    pub category: String,
    #[serde(default = "yes")]
    pub active: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeLineForm {
    pub product_id: i64,
    /// Counted units, or the product's own measure when `in_measure` is set:
    /// 30000 with `inMeasure` means 30ml, and Rust does the division.
    pub quantity_milli: i64,
    #[serde(default)]
    pub in_measure: bool,
}

// ---------------------------------------------------------------------------
// What the frontend receives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffLine {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub role: String,
    pub active: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductLine {
    pub id: i64,
    /// The menu entry that sells this one for one, when there is one.
    pub sale_item_id: Option<i64>,
    /// What it sells for, already formatted. `None` when it is stock only.
    pub price: Option<String>,
    /// The same amount unformatted — `"1200.00"` — which is what `Money::parse`
    /// accepts back. An edit box filled from `price` would send
    /// `"1,200.00 ETB"` and be refused for something the owner never typed.
    pub price_value: Option<String>,
    /// `NONE`, `ML` or `GRAM`.
    pub content_measure: String,
    /// How much one counted unit holds, formatted: `"750"`. Empty when none.
    pub content_per_unit: String,
    pub code: String,
    pub name: String,
    pub category: String,
    pub base_unit: String,
    pub base_units_per_pack: String,
    pub units_per_purchase_pack: i64,
    pub low_stock_threshold: String,
    pub tracks_inventory: bool,
    pub destination: String,
    pub active: bool,
    pub on_hand: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeLineView {
    pub product_id: i64,
    pub name: String,
    pub quantity_milli: i64,
    pub quantity: String,
    /// `NONE`, `ML` or `GRAM`.
    pub measure: String,
    /// The same amount read back in that measure — `"30"` for 30ml — so the
    /// screen shows what was typed rather than a fraction of a bottle. Empty
    /// when the product has no measure.
    pub measure_quantity: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleItemLine {
    pub id: i64,
    /// Set when this entry is one shelf item sold one for one, in which case
    /// the Items card already shows it and the composed card should not.
    pub from_product_id: Option<i64>,
    pub code: String,
    pub name: String,
    pub category: String,
    pub active: bool,
    /// `None` until somebody prices it.
    pub price: Option<String>,
    /// The same amount unformatted, for an edit box. See [`ProductLine`].
    pub price_value: Option<String>,
    pub recipe: Vec<RecipeLineView>,
    /// Active, priced and with a recipe — the three things the till needs.
    pub sellable: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupView {
    pub staff: Vec<StaffLine>,
    pub products: Vec<ProductLine>,
    pub sale_items: Vec<SaleItemLine>,
    /// True once the till could actually serve somebody: at least one waiter
    /// to hold a tab and at least one thing that can be sold.
    pub can_trade: bool,
    /// What is missing, in the order it needs doing. Empty once ready.
    pub missing: Vec<String>,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn staff_lines(conn: &Connection) -> Result<Vec<StaffLine>> {
    let mut statement = conn.prepare(
        "SELECT id, code, full_name, role, active FROM staff
          ORDER BY CASE role WHEN 'OWNER' THEN 0 WHEN 'CASHIER' THEN 1 ELSE 2 END, full_name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StaffLine {
            id: row.get(0)?,
            code: row.get(1)?,
            name: row.get(2)?,
            role: row.get(3)?,
            active: row.get::<_, i64>(4)? == 1,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn product_lines(conn: &Connection, settings: &Settings) -> Result<Vec<ProductLine>> {
    // One query for every twin and its live price, rather than two per product.
    let twins: std::collections::BTreeMap<i64, (i64, Option<i64>)> = {
        let mut statement = conn.prepare(
            "SELECT s.from_product_id, s.id, p.price_minor
               FROM sale_items s
               LEFT JOIN prices p ON p.sale_item_id = s.id AND p.effective_to IS NULL
              WHERE s.from_product_id IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, Option<i64>>(2)?),
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    catalogue::every_product(conn)?
        .into_iter()
        .map(|product| {
            let twin = twins.get(&product.id).copied();
            Ok(ProductLine {
                on_hand: stock::on_hand(conn, product.id)?.to_display(),
                sale_item_id: twin.map(|(sale_item_id, _)| sale_item_id),
                price: twin
                    .and_then(|(_, minor)| minor)
                    .map(|minor| settings.format_money(Money::from_minor(minor))),
                price_value: twin
                    .and_then(|(_, minor)| minor)
                    .map(|minor| Money::from_minor(minor).to_display()),
                id: product.id,
                content_per_unit: if product.content_measure == "NONE" {
                    String::new()
                } else {
                    product.content_per_unit.to_display()
                },
                content_measure: product.content_measure,
                code: product.code,
                name: product.name,
                category: product.category,
                base_unit: product.base_unit,
                base_units_per_pack: Milli::from_thousandths(product.base_units_per_pack)
                    .to_display(),
                units_per_purchase_pack: product.units_per_purchase_pack,
                low_stock_threshold: product.low_stock_threshold.to_display(),
                tracks_inventory: product.tracks_inventory,
                destination: product.destination,
                active: product.active,
            })
        })
        .collect()
}

/// Every sale item, priced or not, with the recipe currently in force.
///
/// The recipe is read back through `catalogue::expand` rather than by reading
/// `recipe_lines` here, so what this screen shows an owner is produced by the
/// same expansion that will draw the stock down. That is one query per item
/// with a recipe: this is an administration screen opened occasionally, not
/// the till.
fn sale_item_lines(conn: &Connection, settings: &Settings) -> Result<Vec<SaleItemLine>> {
    // What each product is measured in, so a line written as "30ml" can be
    // shown back the same way instead of as 0.04 of a bottle.
    let measures: std::collections::BTreeMap<i64, (String, Milli)> =
        catalogue::every_product(conn)?
            .into_iter()
            .map(|product| {
                (
                    product.id,
                    (product.content_measure, product.content_per_unit),
                )
            })
            .collect();

    let mut statement = conn.prepare(
        "SELECT s.id, s.code, s.name, s.category, s.active, p.price_minor, r.id,
                s.from_product_id
           FROM sale_items s
           LEFT JOIN prices  p ON p.sale_item_id = s.id AND p.effective_to IS NULL
           LEFT JOIN recipes r ON r.sale_item_id = s.id AND r.effective_to IS NULL
          ORDER BY s.category, s.name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)? == 1,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
        ))
    })?;
    let found = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    found
        .into_iter()
        .map(
            |(id, code, name, category, active, price, recipe_id, from_product_id)| {
                let recipe: Vec<RecipeLineView> = match recipe_id {
                    Some(recipe_id) => catalogue::expand(conn, recipe_id, Milli::ONE)?
                        .into_iter()
                        .map(|consumed| {
                            let measured = measures.get(&consumed.product_id);
                            let measure = measured
                                .map(|(kind, _)| kind.clone())
                                .unwrap_or_else(|| "NONE".to_owned());
                            let measure_quantity = match measured {
                                Some((kind, per_unit)) if kind != "NONE" => {
                                    catalogue::units_to_measure(consumed.quantity, *per_unit)
                                        .to_display()
                                }
                                _ => String::new(),
                            };
                            RecipeLineView {
                                product_id: consumed.product_id,
                                name: consumed.name,
                                quantity_milli: consumed.quantity.thousandths(),
                                quantity: consumed.quantity.to_display(),
                                measure,
                                measure_quantity,
                            }
                        })
                        .collect(),
                    None => Vec::new(),
                };
                Ok(SaleItemLine {
                    id,
                    from_product_id,
                    code,
                    name,
                    category,
                    active,
                    price: price.map(|minor| settings.format_money(Money::from_minor(minor))),
                    price_value: price.map(|minor| Money::from_minor(minor).to_display()),
                    sellable: active && price.is_some() && !recipe.is_empty(),
                    recipe,
                })
            },
        )
        .collect()
}

fn view_of(conn: &Connection) -> Result<SetupView> {
    let settings = Settings::load(conn)?;
    let staff = staff_lines(conn)?;
    let products = product_lines(conn, &settings)?;
    let sale_items = sale_item_lines(conn, &settings)?;

    let has_waiter = staff
        .iter()
        .any(|person| person.role == "WAITER" && person.active);
    let sellable = sale_items.iter().filter(|item| item.sellable).count();
    let mut missing = Vec::new();
    if products.is_empty() {
        missing.push("Add what you buy — the bottles and cases on the shelf.".to_owned());
    }
    if sale_items.is_empty() {
        missing.push("Add what you sell — the drinks on the menu.".to_owned());
    } else if sellable == 0 {
        missing.push("Give at least one drink a recipe and a price.".to_owned());
    }
    if !has_waiter {
        missing.push("Add a waiter — a tab is opened in somebody's name.".to_owned());
    }

    Ok(SetupView {
        can_trade: has_waiter && sellable > 0,
        missing,
        staff,
        products,
        sale_items,
    })
}

/// Readable by anyone signed in: a cashier needs to see the shelf too.
pub fn setup_view(state: &AppState) -> Result<SetupView> {
    require_session(state)?;
    state.with_db(view_of)
}

// ---------------------------------------------------------------------------
// Writing. Every one of these is owner-only, and says so twice: here in words,
// and again inside `commissioning` against the actor it is given.
// ---------------------------------------------------------------------------

/// Run an owner-authorised change and hand back the refreshed screen.
fn commission(
    state: &AppState,
    work: impl FnOnce(&mut Connection, i64, i64) -> commissioning::Result<commissioning::Change>,
) -> Result<SetupView> {
    let session = require_owner(state)?;
    let now = now_ms();
    state.with_db_mut(|conn| -> Result<SetupView> {
        work(conn, session.staff_id, now)?;
        view_of(conn)
    })
}

pub fn add_staff(state: &AppState, form: &StaffForm) -> Result<SetupView> {
    let role =
        staff::Role::parse(&form.role).map_err(|error| CommandError::refused(error.to_string()))?;
    if role.authenticates() && form.pin.as_deref().unwrap_or("").is_empty() {
        return Err(CommandError::refused(
            "An owner or cashier needs a PIN, because they sign in.",
        ));
    }
    let input = commissioning::NewStaff {
        name: form.name.trim().to_owned(),
        role,
        // A waiter never signs in, so a PIN offered for one is dropped rather
        // than stored as a credential nothing will ever check.
        pin: role.authenticates().then(|| form.pin.clone()).flatten(),
    };
    commission(state, |conn, actor, at| {
        commissioning::create_staff(conn, actor, &input, at)
    })
}

pub fn set_staff_active(state: &AppState, staff_id: i64, active: bool) -> Result<SetupView> {
    commission(state, move |conn, actor, at| {
        commissioning::set_staff_active(conn, actor, staff_id, active, at)
    })
}

fn product_of(form: &ProductForm) -> Result<commissioning::NewProduct> {
    let sale_price = match form.sale_price.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(typed) => Some(money(typed, "Sale price")?),
    };
    Ok(commissioning::NewProduct {
        sale_price,
        content_measure: form.content_measure,
        content_per_unit: Milli::from_thousandths(form.content_per_unit_milli.max(0)),
        name: form.name.trim().to_owned(),
        category: form.category.trim().to_owned(),
        base_unit: form.base_unit,
        base_units_per_pack: milli(form.base_units_per_pack, "Units per pack")?,
        units_per_purchase_pack: form.units_per_purchase_pack,
        low_stock_threshold: Milli::from_thousandths(form.low_stock_threshold.max(0)),
        tracks_inventory: form.tracks_inventory,
        destination: form.destination,
    })
}

pub fn add_product(state: &AppState, form: &ProductForm) -> Result<SetupView> {
    let input = product_of(form)?;
    commission(state, |conn, actor, at| {
        commissioning::create_product(conn, actor, &input, at)
    })
}

/// Start selling something that was added as stock only.
pub fn sell_product(state: &AppState, product_id: i64, price: &str) -> Result<SetupView> {
    let price = money(price, "Sale price")?;
    commission(state, move |conn, actor, at| {
        commissioning::sell_product(conn, actor, product_id, price, at)
    })
}

/// Start selling a measure of something on the shelf — a shot from a bottle.
///
/// The measure arrives in the product's own unit as thousandths: 30000 means
/// 30ml. Rust divides it into the fraction of a bottle the recipe stores.
pub fn sell_by_measure(
    state: &AppState,
    product_id: i64,
    poured_milli: i64,
    price: &str,
) -> Result<SetupView> {
    let poured = milli(poured_milli, "A measure")?;
    let price = money(price, "Shot price")?;
    commission(state, move |conn, actor, at| {
        commissioning::sell_by_measure(conn, actor, product_id, poured, price, at)
    })
}

pub fn edit_product(state: &AppState, product_id: i64, form: &ProductForm) -> Result<SetupView> {
    let new = product_of(form)?;
    let input = commissioning::ProductUpdate {
        content_measure: new.content_measure,
        content_per_unit: new.content_per_unit,
        name: new.name,
        category: new.category,
        base_unit: new.base_unit,
        base_units_per_pack: new.base_units_per_pack,
        units_per_purchase_pack: new.units_per_purchase_pack,
        low_stock_threshold: new.low_stock_threshold,
        tracks_inventory: new.tracks_inventory,
        destination: new.destination,
        active: form.active,
    };
    commission(state, move |conn, actor, at| {
        commissioning::update_product(conn, actor, product_id, &input, at)
    })
}

pub fn add_sale_item(state: &AppState, form: &SaleItemForm) -> Result<SetupView> {
    let input = commissioning::NewSaleItem {
        name: form.name.trim().to_owned(),
        category: form.category.trim().to_owned(),
    };
    commission(state, |conn, actor, at| {
        commissioning::create_sale_item(conn, actor, &input, at)
    })
}

pub fn edit_sale_item(
    state: &AppState,
    sale_item_id: i64,
    form: &SaleItemForm,
) -> Result<SetupView> {
    let input = commissioning::SaleItemUpdate {
        name: form.name.trim().to_owned(),
        category: form.category.trim().to_owned(),
        active: form.active,
    };
    commission(state, move |conn, actor, at| {
        commissioning::update_sale_item(conn, actor, sale_item_id, &input, at)
    })
}

/// Replace a drink's recipe wholesale — the previous version is closed, never
/// edited, so a night already traded keeps the recipe it was poured under.
pub fn set_recipe(
    state: &AppState,
    sale_item_id: i64,
    lines: &[RecipeLineForm],
) -> Result<SetupView> {
    if lines.is_empty() {
        return Err(CommandError::refused(
            "A recipe needs at least one ingredient.",
        ));
    }
    let lines = lines
        .iter()
        .map(|line| {
            Ok(commissioning::RecipeLine {
                product_id: line.product_id,
                quantity: milli(line.quantity_milli, "Every recipe quantity")?,
                in_measure: line.in_measure,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    commission(state, move |conn, actor, at| {
        commissioning::revise_recipe(conn, actor, sale_item_id, &lines, at)
    })
}

/// Take a shelf item off the catalogue, or bring it back. Never a delete.
pub fn set_product_active(state: &AppState, product_id: i64, active: bool) -> Result<SetupView> {
    commission(state, move |conn, actor, at| {
        commissioning::set_product_active(conn, actor, product_id, active, at)
    })
}

/// Take a drink off the menu, or put it back. Never a delete.
pub fn set_sale_item_active(
    state: &AppState,
    sale_item_id: i64,
    active: bool,
) -> Result<SetupView> {
    commission(state, move |conn, actor, at| {
        commissioning::set_sale_item_active(conn, actor, sale_item_id, active, at)
    })
}

pub fn set_price(state: &AppState, sale_item_id: i64, price: &str) -> Result<SetupView> {
    let price = money(price, "Price")?;
    commission(state, move |conn, actor, at| {
        commissioning::reprice(conn, actor, sale_item_id, price, at)
    })
}

// ---------------------------------------------------------------------------
// The Tauri surface. Thin on purpose — the logic above is what is tested.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn cmd_setup_view(state: tauri::State<'_, AppState>) -> Result<SetupView> {
    setup_view(&state)
}

#[tauri::command]
pub fn cmd_add_staff(state: tauri::State<'_, AppState>, form: StaffForm) -> Result<SetupView> {
    add_staff(&state, &form)
}

#[tauri::command]
pub fn cmd_set_staff_active(
    state: tauri::State<'_, AppState>,
    staff_id: i64,
    active: bool,
) -> Result<SetupView> {
    set_staff_active(&state, staff_id, active)
}

#[tauri::command]
pub fn cmd_add_product(state: tauri::State<'_, AppState>, form: ProductForm) -> Result<SetupView> {
    add_product(&state, &form)
}

#[tauri::command]
pub fn cmd_sell_product(
    state: tauri::State<'_, AppState>,
    product_id: i64,
    price: String,
) -> Result<SetupView> {
    sell_product(&state, product_id, &price)
}

#[tauri::command]
pub fn cmd_sell_by_measure(
    state: tauri::State<'_, AppState>,
    product_id: i64,
    poured_milli: i64,
    price: String,
) -> Result<SetupView> {
    sell_by_measure(&state, product_id, poured_milli, &price)
}

#[tauri::command]
pub fn cmd_edit_product(
    state: tauri::State<'_, AppState>,
    product_id: i64,
    form: ProductForm,
) -> Result<SetupView> {
    edit_product(&state, product_id, &form)
}

#[tauri::command]
pub fn cmd_add_sale_item(
    state: tauri::State<'_, AppState>,
    form: SaleItemForm,
) -> Result<SetupView> {
    add_sale_item(&state, &form)
}

#[tauri::command]
pub fn cmd_edit_sale_item(
    state: tauri::State<'_, AppState>,
    sale_item_id: i64,
    form: SaleItemForm,
) -> Result<SetupView> {
    edit_sale_item(&state, sale_item_id, &form)
}

#[tauri::command]
pub fn cmd_set_recipe(
    state: tauri::State<'_, AppState>,
    sale_item_id: i64,
    lines: Vec<RecipeLineForm>,
) -> Result<SetupView> {
    set_recipe(&state, sale_item_id, &lines)
}

#[tauri::command]
pub fn cmd_set_price(
    state: tauri::State<'_, AppState>,
    sale_item_id: i64,
    price: String,
) -> Result<SetupView> {
    set_price(&state, sale_item_id, &price)
}

#[tauri::command]
pub fn cmd_set_product_active(
    state: tauri::State<'_, AppState>,
    product_id: i64,
    active: bool,
) -> Result<SetupView> {
    set_product_active(&state, product_id, active)
}

#[tauri::command]
pub fn cmd_set_sale_item_active(
    state: tauri::State<'_, AppState>,
    sale_item_id: i64,
    active: bool,
) -> Result<SetupView> {
    set_sale_item_active(&state, sale_item_id, active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::fixture;
    use crate::state::Session;

    fn owned() -> AppState {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = fixture::staff(&conn, "OWN-1", "Selam", "OWNER");
        fixture::staff(&conn, "CSH-1", "Abel", "CASHIER");
        let state = AppState::new(conn);
        state.set_session(Some(Session {
            staff_id: owner,
            code: "OWN-1".into(),
            name: "Selam".into(),
            role: "OWNER".into(),
        }));
        state
    }

    fn product_form(name: &str) -> ProductForm {
        ProductForm {
            sale_price: None,
            content_measure: Measure::None,
            content_per_unit_milli: 0,
            name: name.into(),
            category: "Beer".into(),
            base_unit: BaseUnit::Bottle,
            base_units_per_pack: 1_000,
            units_per_purchase_pack: 24,
            low_stock_threshold: 3_000,
            tracks_inventory: true,
            destination: Destination::Bar,
            active: true,
        }
    }

    #[test]
    fn a_fresh_venue_says_what_is_missing_until_it_can_trade() {
        let state = owned();
        let view = setup_view(&state).unwrap();
        assert!(!view.can_trade);
        assert_eq!(view.missing.len(), 3, "{:?}", view.missing);

        let view = add_product(&state, &product_form("Beer")).unwrap();
        let beer = view.products[0].id;
        assert!(!view.can_trade);

        let view = add_sale_item(
            &state,
            &SaleItemForm {
                name: "Beer".into(),
                category: "Bottles".into(),
                active: true,
            },
        )
        .unwrap();
        let bottle = view.sale_items[0].id;
        // Named, but not yet sellable: no recipe and no price.
        assert!(!view.sale_items[0].sellable);
        assert!(view.sale_items[0].price.is_none());

        set_recipe(
            &state,
            bottle,
            &[RecipeLineForm {
                product_id: beer,
                quantity_milli: 1_000,
                in_measure: false,
            }],
        )
        .unwrap();
        let view = set_price(&state, bottle, "50").unwrap();
        assert!(view.sale_items[0].sellable);
        assert_eq!(view.sale_items[0].price.as_deref(), Some("50.00"));
        assert_eq!(view.sale_items[0].recipe[0].name, "Beer");
        // Still no waiter, so no tab could be opened in anybody's name.
        assert!(!view.can_trade);
        assert_eq!(view.missing.len(), 1);

        let view = add_staff(
            &state,
            &StaffForm {
                name: "Sara".into(),
                role: "WAITER".into(),
                pin: None,
            },
        )
        .unwrap();
        assert!(view.can_trade, "{:?}", view.missing);
        assert!(view.missing.is_empty());
    }

    #[test]
    fn something_removed_stays_visible_so_it_can_come_back() {
        // A removed product that vanished from this screen could never be
        // restored, and nothing would show it had been taken off.
        let state = owned();
        add_product(&state, &product_form("Gin")).unwrap();
        let gin = setup_view(&state).unwrap().products[0].id;

        let view = set_product_active(&state, gin, false).unwrap();
        let line = view.products.iter().find(|p| p.id == gin).unwrap();
        assert!(!line.active, "removed but still listed");

        let view = set_product_active(&state, gin, true).unwrap();
        assert!(view.products.iter().find(|p| p.id == gin).unwrap().active);
    }

    #[test]
    fn stock_on_the_shelf_blocks_removal_but_never_editing() {
        // The user's rule: refuse to remove something that is not empty, but
        // always allow it to be edited.
        let state = owned();
        add_product(&state, &product_form("Gin")).unwrap();
        let gin = setup_view(&state).unwrap().products[0].id;
        state
            .with_db(|conn| {
                crate::repo::stock::post(
                    conn,
                    &crate::repo::stock::Movement::new(
                        gin,
                        crate::repo::stock::Kind::StockCorrection,
                        Milli::from_units(4),
                        now_ms(),
                        1,
                    )
                    .because("test shelf"),
                )
                .map(|_| ())
            })
            .unwrap();

        let refused = set_product_active(&state, gin, false).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(refused.message.contains("shelf"), "got: {refused:?}");

        // Editing is untouched by any of that.
        let mut form = product_form("Gin");
        form.category = "Spirits".into();
        assert!(edit_product(&state, gin, &form).is_ok());
    }

    #[test]
    fn a_shot_is_sellable_the_moment_it_is_added() {
        // Built as three separate commands from the screen, a shot could end
        // up with a menu entry and no recipe, or a recipe and no price — which
        // is how this venue ended up with two items that looked finished in
        // the catalogue and were refused at the till.
        let state = owned();
        let mut form = product_form("Jack Daniels");
        form.content_measure = Measure::Ml;
        form.content_per_unit_milli = 750_000;
        form.sale_price = Some("3000".into());
        add_product(&state, &form).unwrap();
        let bottle = setup_view(&state).unwrap().products[0].id;

        // 30ml of a 750ml bottle.
        let view = sell_by_measure(&state, bottle, 30_000, "100").unwrap();

        let shot = view
            .sale_items
            .iter()
            .find(|item| item.name.contains("30ml"))
            .expect("the shot is on the menu under its own name");
        assert!(shot.sellable, "priced and poured, so the till can ring it");
        assert_eq!(shot.price.as_deref(), Some("100.00"));
        assert_eq!(shot.recipe.len(), 1, "one bottle, never a cocktail");
        assert_eq!(shot.recipe[0].measure_quantity, "30");
    }

    #[test]
    fn a_bottle_that_does_not_say_how_much_it_holds_cannot_be_poured() {
        let state = owned();
        add_product(&state, &product_form("Harar Beer")).unwrap();
        let beer = setup_view(&state).unwrap().products[0].id;

        let refused = sell_by_measure(&state, beer, 30_000, "100").unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(
            refused.message.contains("how much one holds"),
            "{refused:?}"
        );
    }

    #[test]
    fn renaming_a_shelf_item_renames_what_the_till_offers() {
        // The pair share a name, copied across when they were made. A rename
        // that stopped at the shelf would leave the till still selling "Gin"
        // long after the bottle became "Gordon's gin".
        let state = owned();
        let mut form = product_form("Gin");
        form.sale_price = Some("1200".into());
        add_product(&state, &form).unwrap();
        let gin = setup_view(&state).unwrap().products[0].id;

        form.name = "Gordon's gin".into();
        let view = edit_product(&state, gin, &form).unwrap();

        let shelf = view.products.iter().find(|p| p.id == gin).unwrap();
        assert_eq!(shelf.name, "Gordon's gin");
        let twin = view
            .sale_items
            .iter()
            .find(|item| item.from_product_id == Some(gin))
            .unwrap();
        assert_eq!(twin.name, "Gordon's gin", "the till kept the old name");
        // What an edit box is filled with has to be what `Money::parse` reads
        // back: the formatted "1,200.00" would be refused on the way in.
        assert_eq!(shelf.price_value.as_deref(), Some("1200.00"));
    }

    #[test]
    fn an_ingredient_of_a_live_drink_cannot_be_removed() {
        let state = owned();
        add_product(&state, &product_form("Gin")).unwrap();
        let gin = setup_view(&state).unwrap().products[0].id;
        let drink = add_sale_item(
            &state,
            &SaleItemForm {
                name: "Gin and tonic".into(),
                category: "Cocktails".into(),
                active: true,
            },
        )
        .unwrap()
        .sale_items
        .iter()
        .find(|item| item.name == "Gin and tonic")
        .unwrap()
        .id;
        set_recipe(
            &state,
            drink,
            &[RecipeLineForm {
                product_id: gin,
                quantity_milli: 1_000,
                in_measure: false,
            }],
        )
        .unwrap();

        let refused = set_product_active(&state, gin, false).unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(
            refused.message.contains("Gin and tonic"),
            "the refusal should name the drink; got: {refused:?}"
        );
    }

    #[test]
    fn a_drink_nobody_is_drinking_comes_off_the_menu() {
        let state = owned();
        let drink = add_sale_item(
            &state,
            &SaleItemForm {
                name: "Negroni".into(),
                category: "Cocktails".into(),
                active: true,
            },
        )
        .unwrap()
        .sale_items[0]
            .id;

        let view = set_sale_item_active(&state, drink, false).unwrap();
        assert!(
            !view
                .sale_items
                .iter()
                .find(|i| i.id == drink)
                .unwrap()
                .active
        );
    }

    #[test]
    fn a_cashier_may_look_at_the_catalogue_but_not_change_it() {
        let state = owned();
        state.set_session(Some(Session {
            staff_id: 2,
            code: "CSH-1".into(),
            name: "Abel".into(),
            role: "CASHIER".into(),
        }));
        setup_view(&state).unwrap();
        assert_eq!(
            add_product(&state, &product_form("Gin")).unwrap_err().kind,
            "NOT_PERMITTED"
        );
    }

    #[test]
    fn a_signing_in_role_cannot_be_created_without_a_pin() {
        let state = owned();
        let refused = add_staff(
            &state,
            &StaffForm {
                name: "Hanna".into(),
                role: "CASHIER".into(),
                pin: None,
            },
        )
        .unwrap_err();
        assert_eq!(refused.kind, "REFUSED");
        assert!(refused.message.contains("PIN"), "{}", refused.message);
    }
}
