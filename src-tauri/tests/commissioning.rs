use servepoint_lib::commissioning::{
    self, BaseUnit, Destination, NewProduct, NewSaleItem, NewStaff, ProductUpdate, RecipeLine,
    SaleItemUpdate,
};
use servepoint_lib::repo::{catalogue, staff};
use servepoint_lib::{Milli, Money};

const NOW: i64 = 1_786_500_000_000;

struct Venue {
    conn: rusqlite::Connection,
    owner: i64,
    cashier: i64,
}

fn venue() -> Venue {
    let conn = servepoint_lib::db::open_in_memory().unwrap();
    conn.execute(
        "INSERT INTO staff (code, full_name, role, pin_hash, pin_salt, created_at)
         VALUES ('OWN-1', 'Selam', 'OWNER', 'hash', 'salt', ?1)",
        [NOW],
    )
    .unwrap();
    let owner = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO staff (code, full_name, role, pin_hash, pin_salt, created_at)
         VALUES ('CSH-1', 'Abel', 'CASHIER', 'hash', 'salt', ?1)",
        [NOW],
    )
    .unwrap();
    let cashier = conn.last_insert_rowid();
    Venue {
        conn,
        owner,
        cashier,
    }
}

fn beer() -> NewProduct {
    NewProduct {
        sale_price: None,
        content_measure: commissioning::Measure::None,
        content_per_unit: Milli::ZERO,
        name: "Beer".into(),
        category: "Bottles".into(),
        base_unit: BaseUnit::Bottle,
        base_units_per_pack: Milli::ONE,
        units_per_purchase_pack: 24,
        low_stock_threshold: Milli::from_units(6),
        tracks_inventory: true,
        destination: Destination::Bar,
    }
}

#[test]
fn only_an_active_owner_can_create_master_data_and_every_change_is_audited() {
    let mut venue = venue();

    let denied =
        commissioning::create_product(&mut venue.conn, venue.cashier, &beer(), NOW).unwrap_err();
    assert!(denied.to_string().contains("active owner"), "got: {denied}");
    assert_eq!(
        venue
            .conn
            .query_row("SELECT COUNT(*) FROM products", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    let product =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap();
    assert_eq!(product.entity_id, 1);
    assert_eq!(product.audit_sequence, 1);
    let action: String = venue
        .conn
        .query_row("SELECT action FROM audit_log", [], |row| row.get(0))
        .unwrap();
    assert_eq!(action, "PRODUCT_CREATED");
}

/// The whole point of the one-step path: a club adds a bottle and it is on the
/// till, with no separate menu entry, recipe or price to remember. All four
/// rows commit together, so there is no state where the screen shows a
/// finished item that the till refuses.
#[test]
fn a_priced_product_is_sellable_the_moment_it_is_created() {
    let mut venue = venue();
    let priced = NewProduct {
        sale_price: Some(Money::from_minor(12_000)),
        ..beer()
    };

    let product =
        commissioning::create_product(&mut venue.conn, venue.owner, &priced, NOW).unwrap();

    let menu = catalogue::menu(&venue.conn).unwrap();
    assert_eq!(menu.len(), 1);
    assert_eq!(menu[0].name, "Beer");
    assert_eq!(menu[0].price, Money::from_minor(12_000));

    // One bottle off the shelf per sale, written as an ordinary recipe so that
    // availability, issue slips and corrections all keep working unchanged.
    let drawn = catalogue::expand(&venue.conn, menu[0].recipe_id, Milli::ONE).unwrap();
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].product_id, product.entity_id);
    assert_eq!(drawn[0].quantity, Milli::ONE);

    // History cannot tell whether one screen or three was used.
    let actions: Vec<String> = venue
        .conn
        .prepare("SELECT action FROM audit_log ORDER BY sequence_no")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        actions,
        [
            "PRODUCT_CREATED",
            "SALE_ITEM_CREATED",
            "RECIPE_CHANGED",
            "PRICE_CHANGED"
        ]
    );
}

/// Stock only: a mixer poured into other drinks and never ordered by name.
#[test]
fn a_product_with_no_price_stays_off_the_menu_until_it_is_asked_for() {
    let mut venue = venue();
    let product =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap();
    assert!(catalogue::menu(&venue.conn).unwrap().is_empty());

    commissioning::sell_product(
        &mut venue.conn,
        venue.owner,
        product.entity_id,
        Money::from_minor(12_000),
        NOW + 1,
    )
    .unwrap();

    let menu = catalogue::menu(&venue.conn).unwrap();
    assert_eq!(menu.len(), 1);
    assert_eq!(menu[0].price, Money::from_minor(12_000));
}

/// A club pours by measure. The owner writes "30ml of whiskey"; the ledger
/// holds the fraction of a bottle that is, so nothing downstream has to learn
/// a second unit.
#[test]
fn a_recipe_written_in_millilitres_is_stored_as_a_fraction_of_a_bottle() {
    let mut venue = venue();
    let whiskey = commissioning::create_product(
        &mut venue.conn,
        venue.owner,
        &NewProduct {
            name: "Whiskey".into(),
            content_measure: commissioning::Measure::Ml,
            content_per_unit: Milli::from_units(750),
            ..beer()
        },
        NOW,
    )
    .unwrap();
    let beer_product =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap();
    let item = commissioning::create_sale_item(
        &mut venue.conn,
        venue.owner,
        &NewSaleItem {
            name: "Boilermaker".into(),
            category: "Cocktails".into(),
        },
        NOW,
    )
    .unwrap();

    commissioning::revise_recipe(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        &[
            // One shot of a 750ml bottle.
            RecipeLine {
                product_id: whiskey.entity_id,
                quantity: Milli::from_units(30),
                in_measure: true,
            },
            // A whole bottle of beer, in counted units as before.
            RecipeLine {
                product_id: beer_product.entity_id,
                quantity: Milli::ONE,
                in_measure: false,
            },
        ],
        NOW + 1,
    )
    .unwrap();

    let stored: Vec<i64> = venue
        .conn
        .prepare(
            "SELECT l.quantity_milli FROM recipe_lines l
               JOIN recipes r ON r.id = l.recipe_id
              WHERE r.effective_to IS NULL ORDER BY l.product_id",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    // 30/750 is 0.040 of a bottle; the beer is one whole bottle.
    assert_eq!(stored, [40, 1_000]);
}

/// A pour so small it would round to nothing must be refused rather than
/// silently drawing zero stock every time the drink is sold.
#[test]
fn a_measure_too_small_to_draw_anything_is_refused() {
    let mut venue = venue();
    let product = commissioning::create_product(
        &mut venue.conn,
        venue.owner,
        &NewProduct {
            content_measure: commissioning::Measure::Ml,
            content_per_unit: Milli::from_units(750),
            ..beer()
        },
        NOW,
    )
    .unwrap();
    let item = commissioning::create_sale_item(
        &mut venue.conn,
        venue.owner,
        &NewSaleItem {
            name: "Bitters".into(),
            category: "Cocktails".into(),
        },
        NOW,
    )
    .unwrap();

    let refused = commissioning::revise_recipe(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        &[RecipeLine {
            product_id: product.entity_id,
            // Under a thousandth of a 750ml bottle.
            quantity: Milli::from_thousandths(300),
            in_measure: true,
        }],
        NOW + 1,
    )
    .unwrap_err();
    assert!(refused.to_string().contains("too small"), "got: {refused}");
}

#[test]
fn staff_pins_are_hashed_but_never_written_to_the_audit_log() {
    let mut venue = venue();
    let secret = "4071";

    let created = commissioning::create_staff(
        &mut venue.conn,
        venue.owner,
        &NewStaff {
            name: "Meron".into(),
            role: staff::Role::Cashier,
            pin: Some(secret.into()),
        },
        NOW,
    )
    .unwrap();

    let (salt, hash): (String, String) = venue
        .conn
        .query_row(
            "SELECT pin_salt, pin_hash FROM staff WHERE id = ?1",
            [created.entity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_ne!(hash, secret);
    assert!(servepoint_lib::auth::verify_pin(secret, &salt, &hash));
    let audit: String = venue
        .conn
        .query_row(
            "SELECT COALESCE(new_value, '') FROM audit_log WHERE action = 'STAFF_CREATED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!audit.contains(secret));
    assert!(!audit.contains(&salt));
    assert!(!audit.contains(&hash));
}

#[test]
fn a_sale_item_becomes_sellable_only_after_recipe_and_price_versions_exist() {
    let mut venue = venue();
    let product =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap();
    let item = commissioning::create_sale_item(
        &mut venue.conn,
        venue.owner,
        &NewSaleItem {
            name: "Beer".into(),
            category: "Bottles".into(),
        },
        NOW + 1,
    )
    .unwrap();
    assert!(catalogue::menu(&venue.conn).unwrap().is_empty());

    commissioning::revise_recipe(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        &[RecipeLine {
            product_id: product.entity_id,
            quantity: Milli::ONE,
            in_measure: false,
        }],
        NOW + 2,
    )
    .unwrap();
    assert!(catalogue::menu(&venue.conn).unwrap().is_empty());

    commissioning::reprice(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        Money::from_minor(5_000),
        NOW + 3,
    )
    .unwrap();

    let menu = catalogue::menu(&venue.conn).unwrap();
    assert_eq!(menu.len(), 1);
    assert_eq!(menu[0].price, Money::from_minor(5_000));
    let actions: Vec<String> = venue
        .conn
        .prepare("SELECT action FROM audit_log ORDER BY sequence_no")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        actions,
        [
            "PRODUCT_CREATED",
            "SALE_ITEM_CREATED",
            "RECIPE_CHANGED",
            "PRICE_CHANGED"
        ]
    );
}

#[test]
fn an_audit_failure_rolls_the_master_change_back() {
    let mut venue = venue();
    venue
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER fail_master_audit BEFORE INSERT ON audit_log
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
        )
        .unwrap();

    let failure =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap_err();
    assert!(
        failure.to_string().contains("injected audit failure"),
        "got: {failure}"
    );
    assert_eq!(
        venue
            .conn
            .query_row("SELECT COUNT(*) FROM products", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        venue
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn a_failed_recipe_audit_leaves_the_previous_version_open() {
    let mut venue = venue();
    let product =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap();
    let item = commissioning::create_sale_item(
        &mut venue.conn,
        venue.owner,
        &NewSaleItem {
            name: "Beer".into(),
            category: "Bottles".into(),
        },
        NOW,
    )
    .unwrap();
    let first = commissioning::revise_recipe(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        &[RecipeLine {
            product_id: product.entity_id,
            quantity: Milli::ONE,
            in_measure: false,
        }],
        NOW,
    )
    .unwrap();
    venue
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER fail_recipe_audit BEFORE INSERT ON audit_log
             WHEN NEW.action = 'RECIPE_CHANGED'
             BEGIN SELECT RAISE(ABORT, 'injected recipe audit failure'); END;",
        )
        .unwrap();

    commissioning::revise_recipe(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        &[RecipeLine {
            product_id: product.entity_id,
            quantity: Milli::from_units(2),
            in_measure: false,
        }],
        NOW + 1,
    )
    .unwrap_err();

    let (open_id, versions): (i64, i64) = venue
        .conn
        .query_row(
            "SELECT MAX(CASE WHEN effective_to IS NULL THEN id END), COUNT(*)
               FROM recipes WHERE sale_item_id = ?1",
            [item.entity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(open_id, first.entity_id);
    assert_eq!(versions, 1);
}

#[test]
fn owner_updates_to_staff_products_and_sale_items_store_before_and_after_facts() {
    let mut venue = venue();
    let waiter = commissioning::create_staff(
        &mut venue.conn,
        venue.owner,
        &NewStaff {
            name: "Sara".into(),
            role: staff::Role::Waiter,
            pin: None,
        },
        NOW,
    )
    .unwrap();
    commissioning::set_staff_active(
        &mut venue.conn,
        venue.owner,
        waiter.entity_id,
        false,
        NOW + 1,
    )
    .unwrap();
    assert!(!staff::find(&venue.conn, waiter.entity_id).unwrap().active);

    let product =
        commissioning::create_product(&mut venue.conn, venue.owner, &beer(), NOW).unwrap();
    commissioning::update_product(
        &mut venue.conn,
        venue.owner,
        product.entity_id,
        &ProductUpdate {
            content_measure: commissioning::Measure::None,
            content_per_unit: Milli::ZERO,
            name: "Amber Beer".into(),
            category: "Beer".into(),
            base_unit: BaseUnit::Bottle,
            base_units_per_pack: Milli::ONE,
            units_per_purchase_pack: 24,
            low_stock_threshold: Milli::from_units(12),
            tracks_inventory: true,
            destination: Destination::Bar,
            active: true,
        },
        NOW + 2,
    )
    .unwrap();
    assert_eq!(
        catalogue::product(&venue.conn, product.entity_id)
            .unwrap()
            .name,
        "Amber Beer"
    );

    let item = commissioning::create_sale_item(
        &mut venue.conn,
        venue.owner,
        &NewSaleItem {
            name: "Beer".into(),
            category: "Bottles".into(),
        },
        NOW + 3,
    )
    .unwrap();
    commissioning::update_sale_item(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        &SaleItemUpdate {
            name: "Amber Beer".into(),
            category: "Beer".into(),
            active: false,
        },
        NOW + 4,
    )
    .unwrap();
    assert!(
        !catalogue::sale_item(&venue.conn, item.entity_id)
            .unwrap()
            .active
    );

    let changed: Vec<(String, String, String)> = venue
        .conn
        .prepare(
            "SELECT action, COALESCE(old_value, ''), COALESCE(new_value, '')
               FROM audit_log
              WHERE action IN ('STAFF_DEACTIVATED','PRODUCT_CHANGED','SALE_ITEM_CHANGED')
              ORDER BY sequence_no",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(changed.len(), 3);
    assert_eq!(changed[0].1, "active=1");
    assert_eq!(changed[0].2, "active=0");
    assert!(changed[1].1.contains("name=Beer"));
    assert!(changed[1].2.contains("name=Amber Beer"));
    assert!(changed[2].1.contains("active=1"));
    assert!(changed[2].2.contains("active=0"));
}

#[test]
fn a_rejected_price_does_not_close_the_current_price_or_add_an_audit_entry() {
    let mut venue = venue();
    let item = commissioning::create_sale_item(
        &mut venue.conn,
        venue.owner,
        &NewSaleItem {
            name: "Beer".into(),
            category: "Bottles".into(),
        },
        NOW,
    )
    .unwrap();
    commissioning::reprice(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        Money::from_minor(5_000),
        NOW + 1,
    )
    .unwrap();
    let before_audit: i64 = venue
        .conn
        .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
        .unwrap();

    assert!(commissioning::reprice(
        &mut venue.conn,
        venue.owner,
        item.entity_id,
        Money::from_minor(-1),
        NOW + 2,
    )
    .is_err());

    let current: (i64, i64) = venue
        .conn
        .query_row(
            "SELECT price_minor, COUNT(*) OVER () FROM prices
              WHERE sale_item_id = ?1 AND effective_to IS NULL",
            [item.entity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(current, (5_000, 1));
    assert_eq!(
        venue
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before_audit
    );
}
