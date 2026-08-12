// schema-check.mjs — apply the migrations and assert the database enforces
// what 10-business-logic-for-port.md §11 says it must.
//
// Stands in for the Rust migration runner until that exists. Loads
// src-tauri/migrations/*.sql in filename order — the same order the Rust side
// will use via an explicit include_str! array (D6: never directory scanning,
// which behaves differently in a packaged build).
//
// The point of these tests is §11's opening line: these rules are NOT left to
// the service layer, because a future bug or a support session in a SQLite
// browser would otherwise break them. So they are asserted against the
// database itself, with no application code in the way.
//
//   node tools/schema-check.mjs

import { DatabaseSync } from 'node:sqlite';
import { readdirSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const MIGRATIONS_DIR = join(ROOT, 'src-tauri', 'migrations');

let passed = 0;
const failures = [];

function check(name, fn) {
  try {
    fn();
    passed++;
  } catch (err) {
    failures.push({ name, message: err.message });
  }
}

/** Assert that `fn` is rejected by the database, and for the right reason. */
function rejects(fn, expected) {
  let threw = null;
  try {
    fn();
  } catch (err) {
    threw = err;
  }
  if (!threw) throw new Error('expected the database to reject this, it did not');
  if (expected && !threw.message.includes(expected)) {
    throw new Error(`rejected, but with the wrong reason: ${threw.message}`);
  }
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

// ---------------------------------------------------------------------------
// Apply migrations
// ---------------------------------------------------------------------------

function migrate(db) {
  // Both pragmas are set explicitly. foreign_keys defaults to OFF in SQLite and
  // silently does nothing if forgotten (§13).
  db.exec('PRAGMA foreign_keys = ON;');

  const files = readdirSync(MIGRATIONS_DIR).filter((f) => f.endsWith('.sql')).sort();
  if (files.length === 0) throw new Error(`no migrations found in ${MIGRATIONS_DIR}`);

  for (const file of files) {
    const version = Number.parseInt(file.slice(0, 4), 10);
    if (Number.isNaN(version)) throw new Error(`migration ${file} has no NNNN prefix`);
    db.exec('BEGIN');
    try {
      db.exec(readFileSync(join(MIGRATIONS_DIR, file), 'utf8'));
      db.prepare(
        'INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)'
      ).run(version, file, Date.now());
      db.exec('COMMIT');
    } catch (err) {
      db.exec('ROLLBACK');
      throw new Error(`migration ${file} failed: ${err.message}`);
    }
  }
  return files;
}

const db = new DatabaseSync(':memory:');
const applied = migrate(db);

const run = (sql, ...args) => db.prepare(sql).run(...args);
const get = (sql, ...args) => db.prepare(sql).get(...args);

const OWNER = ['O01', 'Test Owner', 'OWNER', 'hash', 'salt', 0];
const CASHIER = ['C01', 'Test Cashier', 'CASHIER', 'hash', 'salt', 0];
const WAITER = ['W01', 'Test Waiter', 'WAITER', null, null, 0];
const insertStaff = (row) =>
  run(
    `INSERT INTO staff (code, full_name, role, pin_hash, pin_salt, created_at)
     VALUES (?, ?, ?, ?, ?, ?)`,
    ...row
  );

// ---------------------------------------------------------------------------
// §1.4 — sequences
// ---------------------------------------------------------------------------

check('sequences: allocation increments and returns in one statement', () => {
  const first = get(
    `UPDATE sequences SET next_value = next_value + 1
      WHERE name = 'ISSUE_RECEIPT' RETURNING next_value - 1 AS seq`
  );
  const second = get(
    `UPDATE sequences SET next_value = next_value + 1
      WHERE name = 'ISSUE_RECEIPT' RETURNING next_value - 1 AS seq`
  );
  assert(first.seq === 1, `first allocation should be 1, got ${first.seq}`);
  assert(second.seq === 2, `second allocation should be 2, got ${second.seq}`);
});

check('sequences: only the four named sequences exist', () => {
  const names = db
    .prepare('SELECT name FROM sequences ORDER BY name')
    .all()
    .map((r) => r.name);
  assert(
    JSON.stringify(names) ===
      JSON.stringify(['CUSTOMER_RECEIPT', 'ISSUE_RECEIPT', 'SHIFT', 'TAB']),
    `unexpected sequences: ${names.join(', ')}`
  );
  rejects(() => run("INSERT INTO sequences VALUES ('BR', 1)"), 'CHECK');
});

// ---------------------------------------------------------------------------
// §12 / D8 / D9 / D15 — settings
// ---------------------------------------------------------------------------

check('settings: receipt identity ships blank, not Tonic-specific (D8)', () => {
  for (const key of ['receipt.business_name', 'receipt.address', 'receipt.tin',
                     'locale.currency_code']) {
    const row = get('SELECT value FROM settings WHERE key = ?', key);
    assert(row !== undefined, `${key} is missing`);
    assert(row.value === '', `${key} should ship blank, got "${row.value}"`);
  }
});

check('settings: there is NO way to allow negative stock (D9, revised)', () => {
  // Insufficient stock always blocks the sale. Neither the original boolean nor
  // the four-value policy that briefly replaced it may exist — a key here would
  // be a switch that turns the safety off.
  for (const key of ['inventory.allow_negative', 'inventory.stock_policy']) {
    assert(
      get('SELECT 1 AS x FROM settings WHERE key = ?', key) === undefined,
      `${key} must not exist — blocking is unconditional`
    );
  }
  const stray = db
    .prepare("SELECT key FROM settings WHERE key LIKE 'inventory.%'")
    .all()
    .map((r) => r.key);
  assert(stray.length === 0, `unexpected inventory settings: ${stray.join(', ')}`);
});

check('settings: report printing is off, bar slips are not a setting (D23)', () => {
  assert(
    get("SELECT value FROM settings WHERE key = 'printing.report_enabled'").value === '0',
    'the nightly report must not print by default — it is read on screen'
  );
  assert(
    get("SELECT value FROM settings WHERE key = 'printing.customer_receipt_enabled'").value === '1',
    'customer receipts print on request by default'
  );
  assert(
    get("SELECT 1 AS x FROM settings WHERE key = 'printing.issue_slip_enabled'") === undefined,
    'bar issue slips are how drinks get released and must never be switchable'
  );
});

check('settings: receipt payment footer and customer TIN exist and are off (D24/D25)', () => {
  assert(
    get("SELECT value FROM settings WHERE key = 'payments.bank_accounts'").value === '',
    'bank accounts ship empty so the section is omitted from the receipt'
  );
  assert(
    get("SELECT value FROM settings WHERE key = 'payments.qr_enabled'").value === '0',
    'the EthSwitch QR is opt-in'
  );
  assert(
    get("SELECT value FROM settings WHERE key = 'tabs.ask_customer_tin'").value === '0',
    'prompting for a customer TIN is opt-in'
  );
});

check('settings: the inert keys still exist as rows (D15 / §14)', () => {
  for (const key of ['tabs.age_warning_days', 'payments.partial_enabled',
                     'reporting.show_cost', 'locale.rounding']) {
    assert(
      get('SELECT 1 AS x FROM settings WHERE key = ?', key) !== undefined,
      `${key} should remain in the database even though nothing reads it`
    );
  }
});

check('settings: an unknown tab reference mode is refused', () => {
  rejects(
    () => run("UPDATE settings SET value = 'SEAT' WHERE key = 'tabs.reference_mode'"),
    'invalid'
  );
  for (const v of ['TABLE', 'CUSTOMER_NAME', 'CUSTOMER_PHONE', 'CUSTOM']) {
    run('UPDATE settings SET value = ? WHERE key = ?', v, 'tabs.reference_mode');
  }
  run("UPDATE settings SET value = 'TABLE' WHERE key = 'tabs.reference_mode'");
});

check('settings: a malformed BOOLEAN, RATE or TIME is refused', () => {
  rejects(() => run("UPDATE settings SET value = 'yes' WHERE key = 'tax.enabled'"), 'invalid');
  rejects(() => run("UPDATE settings SET value = '15%' WHERE key = 'tax.rate_bp'"), 'invalid');
  rejects(() => run("UPDATE settings SET value = '6pm' WHERE key = 'shift.day_start'"), 'invalid');
  run("UPDATE settings SET value = '1'     WHERE key = 'tax.enabled'");
  run("UPDATE settings SET value = '1500'  WHERE key = 'tax.rate_bp'");
  run("UPDATE settings SET value = '18:00' WHERE key = 'shift.day_start'");
});

check('settings: rows are never deleted', () => {
  rejects(() => run("DELETE FROM settings WHERE key = 'tax.enabled'"), 'never deleted');
});

// ---------------------------------------------------------------------------
// §0.3 — staff
// ---------------------------------------------------------------------------

check('staff: owners and cashiers carry a PIN, waiters must not (D22)', () => {
  insertStaff(OWNER);
  insertStaff(CASHIER);
  insertStaff(WAITER);
  rejects(() => insertStaff(['C02', 'No PIN', 'CASHIER', null, null, 0]), 'carry a PIN');
  rejects(() => insertStaff(['O02', 'No PIN', 'OWNER', null, null, 0]), 'carry a PIN');
  rejects(() => insertStaff(['W02', 'Has PIN', 'WAITER', 'hash', 'salt', 0]), 'carry a PIN');
});

check('staff: BARTENDER is accepted (§14 — later a data change, not a schema change)', () => {
  insertStaff(['B01', 'Test Bartender', 'BARTENDER', null, null, 0]);
});

check('staff: an unknown role is refused', () => {
  rejects(() => insertStaff(['M01', 'Manager', 'MANAGER', null, null, 0]), 'CHECK');
});

check('staff: are deactivated, never deleted', () => {
  rejects(() => run("DELETE FROM staff WHERE code = 'W01'"), 'never deleted');
  run("UPDATE staff SET active = 0 WHERE code = 'W01'");
  assert(get("SELECT active FROM staff WHERE code = 'W01'").active === 0, 'deactivation failed');
});

check('staff: the last owner cannot be deactivated (D22 lockout guard)', () => {
  // Settings live behind the owner role on an offline machine with no password
  // recovery. Losing the last owner would lock the venue out of its own tax rate.
  rejects(() => run("UPDATE staff SET active = 0 WHERE code = 'O01'"), 'last active owner');

  // With a second owner present, the first may be retired.
  insertStaff(['O03', 'Second Owner', 'OWNER', 'hash', 'salt', 0]);
  run("UPDATE staff SET active = 0 WHERE code = 'O01'");
  assert(
    get("SELECT active FROM staff WHERE code = 'O01'").active === 0,
    'deactivation should succeed once another owner exists'
  );
  // ...and now the remaining one is protected in turn.
  rejects(() => run("UPDATE staff SET active = 0 WHERE code = 'O03'"), 'last active owner');
});

check('staff: a blank name is refused', () => {
  rejects(() => insertStaff(['X01', '   ', 'WAITER', null, null, 0]), 'CHECK');
});

// ---------------------------------------------------------------------------
// §2 — catalogue: products, sale items, recipes, prices
//
// Fixtures build the worked example from §2.1: one product Gin (base unit
// SHOT, 24 shots per bottle) carrying three sale items, plus beer and tonic.
// ---------------------------------------------------------------------------

const product = (code, name, unit, perPack, opts = {}) =>
  run(
    `INSERT INTO products (code, name, base_unit, base_units_per_pack,
                           tracks_inventory, destination, created_at)
     VALUES (?, ?, ?, ?, ?, ?, 0)`,
    code, name, unit, perPack,
    opts.tracks ?? 1, opts.destination ?? 'BAR'
  );

const saleItem = (code, name, category) =>
  run(
    'INSERT INTO sale_items (code, name, category, created_at) VALUES (?, ?, ?, 0)',
    code, name, category
  );

const idOf = (table, code) => get(`SELECT id FROM ${table} WHERE code = ?`, code).id;

check('catalogue: the §2.1 worked example builds', () => {
  product('GIN', 'Gin', 'SHOT', 24000);
  product('SG', 'St. George', 'BOTTLE', 1000);
  product('TON', 'Tonic', 'BOTTLE', 1000);
  saleItem('GIN-BTL', 'Gin, bottle', 'BOTTLE');
  saleItem('GIN-SHOT', 'Gin, shot', 'SHOT');
  saleItem('GT', 'Gin & Tonic', 'COCKTAIL');
  saleItem('BEER-SG', 'St. George', 'BEER');
  assert(get('SELECT COUNT(*) AS n FROM products').n === 3, 'products');
  assert(get('SELECT COUNT(*) AS n FROM sale_items').n === 4, 'sale items');
});

check('products: unknown base unit or destination is refused', () => {
  rejects(() => product('X1', 'Bad unit', 'CRATE', 1000), 'CHECK');
  rejects(() => product('X2', 'Bad dest', 'BOTTLE', 1000, { destination: 'CELLAR' }), 'CHECK');
});

check('products: a zero conversion factor is refused', () => {
  // base_units_per_pack of 0 would make every yield calculation divide by zero
  // and every pack split meaningless.
  rejects(() => product('X3', 'Zero pack', 'SHOT', 0), 'CHECK');
});

check('products: are deactivated, never deleted', () => {
  rejects(() => run("DELETE FROM products WHERE code = 'GIN'"), 'never deleted');
});

check('sale items: a blank category is refused (D13 needs the bucket)', () => {
  rejects(() => saleItem('X4', 'No category', '   '), 'CHECK');
});

check('recipes: only one open version per sale item', () => {
  const now = 1_700_000_000_000;
  const gt = idOf('sale_items', 'GT');
  run(
    `INSERT INTO recipes (sale_item_id, version, effective_from, created_by)
     VALUES (?, 1, ?, NULL)`, gt, now
  );
  rejects(
    () => run(
      `INSERT INTO recipes (sale_item_id, version, effective_from, created_by)
       VALUES (?, 2, ?, NULL)`, gt, now
    ),
    'UNIQUE'
  );
});

check('recipes: a recipe MAY name the same product twice (§2.5 double measure)', () => {
  // Expansion sums rather than overwrites, so a double measure written as two
  // lines must be accepted. A UNIQUE(recipe_id, product_id) would forbid the
  // case the specification explicitly calls out.
  const r = get("SELECT id FROM recipes WHERE sale_item_id = ? AND effective_to IS NULL",
                idOf('sale_items', 'GT')).id;
  const gin = idOf('products', 'GIN');
  run('INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli) VALUES (?, ?, 1000)', r, gin);
  run('INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli) VALUES (?, ?, 1000)', r, gin);
  run('INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli) VALUES (?, ?, 500)',
      r, idOf('products', 'TON'));

  const total = get(
    'SELECT SUM(quantity_milli) AS q FROM recipe_lines WHERE recipe_id = ? AND product_id = ?',
    r, gin
  ).q;
  assert(total === 2000, `two lines of 1000 must sum to 2000, got ${total}`);
});

check('recipes: a zero or negative quantity is refused', () => {
  const r = get("SELECT id FROM recipes WHERE sale_item_id = ? AND effective_to IS NULL",
                idOf('sale_items', 'GT')).id;
  rejects(
    () => run('INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli) VALUES (?, ?, 0)',
              r, idOf('products', 'GIN')),
    'CHECK'
  );
});

check('recipes: closing a version freezes it, and it cannot reopen', () => {
  const gt = idOf('sale_items', 'GT');
  const r = get('SELECT id FROM recipes WHERE sale_item_id = ? AND effective_to IS NULL', gt).id;

  run('UPDATE recipes SET effective_to = ? WHERE id = ?', 1_700_000_100_000, r);

  rejects(
    () => run('INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli) VALUES (?, ?, 1000)',
              r, idOf('products', 'GIN')),
    'closed recipe version'
  );
  rejects(() => run('UPDATE recipe_lines SET quantity_milli = 9 WHERE recipe_id = ?', r), 'immutable');
  rejects(() => run('DELETE FROM recipe_lines WHERE recipe_id = ?', r), 'immutable');
  rejects(() => run('UPDATE recipes SET effective_to = NULL WHERE id = ?', r), 'never reopened');

  // ...and now a new version may open, because the old one is closed.
  run(`INSERT INTO recipes (sale_item_id, version, effective_from, created_by)
       VALUES (?, 2, ?, NULL)`, gt, 1_700_000_100_000);
  assert(
    get('SELECT COUNT(*) AS n FROM recipes WHERE sale_item_id = ?', gt).n === 2,
    'the sale item should now have two versions, one open'
  );
});

check('prices: only one open price per sale item, and never negative', () => {
  const beer = idOf('sale_items', 'BEER-SG');
  run(`INSERT INTO prices (sale_item_id, price_minor, effective_from) VALUES (?, 5000, ?)`,
      beer, 1_700_000_000_000);
  rejects(
    () => run('INSERT INTO prices (sale_item_id, price_minor, effective_from) VALUES (?, 5500, ?)',
              beer, 1_700_000_100_000),
    'UNIQUE'
  );
  rejects(
    () => run('INSERT INTO prices (sale_item_id, price_minor, effective_from) VALUES (?, -1, ?)',
              idOf('sale_items', 'GT'), 0),
    'CHECK'
  );
});

check('prices: a price change closes the old one and opens a new one', () => {
  const beer = idOf('sale_items', 'BEER-SG');
  run('UPDATE prices SET effective_to = ? WHERE sale_item_id = ? AND effective_to IS NULL',
      1_700_000_100_000, beer);
  run('INSERT INTO prices (sale_item_id, price_minor, effective_from) VALUES (?, 5500, ?)',
      beer, 1_700_000_100_000);

  const current = get(
    'SELECT price_minor FROM prices WHERE sale_item_id = ? AND effective_to IS NULL', beer
  ).price_minor;
  assert(current === 5500, `current price should be 5500, got ${current}`);
  assert(
    get('SELECT COUNT(*) AS n FROM prices WHERE sale_item_id = ?', beer).n === 2,
    'the old price must survive so historical bills stay explicable'
  );
  rejects(() => run('DELETE FROM prices WHERE sale_item_id = ?', beer), 'never deleted');
});

// ---------------------------------------------------------------------------
// §4 / §5 — shifts, tabs, transfers
//
// These run as one narrative night, in order, because that is the only way to
// test a lifecycle: a tab cannot be closed before it is opened, and a shift
// cannot be frozen before it is closed.
// ---------------------------------------------------------------------------

const T0 = 1_700_000_000_000;
const HOUR = 3_600_000;

const openShift = (code, date, by, at = T0) =>
  run(
    `INSERT INTO shifts (code, business_date, opened_at, opened_by,
                         opening_float_minor, expected_end_at)
     VALUES (?, ?, ?, ?, 200000, ?)`,
    code, date, at, by, at + 12 * HOUR
  );

check('shifts: only an owner or cashier can open a night (D22)', () => {
  // A waiter has no PIN and cannot log in, so they cannot be the person who
  // opened the till.
  rejects(
    () => openShift('SHIFT-000000', '2026-08-10', idOf('staff', 'W01')),
    'owner or cashier'
  );
});

check('shifts: a night opens against a business date, not a timestamp', () => {
  openShift('SHIFT-000001', '2026-08-11', idOf('staff', 'C01'));
  const s = get("SELECT status, business_date FROM shifts WHERE code = 'SHIFT-000001'");
  assert(s.status === 'OPEN', `a new shift should be OPEN, got ${s.status}`);
  assert(s.business_date === '2026-08-11', 'the business date is stored as given');
});

check('shifts: at most one may be OPEN, ever (INV-9)', () => {
  // Two open shifts would split one night's takings across two reports, and
  // neither would balance.
  rejects(() => openShift('SHIFT-000002', '2026-08-12', idOf('staff', 'C01')), 'UNIQUE');
});

check('shifts: the business date must be YYYY-MM-DD', () => {
  rejects(() => openShift('SHIFT-000003', '11-08-2026', idOf('staff', 'C01')), 'CHECK');
});

check('shifts: cannot be conjured directly into a closed state', () => {
  rejects(
    () => run(
      `INSERT INTO shifts (code, business_date, status, opened_at, opened_by,
                           expected_end_at, closed_at, closed_by, counted_cash_minor)
       VALUES ('SHIFT-000004', '2026-08-13', 'CLOSED', ?, ?, ?, ?, ?, 0)`,
      T0, idOf('staff', 'C01'), T0 + HOUR, T0 + HOUR, idOf('staff', 'C01')
    ),
    'always created OPEN'
  );
});

// --- tabs, while the night is trading ---------------------------------------

const openTab = (code, mode, label, waiterCode, fields = {}, shiftCode = 'SHIFT-000001') =>
  run(
    `INSERT INTO tabs (code, opened_shift_id, waiter_id, reference_mode,
                       table_no, customer_name, customer_phone, custom_ref,
                       display_label, opened_at, opened_by)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    code, idOf('shifts', shiftCode), idOf('staff', waiterCode), mode,
    fields.table_no ?? null, fields.customer_name ?? null,
    fields.customer_phone ?? null, fields.custom_ref ?? null,
    label, T0 + HOUR, idOf('staff', 'C01')
  );

check('tabs: a tab opens on the trading shift and belongs to a waiter', () => {
  insertStaff(['W02', 'Second Waiter', 'WAITER', null, null, 0]);
  openTab('TAB-000001', 'TABLE', 'Table 7', 'W01', { table_no: '7' });
  const t = get("SELECT status, display_label FROM tabs WHERE code = 'TAB-000001'");
  assert(t.status === 'OPEN' && t.display_label === 'Table 7', 'tab did not open as expected');
});

check('tabs: a tab cannot belong to a cashier', () => {
  // Liability follows the person who carried the drinks, not the person at the
  // till.
  rejects(
    () => openTab('TAB-000002', 'TABLE', 'Table 8', 'C01', { table_no: '8' }),
    'belongs to a waiter'
  );
});

check('tabs: the reference mode must actually be answerable (§5.1)', () => {
  // A CUSTOM tab with no custom_ref has a label nothing can rebuild or verify.
  rejects(() => openTab('TAB-000003', 'CUSTOM', 'Birthday', 'W01'), 'CHECK');
  rejects(
    () => openTab('TAB-000004', 'CUSTOMER_NAME', 'Selam', 'W01', { table_no: '9' }),
    'CHECK'
  );
  // With the field the mode names, the same tab is fine.
  openTab('TAB-000005', 'CUSTOMER_PHONE', 'Selam (0911...)', 'W01',
          { customer_name: 'Selam', customer_phone: '0911000000' });
});

check('tabs: the label is unique among OPEN tabs only (§5.1)', () => {
  rejects(
    () => openTab('TAB-000006', 'TABLE', 'Table 7', 'W02', { table_no: '7' }),
    'UNIQUE'
  );
});

check('tabs: identity and reference are frozen at open', () => {
  rejects(
    () => run("UPDATE tabs SET display_label = 'Table 9' WHERE code = 'TAB-000001'"),
    'frozen at open'
  );
  rejects(
    () => run("UPDATE tabs SET reference_mode = 'CUSTOM' WHERE code = 'TAB-000001'"),
    'frozen at open'
  );
});

// --- transfers ---------------------------------------------------------------

const transfer = (tabCode, fromCode, toCode) =>
  run(
    `INSERT INTO tab_transfers (tab_id, from_waiter_id, to_waiter_id, shift_id,
                                transferred_at, transferred_by, reason)
     VALUES (?, ?, ?, ?, ?, ?, 'went home')`,
    idOf('tabs', tabCode), idOf('staff', fromCode), idOf('staff', toCode),
    idOf('shifts', 'SHIFT-000001'), T0 + 2 * HOUR, idOf('staff', 'C01')
  );

check('tab transfers: responsibility moves, and the log records it (§5.4)', () => {
  transfer('TAB-000001', 'W01', 'W02');
  run('UPDATE tabs SET waiter_id = ? WHERE code = ?', idOf('staff', 'W02'), 'TAB-000001');

  const t = get("SELECT waiter_id FROM tabs WHERE code = 'TAB-000001'");
  assert(t.waiter_id === idOf('staff', 'W02'), 'the tab should now sit with the second waiter');
  assert(
    get('SELECT COUNT(*) AS n FROM tab_transfers WHERE tab_id = ?',
        idOf('tabs', 'TAB-000001')).n === 1,
    'the transfer must leave a trace'
  );
});

check('tab transfers: the row must agree with the tab it claims to move', () => {
  // The tab now sits with W02, so a transfer claiming to move it off W01 is
  // describing a state that no longer exists.
  rejects(() => transfer('TAB-000001', 'W01', 'W02'), "tab's current waiter");
  rejects(() => transfer('TAB-000001', 'W02', 'C01'), 'transfer to a waiter');
});

check('tab transfers: append-only (§11.1)', () => {
  rejects(() => run("UPDATE tab_transfers SET reason = 'edited'"), 'append-only');
  rejects(() => run('DELETE FROM tab_transfers'), 'append-only');
});

// --- closing a tab -----------------------------------------------------------

const closeTab = (code) =>
  run(
    `UPDATE tabs SET status = 'CLOSED', closed_at = ?, closed_by = ?, closed_shift_id = ?
      WHERE code = ?`,
    T0 + 3 * HOUR, idOf('staff', 'C01'), idOf('shifts', 'SHIFT-000001'), code
  );

check('tabs: closing without the closing facts is refused', () => {
  rejects(
    () => run("UPDATE tabs SET status = 'CLOSED' WHERE code = 'TAB-000001'"),
    'closing requires'
  );
});

check('tabs: a closed tab is NEVER reopened (§5.2)', () => {
  closeTab('TAB-000001');
  rejects(
    () => run("UPDATE tabs SET status = 'OPEN' WHERE code = 'TAB-000001'"),
    'never reopened'
  );
  rejects(() => run("DELETE FROM tabs WHERE code = 'TAB-000001'"), 'never deleted');
});

check('tabs: the label frees up once the tab closes — one table, many parties', () => {
  openTab('TAB-000007', 'TABLE', 'Table 7', 'W01', { table_no: '7' });
  assert(
    get("SELECT COUNT(*) AS n FROM tabs WHERE display_label = 'Table 7'").n === 2,
    'the same table should be reusable across parties'
  );
});

check('tabs: a closed tab freezes its waiter, and cannot be transferred', () => {
  rejects(
    () => run('UPDATE tabs SET waiter_id = ? WHERE code = ?',
              idOf('staff', 'W01'), 'TAB-000001'),
    'frozen'
  );
  rejects(() => transfer('TAB-000001', 'W02', 'W01'), 'only an open tab');
});

check('tabs: CLOSED -> RECONCILED is permitted', () => {
  run("UPDATE tabs SET status = 'RECONCILED' WHERE code = 'TAB-000001'");
  assert(
    get("SELECT status FROM tabs WHERE code = 'TAB-000001'").status === 'RECONCILED',
    'a settled tab should reach RECONCILED'
  );
  rejects(
    () => run("UPDATE tabs SET status = 'OPEN' WHERE code = 'TAB-000001'"),
    'never reopened'
  );
});

// --- closing the night -------------------------------------------------------

check('shifts: OPEN -> CLOSED directly is refused (§4.2)', () => {
  // CLOSING exists so the drawer can be counted without the till still taking
  // sales behind the cashier.
  rejects(
    () => run(
      `UPDATE shifts SET status = 'CLOSED', closed_at = ?, closed_by = ?,
                         counted_cash_minor = 1590000
        WHERE code = 'SHIFT-000001'`,
      T0 + 12 * HOUR, idOf('staff', 'C01')
    ),
    'OPEN -> CLOSING -> CLOSED'
  );
});

check('shifts: closing requires counted cash, then the night is frozen', () => {
  run("UPDATE shifts SET status = 'CLOSING' WHERE code = 'SHIFT-000001'");
  rejects(
    () => run("UPDATE shifts SET status = 'CLOSED' WHERE code = 'SHIFT-000001'"),
    'counted cash'
  );

  run(
    `UPDATE shifts SET status = 'CLOSED', closed_at = ?, closed_by = ?,
                       counted_cash_minor = 1590000
      WHERE code = 'SHIFT-000001'`,
    T0 + 12 * HOUR, idOf('staff', 'C01')
  );

  // The report is stored and may already be on paper. Changing the counted
  // cash now would make the two disagree with no trace.
  rejects(
    () => run("UPDATE shifts SET counted_cash_minor = 1 WHERE code = 'SHIFT-000001'"),
    'frozen'
  );
  rejects(() => run("DELETE FROM shifts WHERE code = 'SHIFT-000001'"), 'never deleted');
});

check('shifts: the same night cannot trade twice (§11.3)', () => {
  rejects(() => openShift('SHIFT-000008', '2026-08-11', idOf('staff', 'C01')), 'UNIQUE');
});

check('tabs: no tab can be opened without a trading shift', () => {
  // Otherwise a tab could attach itself to a night whose report has already
  // been generated and signed.
  rejects(
    () => openTab('TAB-000009', 'TABLE', 'Table 12', 'W01', { table_no: '12' }),
    'only be opened in an open shift'
  );
});

check('tabs: an open tab survives the close of its shift (§4.5)', () => {
  // Both open tabs and unsettled waiters legitimately carry over. Neither
  // blocks the close; they are shown for acknowledgement.
  const t = get("SELECT status FROM tabs WHERE code = 'TAB-000007'");
  assert(t.status === 'OPEN', 'a tab opened before the close must still be open after it');
});

// ---------------------------------------------------------------------------
// §6 — orders, the print state machine, corrections and voids
//
// The night continues: SHIFT-000001 is closed, so a new night opens and
// TAB-000007 — still open from before — carries on taking rounds. That is not
// test convenience; §5.2 says tabs cross nights and this proves it.
// ---------------------------------------------------------------------------

openShift('SHIFT-000002', '2026-08-12', idOf('staff', 'C01'), T0 + 24 * HOUR);

const S2 = idOf('shifts', 'SHIFT-000002');
const TAB = idOf('tabs', 'TAB-000007');
const W1 = idOf('staff', 'W01');
const C1 = idOf('staff', 'C01');

// A recipe for the beer, so order lines have a real version to snapshot.
run(`INSERT INTO recipes (sale_item_id, version, effective_from) VALUES (?, 1, ?)`,
    idOf('sale_items', 'BEER-SG'), T0);
const BEER_RECIPE = get(
  'SELECT id FROM recipes WHERE sale_item_id = ? AND effective_to IS NULL',
  idOf('sale_items', 'BEER-SG')
).id;
run('INSERT INTO recipe_lines (recipe_id, product_id, quantity_milli) VALUES (?, ?, 1000)',
    BEER_RECIPE, idOf('products', 'SG'));

const draftOrder = (shift = S2, tab = TAB, at = T0 + 25 * HOUR) =>
  Number(
    run(
      `INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at)
       VALUES (?, ?, ?, ?, ?)`,
      tab, shift, W1, C1, at
    ).lastInsertRowid
  );

/** Two beers at 55.00 — 2000 milli x 5500 santim = 110.00. */
const addLine = (orderId, qtyMilli = 2000, price = 5500, total = 11000) =>
  run(
    `INSERT INTO order_lines (order_id, sale_item_id, sale_item_name, recipe_id,
                              quantity_milli, unit_price_minor, line_total_minor)
     VALUES (?, ?, 'St. George', ?, ?, ?, ?)`,
    orderId, idOf('sale_items', 'BEER-SG'), BEER_RECIPE, qtyMilli, price, total
  );

/**
 * The §6.3 three-transaction protocol, in order: burn the number and commit
 * PRINTING, freeze the text, touch the device, then confirm. Tests run it in
 * full because the correction control (§6.4) validates the typed BR number
 * against a real printed slip — a fabricated number must not work here either.
 */
const issueSlip = (orderId, at) => {
  const shift = get('SELECT shift_id FROM orders WHERE id = ?', orderId).shift_id;
  const seq = get(
    `UPDATE sequences SET next_value = next_value + 1
      WHERE name = 'ISSUE_RECEIPT' RETURNING next_value - 1 AS seq`
  ).seq;
  const number = `BR-${String(seq).padStart(6, '0')}`;
  const rid = Number(
    run(
      `INSERT INTO receipts (receipt_type, sequence_no, receipt_number, order_id,
                             destination, waiter_name, shift_id, created_at)
       VALUES ('ISSUE', ?, ?, ?, 'BAR', 'Test Waiter', ?, ?)`,
      seq, number, orderId, shift, at
    ).lastInsertRowid
  );
  run("UPDATE orders SET status = 'PRINTING' WHERE id = ?", orderId);
  run('UPDATE receipts SET rendered_text = ? WHERE id = ?',
      `BAR ISSUE RECEIPT\n${number}\n`, rid);
  return { rid, number, shift };
};

const issueOrder = (orderId, at = T0 + 26 * HOUR) => {
  const { rid, number, shift } = issueSlip(orderId, at);
  run("UPDATE receipts SET status = 'PRINTED', printed_at = ? WHERE id = ?", at, rid);
  run(
    `INSERT INTO receipt_prints (receipt_id, print_no, outcome, shift_id, created_by, created_at)
     VALUES (?, 1, 'SUCCESS', ?, ?, ?)`,
    rid, shift, C1, at
  );
  run("UPDATE orders SET status = 'ISSUED', issued_at = ? WHERE id = ?", at, orderId);
  return number;
};

/** The live slip number for an order — what the cashier types off the paper. */
const brOf = (orderId) =>
  get("SELECT receipt_number FROM receipts WHERE order_id = ? AND status <> 'VOID'",
      orderId).receipt_number;

let O1;

check('orders: a round is drafted, priced, then issued through PRINTING', () => {
  O1 = draftOrder();
  addLine(O1);
  assert(get('SELECT status FROM orders WHERE id = ?', O1).status === 'DRAFT',
         'an order starts as a draft');
  issueOrder(O1);
  assert(get('SELECT status FROM orders WHERE id = ?', O1).status === 'ISSUED',
         'the order should be issued');
});

check('orders: the line total must be the quantity times the price (§1.1)', () => {
  // A service-layer rounding bug would otherwise be invisible until an owner
  // added up a receipt by hand.
  const o = draftOrder();
  rejects(() => addLine(o, 2000, 5500, 11001), 'CHECK');
  rejects(() => addLine(o, 1500, 3333, 4999), 'CHECK');
  addLine(o, 1500, 3333, 5000);   // 4999.5 rounds half up
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", o);
});

check('orders: DRAFT cannot jump straight to ISSUED (§6.2)', () => {
  // PRINTING commits before the printer is touched. Skipping it is exactly the
  // bug the two-transaction protocol exists to prevent.
  const o = draftOrder();
  addLine(o);
  rejects(
    () => run("UPDATE orders SET status = 'ISSUED', issued_at = 1 WHERE id = ?", o),
    'state machine'
  );
  // The permitted retry path after a confirmed non-print is PRINTING -> DRAFT.
  run("UPDATE orders SET status = 'PRINTING' WHERE id = ?", o);
  run("UPDATE orders SET status = 'DRAFT' WHERE id = ?", o);
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", o);
  rejects(
    () => run("UPDATE orders SET status = 'DRAFT' WHERE id = ?", o),
    'state machine'
  );
});

check('orders: lines cannot be added once the slip is being printed', () => {
  rejects(() => addLine(O1), 'only be added to a draft');
  rejects(() => run('UPDATE order_lines SET quantity_milli = 1 WHERE order_id = ?', O1),
          'append-only');
  rejects(() => run('DELETE FROM order_lines WHERE order_id = ?', O1), 'append-only');
});

check('orders: are never deleted, and issuing needs a time', () => {
  rejects(() => run('DELETE FROM orders WHERE id = ?', O1), 'never deleted');
  const o = draftOrder();
  addLine(o);
  run("UPDATE orders SET status = 'PRINTING' WHERE id = ?", o);
  rejects(() => run("UPDATE orders SET status = 'ISSUED' WHERE id = ?", o), 'issued_at');
  run("UPDATE orders SET status = 'DRAFT' WHERE id = ?", o);
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", o);
});

// --- correction chains -------------------------------------------------------

check('orders: a replacement must carry the root of the chain it joins', () => {
  rejects(
    () => run(
      `INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at,
                           replaces_order_id, root_order_id)
       VALUES (?, ?, ?, ?, ?, ?, NULL)`,
      TAB, S2, W1, C1, T0 + 27 * HOUR, O1
    ),
    'root of the chain'
  );
  // ...and a first order is its own root, so it may not claim another.
  rejects(
    () => run(
      `INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at,
                           replaces_order_id, root_order_id)
       VALUES (?, ?, ?, ?, ?, NULL, ?)`,
      TAB, S2, W1, C1, T0 + 27 * HOUR, O1
    ),
    'root of the chain'
  );
});

const replacementFor = (originalId) =>
  Number(
    run(
      `INSERT INTO orders (tab_id, shift_id, waiter_id, cashier_id, created_at,
                           replaces_order_id, root_order_id)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
      TAB, S2, W1, C1, T0 + 27 * HOUR, originalId,
      get('SELECT COALESCE(root_order_id, id) AS root FROM orders WHERE id = ?', originalId).root
    ).lastInsertRowid
  );

check('orders: an order may be replaced at most once (INV-5)', () => {
  // Two replacements would fork the chain, and BOTH leaves would count toward
  // the tab — one round billed twice.
  const r1 = replacementFor(O1);
  rejects(() => replacementFor(O1), 'UNIQUE');

  // §6.5 abandon path: the slot is released by clearing the link while the
  // replacement is back in DRAFT, and only then may another attempt be made.
  run("UPDATE orders SET status = 'PRINTING' WHERE id = ?", r1);
  run("UPDATE orders SET status = 'DRAFT' WHERE id = ?", r1);
  run('UPDATE orders SET replaces_order_id = NULL WHERE id = ?', r1);
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", r1);

  const r2 = replacementFor(O1);
  assert(r2 > 0, 'a fresh correction attempt should be possible after abandoning');
});

check('orders: a draft may be carried into tonight, an issued order may not', () => {
  // §6.3: a failed-print draft is not a sale yet, so its retry belongs to the
  // current night. An issued order is money already reported on.
  const o = draftOrder();
  addLine(o);
  run('UPDATE orders SET shift_id = ? WHERE id = ?', S2, o);
  rejects(() => run('UPDATE orders SET shift_id = ? WHERE id = ?',
                    idOf('shifts', 'SHIFT-000001'), O1),
          'only a draft');
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", o);
});

check('orders: only an issued order can be replaced', () => {
  const o = draftOrder();
  addLine(o);
  rejects(() => replacementFor(o), 'only an issued order can be replaced');
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", o);
});

// --- the correction itself ---------------------------------------------------

check('corrections: returned plus written off must equal what left the bill', () => {
  const replacement = get(
    'SELECT id FROM orders WHERE replaces_order_id = ?', O1
  ).id;
  issueOrder(replacement, T0 + 28 * HOUR);

  // §6.4: the number is typed off the slip in the cashier's hand, and it is
  // validated. An invented one makes the whole control cosmetic.
  rejects(
    () => run(
      `INSERT INTO order_corrections (correction_type, original_order_id,
                                      replacement_order_id, issue_receipt_number,
                                      reason, shift_id, created_by, created_at)
       VALUES ('CORRECTION', ?, ?, 'BR-999999', 'invented number', ?, ?, ?)`,
      O1, replacement, S2, C1, T0 + 28 * HOUR
    ),
    'not a printed slip for this order'
  );

  run(
    `INSERT INTO order_corrections (correction_type, original_order_id,
                                    replacement_order_id, issue_receipt_number,
                                    reason, shift_id, created_by, created_at)
     VALUES ('CORRECTION', ?, ?, ?, 'wrong drink poured', ?, ?, ?)`,
    O1, replacement, brOf(O1), S2, C1, T0 + 28 * HOUR
  );
  const cid = get('SELECT id FROM order_corrections WHERE original_order_id = ?', O1).id;
  const sg = idOf('products', 'SG');

  const line = (before, after, returned, writtenOff) =>
    run(
      `INSERT INTO order_correction_lines (correction_id, product_id, before_milli,
                                           after_milli, delta_milli, returned_milli,
                                           written_off_milli)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
      cid, sg, before, after, after - before, returned, writtenOff
    );

  // One bottle taken off the bill: it either came back or it did not, and the
  // difference is the write-off nobody can hide.
  rejects(() => line(2000, 1000, 400, 400), 'CHECK');   // 800 <> 1000
  rejects(() => line(2000, 1000, 1500, 0), 'CHECK');    // more back than removed
  rejects(() => line(1000, 2000, 500, 0), 'CHECK');     // nothing removed, so nothing returned
  line(2000, 1000, 600, 400);                           // 600 back, 400 written off

  run("UPDATE orders SET status = 'REPLACED' WHERE id = ?", O1);
  assert(get('SELECT status FROM orders WHERE id = ?', O1).status === 'REPLACED',
         'the original should end up replaced, never reversed');
});

check('corrections: an order is corrected at most once, and never edited', () => {
  // Once corrected the original is REPLACED, and a replaced order is not
  // correctable — the first line of defence.
  rejects(
    () => run(
      `INSERT INTO order_corrections (correction_type, original_order_id,
                                      issue_receipt_number, reason, shift_id,
                                      created_by, created_at)
       VALUES ('VOID', ?, ?, 'again', ?, ?, ?)`,
      O1, brOf(O1), S2, C1, T0 + 29 * HOUR
    ),
    'only an issued order'
  );

  // The second line of defence catches the race inside the transaction, where
  // the original is still ISSUED because the status update has not run yet.
  const o = draftOrder();
  addLine(o);
  issueOrder(o, T0 + 29 * HOUR);
  const insertVoid = () => run(
    `INSERT INTO order_corrections (correction_type, original_order_id,
                                    issue_receipt_number, reason, shift_id,
                                    created_by, created_at)
     VALUES ('VOID', ?, ?, 'spilled', ?, ?, ?)`,
    o, brOf(o), S2, C1, T0 + 29 * HOUR
  );
  insertVoid();
  rejects(insertVoid, 'UNIQUE');

  run(
    `UPDATE orders SET status = 'VOIDED', void_reason = 'spilled',
                       voided_at = ?, voided_by = ? WHERE id = ?`,
    T0 + 29 * HOUR, C1, o
  );

  rejects(() => run("UPDATE order_corrections SET reason = 'edited'"), 'append-only');
  rejects(() => run('DELETE FROM order_correction_lines'), 'append-only');
});

check('corrections: a void carries no replacement, a correction must have one', () => {
  const o = draftOrder();
  addLine(o);
  issueOrder(o, T0 + 29 * HOUR);

  rejects(
    () => run(
      `INSERT INTO order_corrections (correction_type, original_order_id,
                                      issue_receipt_number, reason, shift_id,
                                      created_by, created_at)
       VALUES ('CORRECTION', ?, ?, 'no replacement', ?, ?, ?)`,
      o, brOf(o), S2, C1, T0 + 29 * HOUR
    ),
    'CHECK'
  );

  run(
    `INSERT INTO order_corrections (correction_type, original_order_id,
                                    issue_receipt_number, reason, shift_id,
                                    created_by, created_at)
     VALUES ('VOID', ?, ?, 'customer left', ?, ?, ?)`,
    o, brOf(o), S2, C1, T0 + 29 * HOUR
  );

  // §6.6: a void must always answer who, when and why.
  rejects(() => run("UPDATE orders SET status = 'VOIDED' WHERE id = ?", o), 'requires a reason');
  run(
    `UPDATE orders SET status = 'VOIDED', void_reason = 'customer left',
                       voided_at = ?, voided_by = ? WHERE id = ?`,
    T0 + 29 * HOUR, C1, o
  );
  assert(get('SELECT status FROM orders WHERE id = ?', o).status === 'VOIDED', 'void failed');
});

check('corrections: the frozen intent is never edited, and lines go first', () => {
  const original = draftOrder();
  addLine(original);
  issueOrder(original, T0 + 30 * HOUR);
  const replacement = replacementFor(original);

  run(
    `INSERT INTO pending_order_corrections (original_order_id, replacement_order_id,
                                            issue_receipt_number, reason, shift_id,
                                            created_by, created_at)
     VALUES (?, ?, ?, 'one too many', ?, ?, ?)`,
    original, replacement, brOf(original), S2, C1, T0 + 30 * HOUR
  );
  const pid = get('SELECT id FROM pending_order_corrections WHERE original_order_id = ?',
                  original).id;
  run(
    `INSERT INTO pending_order_correction_lines (pending_id, product_id, before_milli,
                                                 after_milli, delta_milli,
                                                 returned_milli, written_off_milli)
     VALUES (?, ?, 2000, 1000, -1000, 1000, 0)`,
    pid, idOf('products', 'SG')
  );

  rejects(() => run("UPDATE pending_order_corrections SET reason = 'x'"), 'never edited');
  rejects(() => run('DELETE FROM pending_order_corrections WHERE id = ?', pid),
          'lines before the intent');

  // Completion deletes the intent — lines first, so recovery never sees a
  // dangling line that looks like outstanding work.
  run('DELETE FROM pending_order_correction_lines WHERE pending_id = ?', pid);
  run('DELETE FROM pending_order_corrections WHERE id = ?', pid);
  assert(get('SELECT COUNT(*) AS n FROM pending_order_corrections').n === 0,
         'the intent should be cleared on completion');
});

// --- the shift boundary ------------------------------------------------------

check('corrections: an order cannot be corrected after its night has closed (§6.5)', () => {
  const stranded = draftOrder();
  addLine(stranded);
  issueOrder(stranded, T0 + 31 * HOUR);

  run("UPDATE shifts SET status = 'CLOSING' WHERE id = ?", S2);
  run(`UPDATE shifts SET status = 'CLOSED', closed_at = ?, closed_by = ?,
                         counted_cash_minor = 0 WHERE id = ?`,
      T0 + 36 * HOUR, C1, S2);
  openShift('SHIFT-000003', '2026-08-13', C1, T0 + 48 * HOUR);
  const S3 = idOf('shifts', 'SHIFT-000003');

  // The money is banked and the report is stored. The remedy now is a stock
  // adjustment and a written note — never a restatement.
  rejects(
    () => run(
      `INSERT INTO order_corrections (correction_type, original_order_id,
                                      issue_receipt_number, reason, shift_id,
                                      created_by, created_at)
       VALUES ('VOID', ?, ?, 'too late', ?, ?, ?)`,
      stranded, brOf(stranded), S3, C1, T0 + 49 * HOUR
    ),
    'own shift'
  );
});

check('orders: a tab keeps trading across nights (§5.2)', () => {
  const o = draftOrder(idOf('shifts', 'SHIFT-000003'), TAB, T0 + 49 * HOUR);
  addLine(o);
  issueOrder(o, T0 + 49 * HOUR);
  assert(
    get('SELECT COUNT(*) AS n FROM orders WHERE tab_id = ? AND status = ?', TAB, 'ISSUED').n >= 2,
    'the same tab should carry issued orders from two different nights'
  );
});

// ---------------------------------------------------------------------------
// §6.7 - §6.10 — receipts and print attempts
// ---------------------------------------------------------------------------

const S3 = idOf('shifts', 'SHIFT-000003');

check('receipts: an issue slip physically cannot carry money (§6.7)', () => {
  // A slip with no money on it cannot be mistaken for a bill if a customer is
  // handed one by accident.
  const o = draftOrder(S3, TAB, T0 + 50 * HOUR);
  addLine(o);
  rejects(
    () => run(
      `INSERT INTO receipts (receipt_type, sequence_no, receipt_number, order_id,
                             destination, waiter_name, total_minor, shift_id, created_at)
       VALUES ('ISSUE', 9001, 'BR-909001', ?, 'BAR', 'Test Waiter', 11000, ?, ?)`,
      o, S3, T0 + 50 * HOUR
    ),
    'CHECK'
  );
  run("UPDATE orders SET status = 'ABANDONED' WHERE id = ?", o);
});

check('receipts: a receipt is an order slip or a tab receipt, never both', () => {
  rejects(
    () => run(
      `INSERT INTO receipts (receipt_type, sequence_no, receipt_number, order_id,
                             tab_id, destination, waiter_name, shift_id, created_at)
       VALUES ('ISSUE', 9002, 'BR-909002', ?, ?, 'BAR', 'Test Waiter', ?, ?)`,
      O1, TAB, S3, T0 + 50 * HOUR
    ),
    'CHECK'
  );
});

check('receipts: an abandoned number is retained as VOID, never reused (§6.8)', () => {
  const o = draftOrder(S3, TAB, T0 + 51 * HOUR);
  addLine(o);
  const first = issueSlip(o, T0 + 51 * HOUR);

  // "No, nothing printed": the number dies, the order goes back to draft.
  run("UPDATE receipts SET status = 'VOID' WHERE id = ?", first.rid);
  run("UPDATE orders SET status = 'DRAFT' WHERE id = ?", o);

  const second = issueOrder(o, T0 + 52 * HOUR);
  assert(second !== first.number, 'the retry must burn a fresh number');
  assert(
    get('SELECT status FROM receipts WHERE receipt_number = ?', first.number).status === 'VOID',
    'the abandoned number must survive so the sequence stays gapless'
  );
  rejects(() => run('DELETE FROM receipts WHERE receipt_number = ?', first.number),
          'never deleted');
});

check('receipts: two live slips for one round are impossible', () => {
  rejects(
    () => run(
      `INSERT INTO receipts (receipt_type, sequence_no, receipt_number, order_id,
                             destination, waiter_name, shift_id, created_at)
       VALUES ('ISSUE', 9003, 'BR-909003', ?, 'BAR', 'Test Waiter', ?, ?)`,
      O1, S3, T0 + 52 * HOUR
    ),
    'UNIQUE'
  );
});

check('receipts: the rendered text is written once, while pending (§6.3)', () => {
  const rid = get('SELECT id FROM receipts WHERE order_id = ? AND status = ?',
                  O1, 'PRINTED').id;
  rejects(() => run('UPDATE receipts SET rendered_text = ? WHERE id = ?', 'tampered', rid),
          'written once');
  // Re-storing byte-identical text is the safe no-op that makes a retry idempotent.
  const same = get('SELECT rendered_text AS t FROM receipts WHERE id = ?', rid).t;
  run('UPDATE receipts SET rendered_text = ? WHERE id = ?', same, rid);
});

check('receipts: identity, actors and money are immutable (§11.2)', () => {
  const rid = get('SELECT id FROM receipts WHERE order_id = ? AND status = ?',
                  O1, 'PRINTED').id;
  rejects(() => run("UPDATE receipts SET waiter_name = 'Someone Else' WHERE id = ?", rid),
          'immutable');
  rejects(() => run("UPDATE receipts SET receipt_number = 'BR-000999' WHERE id = ?", rid),
          'immutable');
  rejects(() => run("UPDATE receipts SET status = 'PENDING' WHERE id = ?", rid),
          'not permitted');
});

check('receipts: a customer receipt needs a closed tab, and there is only one', () => {
  // §6.9: never for an open tab — the bill must be frozen first, or the
  // document contradicts itself the moment the next round lands.
  const customerReceipt = (tabId, seq, number) => run(
    `INSERT INTO receipts (receipt_type, sequence_no, receipt_number, tab_id,
                           waiter_name, cashier_name, subtotal_minor,
                           service_charge_minor, tax_minor, total_minor,
                           tax_rate_bp, service_rate_bp, tax_inclusive,
                           shift_id, created_at)
     VALUES ('CUSTOMER', ?, ?, ?, 'Test Waiter', 'Test Cashier',
             22000, 2200, 0, 24200, 0, 1000, 1, ?, ?)`,
    seq, number, tabId, S3, T0 + 53 * HOUR
  );

  rejects(() => customerReceipt(TAB, 1, 'CR-000001'), 'needs a closed tab');

  const settled = idOf('tabs', 'TAB-000001');   // RECONCILED earlier tonight
  customerReceipt(settled, 1, 'CR-000001');
  rejects(() => customerReceipt(settled, 2, 'CR-000002'), 'UNIQUE');
});

check('receipt prints: the attempt is UNKNOWN before the printer is touched', () => {
  const rid = get("SELECT id FROM receipts WHERE receipt_number = 'CR-000001'").id;
  run('UPDATE receipts SET rendered_text = ? WHERE id = ?', 'CUSTOMER RECEIPT\nCR-000001\n', rid);

  const attempt = (no, outcome, reason = '') => run(
    `INSERT INTO receipt_prints (receipt_id, print_no, outcome, reason,
                                 shift_id, created_by, created_at)
     VALUES (?, ?, ?, ?, ?, ?, ?)`,
    rid, no, outcome, reason, S3, C1, T0 + 53 * HOUR
  );

  attempt(1, 'UNKNOWN');

  // A reprint of a document that never printed is not a reprint — it is a
  // first print, and it goes through the normal path.
  rejects(() => attempt(2, 'UNKNOWN', 'customer asked again'), 'need a reason and a printed');

  run("UPDATE receipt_prints SET outcome = 'SUCCESS' WHERE receipt_id = ? AND print_no = 1", rid);
  run("UPDATE receipts SET status = 'PRINTED', printed_at = ? WHERE id = ?", T0 + 53 * HOUR, rid);

  rejects(() => attempt(5, 'UNKNOWN', 'skipping ahead'), 'numbered consecutively');
  rejects(() => attempt(2, 'UNKNOWN'), 'need a reason');
  attempt(2, 'UNKNOWN', 'customer lost the first copy');

  // A power cut here leaves the bytes AND an explicit question. What it must
  // never do is let a third copy stack on top of an unanswered second.
  rejects(() => attempt(3, 'UNKNOWN', 'and again'), 'resolve the outstanding');

  run("UPDATE receipt_prints SET outcome = 'SUCCESS' WHERE receipt_id = ? AND print_no = 2", rid);
});

check('receipt prints: an answered attempt stays answered (§11.2)', () => {
  const rid = get("SELECT id FROM receipts WHERE receipt_number = 'CR-000001'").id;
  rejects(
    () => run("UPDATE receipt_prints SET outcome = 'FAILED' WHERE receipt_id = ? AND print_no = 1", rid),
    'answered once'
  );
  rejects(() => run('DELETE FROM receipt_prints WHERE receipt_id = ?', rid), 'append-only');
  rejects(
    () => run('UPDATE receipt_prints SET print_no = 9 WHERE receipt_id = ? AND print_no = 2', rid),
    'immutable'
  );
});

// ---------------------------------------------------------------------------
// §8 / §3 — purchasing and the movement ledger
// ---------------------------------------------------------------------------

const TON = idOf('products', 'TON');

check('suppliers: normalized identity stops a duplicate master (§11.3)', () => {
  run(`INSERT INTO suppliers (name, normalized_name, phone, created_at)
       VALUES ('Dashen Brewery', 'dashen brewery', '0911', ?)`, T0);
  // Without normalization, "  DASHEN BREWERY " is a second supplier and the
  // same invoice can be received once against each.
  rejects(
    () => run(`INSERT INTO suppliers (name, normalized_name, created_at)
               VALUES ('  DASHEN BREWERY ', 'dashen brewery', ?)`, T0),
    'UNIQUE'
  );
  // The stored key must actually be the normalized name, not whatever was passed.
  rejects(
    () => run(`INSERT INTO suppliers (name, normalized_name, created_at)
               VALUES ('Meta Abo', 'Meta Abo', ?)`, T0),
    'CHECK'
  );
});

const SUPPLIER = get("SELECT id FROM suppliers WHERE normalized_name = 'dashen brewery'").id;
let PURCHASE;

check('purchases: a delivery is received with the club shut (Appendix A #4)', () => {
  // shift_id NULL is the whole point: deliveries arrive during the day. The
  // prior build made this column NOT NULL and could not receive stock at all.
  PURCHASE = Number(
    run(
      `INSERT INTO purchases (supplier_id, invoice_ref, received_at, shift_id,
                              total_cost_minor, created_by, created_at)
       VALUES (?, 'INV-4471', ?, NULL, 20000, ?, ?)`,
      SUPPLIER, T0 + 54 * HOUR, C1, T0 + 54 * HOUR
    ).lastInsertRowid
  );
  assert(get('SELECT shift_id FROM purchases WHERE id = ?', PURCHASE).shift_id === null,
         'a delivery must be recordable with no shift');
});

check('purchases: the same invoice cannot be received twice (§8.1)', () => {
  rejects(
    () => run(
      `INSERT INTO purchases (supplier_id, invoice_ref, received_at,
                              total_cost_minor, created_by, created_at)
       VALUES (?, 'INV-4471', ?, 20000, ?, ?)`,
      SUPPLIER, T0 + 55 * HOUR, C1, T0 + 55 * HOUR
    ),
    'UNIQUE'
  );
});

check('purchase lines: the unit cost must agree with the invoice total (§8.2)', () => {
  // 10 bottles for 200.00 is 20.00 each. A unit cost typed against the wrong
  // pack size would quietly poison the weighted average.
  rejects(
    () => run(
      `INSERT INTO purchase_lines (purchase_id, product_id, quantity_milli,
                                   unit_cost_minor, line_cost_minor)
       VALUES (?, ?, 10000, 800, 20000)`, PURCHASE, TON
    ),
    'CHECK'
  );
  run(
    `INSERT INTO purchase_lines (purchase_id, product_id, quantity_milli,
                                 unit_cost_minor, line_cost_minor)
     VALUES (?, ?, 10000, 2000, 20000)`, PURCHASE, TON
  );
  rejects(
    () => run(
      `INSERT INTO purchase_lines (purchase_id, product_id, quantity_milli,
                                   unit_cost_minor, line_cost_minor)
       VALUES (?, ?, 5000, 2000, 10000)`, PURCHASE, TON
    ),
    'UNIQUE'
  );
});

check('stock: a purchase movement must match its invoice line, and post once', () => {
  const post = (qty) => run(
    `INSERT INTO stock_movements (product_id, movement_type, quantity_milli,
                                  unit_cost_minor, reason, purchase_id,
                                  created_at, created_by)
     VALUES (?, 'PURCHASE', ?, 2000, 'INV-4471', ?, ?, ?)`,
    TON, qty, PURCHASE, T0 + 54 * HOUR, C1
  );
  rejects(() => post(9000), 'match its invoice line');
  post(10000);
  rejects(() => post(10000), 'UNIQUE');

  // §8.1: lines are frozen the moment stock posts, or the ledger and the
  // invoice could never be reconciled again.
  rejects(
    () => run(
      `INSERT INTO purchase_lines (purchase_id, product_id, quantity_milli,
                                   unit_cost_minor, line_cost_minor)
       VALUES (?, ?, 1000, 2000, 2000)`, PURCHASE, idOf('products', 'SG')
    ),
    'after stock has posted'
  );
});

check('stock: a correction never reverses the sale (§3.4)', () => {
  // The worked example: the receipt said 5, two came back.
  const o = draftOrder(S3, TAB, T0 + 56 * HOUR);
  addLine(o);
  issueOrder(o, T0 + 56 * HOUR);

  const move = (type, qty) => run(
    `INSERT INTO stock_movements (product_id, movement_type, quantity_milli,
                                  unit_cost_minor, order_id, shift_id,
                                  created_at, created_by)
     VALUES (?, ?, ?, 2000, ?, ?, ?, ?)`,
    TON, type, qty, o, S3, T0 + 56 * HOUR, C1
  );
  move('SALE', -5000);
  move('RETURN', 2000);

  const onHand = get(
    'SELECT COALESCE(SUM(quantity_milli), 0) AS q FROM stock_movements WHERE product_id = ?',
    TON
  ).q;
  // 10 delivered, 5 sold, 2 signed back = 7. The three unreturned bottles get
  // NO movement: they are already gone via the sale, and a LOSS as well would
  // remove the same drinks twice.
  assert(onHand === 7000, `on hand should be 7000 milli, got ${onHand}`);
});

check('stock: the sign is part of the type, not a convention (§3.2)', () => {
  const move = (type, qty, reason = 'x') => run(
    `INSERT INTO stock_movements (product_id, movement_type, quantity_milli,
                                  reason, created_at, created_by)
     VALUES (?, ?, ?, ?, ?, ?)`,
    TON, type, qty, reason, T0 + 57 * HOUR, C1
  );
  // A purchase that removes stock: the invoice-line trigger catches it first,
  // since no invoice line can ever carry a negative quantity.
  rejects(() => move('PURCHASE', -1000), 'match its invoice line');
  rejects(() => move('DAMAGE', 1000), 'CHECK');      // damage that creates stock
  // A sale with no order: caught as an unissued order, which is what a
  // missing one is.
  rejects(() => move('SALE', -1000), 'belong to an issued order');
  rejects(() => move('LOSS', -1000, '   '), 'CHECK'); // a loss with no explanation
});

check('stock: a stock correction is the only way out, and it must say why (D9)', () => {
  // Insufficient stock always blocks the sale and there is no setting to turn
  // that off. The way out of a wrong count is to fix the count, in the open.
  rejects(
    () => run(
      `INSERT INTO stock_movements (product_id, movement_type, quantity_milli,
                                    reason, created_at, created_by)
       VALUES (?, 'STOCK_CORRECTION', 500, '', ?, ?)`, TON, T0 + 57 * HOUR, C1
    ),
    'CHECK'
  );
  run(
    `INSERT INTO stock_movements (product_id, movement_type, quantity_milli,
                                  reason, shift_id, created_at, created_by)
     VALUES (?, 'STOCK_CORRECTION', 500, 'two crates found in the back', ?, ?, ?)`,
    TON, S3, T0 + 57 * HOUR, C1
  );
  assert(
    get('SELECT SUM(quantity_milli) AS q FROM stock_movements WHERE product_id = ?', TON).q === 7500,
    'the correction should move the count, in the open'
  );
});

check('stock: the ledger is append-only, always (§3.1)', () => {
  rejects(() => run('UPDATE stock_movements SET quantity_milli = 0'), 'append-only');
  rejects(() => run('DELETE FROM stock_movements'), 'append-only');
});

// --- stock counts, between shifts --------------------------------------------

check('stock counts: cannot be applied while a shift is trading (§3.5)', () => {
  const cid = Number(
    run('INSERT INTO stock_counts (counted_at, created_by) VALUES (?, ?)',
        T0 + 58 * HOUR, C1).lastInsertRowid
  );
  run(
    `INSERT INTO stock_count_lines (stock_count_id, product_id, system_milli,
                                    counted_milli, variance_milli)
     VALUES (?, ?, 7500, 7000, -500)`, cid, TON
  );
  // The shelf moves under the counter's hands while sales continue, so the
  // variance would measure nothing.
  rejects(
    () => run(`UPDATE stock_counts SET status = 'APPLIED', applied_at = ?, applied_by = ?
                WHERE id = ?`, T0 + 58 * HOUR, C1, cid),
    'apply between shifts'
  );

  // Close the night, then the count may be applied.
  run("UPDATE shifts SET status = 'CLOSING' WHERE id = ?", S3);
  run(`UPDATE shifts SET status = 'CLOSED', closed_at = ?, closed_by = ?,
                         counted_cash_minor = 0 WHERE id = ?`, T0 + 60 * HOUR, C1, S3);
  run(`UPDATE stock_counts SET status = 'APPLIED', applied_at = ?, applied_by = ?
        WHERE id = ?`, T0 + 61 * HOUR, C1, cid);

  rejects(
    () => run(`INSERT INTO stock_count_lines (stock_count_id, product_id, system_milli,
                                              counted_milli, variance_milli)
               VALUES (?, ?, 0, 0, 0)`, cid, idOf('products', 'SG')),
    'only a draft count'
  );
  rejects(() => run("UPDATE stock_counts SET note = 'x' WHERE id = ?", cid), 'frozen');
});

check('stock: an adjustment must match an unposted count line (§11.3)', () => {
  const cid = get('SELECT id FROM stock_counts ORDER BY id DESC LIMIT 1').id;
  const adjust = (qty) => run(
    `INSERT INTO stock_movements (product_id, movement_type, quantity_milli,
                                  unit_cost_minor, reason, stock_count_id,
                                  created_at, created_by)
     VALUES (?, 'ADJUSTMENT', ?, 2000, 'count', ?, ?, ?)`,
    TON, qty, cid, T0 + 61 * HOUR, C1
  );
  rejects(() => adjust(-400), 'match an unposted count line');
  adjust(-500);
  rejects(() => adjust(-500), 'match an unposted count line');

  assert(
    get('SELECT SUM(quantity_milli) AS q FROM stock_movements WHERE product_id = ?', TON).q === 7000,
    'the ledger should now agree with what was physically counted'
  );
});

// ---------------------------------------------------------------------------
// §7 — the money layer
// ---------------------------------------------------------------------------

openShift('SHIFT-000004', '2026-08-14', C1, T0 + 72 * HOUR);
const S4 = idOf('shifts', 'SHIFT-000004');

check('cash: there is NO path from a tab into the drawer (§7.6)', () => {
  // The single most important line in the specification, enforced by absence:
  // computing expected cash from sales makes the drawer look short by every
  // unreconciled tab, every night, indistinguishably from theft.
  const columns = db.prepare("PRAGMA table_info('cash_movements')").all().map((c) => c.name);
  for (const forbidden of ['tab_id', 'tab_payment_id', 'order_id']) {
    assert(!columns.includes(forbidden),
           `cash_movements must never carry ${forbidden} — that is the whole of §7.6`);
  }
});

check('tabs: the bill is frozen when the tab closes (§7.3)', () => {
  run(`UPDATE tabs SET status = 'CLOSED', closed_at = ?, closed_by = ?, closed_shift_id = ?
        WHERE code = 'TAB-000007'`, T0 + 73 * HOUR, C1, S4);

  // A bill cannot be frozen for a tab still taking orders.
  rejects(
    () => run(
      `INSERT INTO tab_payments (tab_id, waiter_id, subtotal_minor, service_charge_minor,
                                 tax_minor, total_minor, liability_minor, tax_rate_bp,
                                 service_rate_bp, tax_inclusive, shift_id, created_by, created_at)
       VALUES (?, ?, 22000, 2200, 0, 24200, 24200, 0, 1000, 1, ?, ?, ?)`,
      idOf('tabs', 'TAB-000005'), W1, S4, C1, T0 + 73 * HOUR
    ),
    'frozen when the tab closes'
  );

  run(
    `INSERT INTO tab_payments (tab_id, waiter_id, subtotal_minor, service_charge_minor,
                               tax_minor, total_minor, liability_minor, tax_rate_bp,
                               service_rate_bp, tax_inclusive, shift_id, created_by, created_at)
     VALUES (?, ?, 22000, 2200, 0, 24200, 24200, 0, 1000, 1, ?, ?, ?)`,
    TAB, W1, S4, C1, T0 + 73 * HOUR
  );
  rejects(() => run('UPDATE tab_payments SET liability_minor = 0'), 'append-only');
});

check('tabs: a comped tab carries zero liability and says why (§7.8)', () => {
  const payment = (tabId, comped, liability, reason) => run(
    `INSERT INTO tab_payments (tab_id, waiter_id, subtotal_minor, service_charge_minor,
                               tax_minor, total_minor, liability_minor, is_comped,
                               comp_reason, tax_rate_bp, service_rate_bp, tax_inclusive,
                               shift_id, created_by, created_at)
     VALUES (?, ?, 5000, 0, 0, 5000, ?, ?, ?, 0, 0, 1, ?, ?, ?)`,
    tabId, W1, liability, comped, reason, S4, C1, T0 + 73 * HOUR
  );

  openTab('TAB-000010', 'TABLE', 'Table 20', 'W01', { table_no: '20' }, 'SHIFT-000004');
  const t = idOf('tabs', 'TAB-000010');
  run(`UPDATE tabs SET status = 'CLOSED', closed_at = ?, closed_by = ?, closed_shift_id = ?,
                       is_comped = 1 WHERE id = ?`, T0 + 73 * HOUR, C1, S4, t);

  // A comp that still bills the waiter would make them liable for the house's
  // own giveaway until they got a chance to declare it.
  rejects(() => payment(t, 1, 5000, 'staff drink'), 'CHECK');
  rejects(() => payment(t, 1, 0, '  '), 'CHECK');
  payment(t, 1, 0, 'owner authorised — supplier visit');
});

let RECON;

check('reconciliation: one method per settlement, and no overage (§7.5)', () => {
  const recon = (cash, nonCash, writtenOff, shortfall, reason = null, expected = 24200) => run(
    `INSERT INTO reconciliations (waiter_id, cashier_id, expected_minor, cash_minor,
                                  non_cash_minor, written_off_minor, shortfall_minor,
                                  write_off_reason, shift_id, created_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    W1, C1, expected, cash, nonCash, writtenOff, shortfall, reason, S4, T0 + 74 * HOUR
  );

  rejects(() => recon(10000, 14200, 0, 0), 'CHECK');          // split tender
  rejects(() => recon(30000, 0, 0, 0, null, 24200), 'CHECK'); // more than owed
  rejects(() => recon(20000, 0, 0, 0), 'CHECK');              // shortfall does not add up
  rejects(() => recon(0, 0, 4200, 20000, null), 'CHECK');     // write-off with no reason

  // A waiter hands over 200.00 of the 242.00 owed: the rest stays on their
  // running balance. Partial settlement needs no special path.
  recon(20000, 0, 0, 4200);
  RECON = get('SELECT id FROM reconciliations ORDER BY id DESC LIMIT 1').id;
});

check('reconciliation: the allocation must equal the frozen liability (§11.3)', () => {
  const allocate = (tabId, amount) => run(
    `INSERT INTO reconciliation_tabs (reconciliation_id, tab_id, amount_minor)
     VALUES (?, ?, ?)`, RECON, tabId, amount
  );
  // Anything else lets a settlement quietly discount a bill.
  rejects(() => allocate(TAB, 20000), 'frozen liability');
  allocate(TAB, 24200);

  // §11.3: a tab may appear in only ONE reconciliation, ever.
  rejects(() => allocate(TAB, 24200), 'UNIQUE');
});

check('cash: only the cash portion reaches the drawer (§7.6, §7.7)', () => {
  const move = (type, amount, extra = {}) => run(
    `INSERT INTO cash_movements (shift_id, movement_type, amount_minor, category,
                                 reason, reconciliation_id, created_by, created_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    S4, type, amount, extra.category ?? null, extra.reason ?? '',
    extra.recon ?? null, C1, T0 + 74 * HOUR
  );

  move('OPENING_FLOAT', 200000);
  rejects(() => move('OPENING_FLOAT', 100000), 'UNIQUE');        // one float per night
  rejects(() => move('RECONCILIATION', 20000), 'CHECK');         // must name its settlement
  move('RECONCILIATION', 20000, { recon: RECON });
  rejects(() => move('PAYOUT', 5000, { category: 'ice' }), 'CHECK');  // payouts are negative
  rejects(() => move('PAYOUT', -5000), 'CHECK');                 // and always categorised
  move('PAYOUT', -5000, { category: 'ice' });

  const expected = get(
    'SELECT SUM(amount_minor) AS c FROM cash_movements WHERE shift_id = ?', S4
  ).c;
  // 2000.00 float + 200.00 cash settled - 50.00 paid out. The 42.00 shortfall
  // and the mobile-money portion are deliberately absent.
  assert(expected === 215000, `expected cash should be 215000, got ${expected}`);
  rejects(() => run('UPDATE cash_movements SET amount_minor = 0'), 'append-only');
});

check('reconciliation: sealing is the one permitted change (§7.5)', () => {
  run('UPDATE reconciliations SET finalized_at = ? WHERE id = ?', T0 + 75 * HOUR, RECON);

  rejects(
    () => run(`INSERT INTO reconciliation_tabs (reconciliation_id, tab_id, amount_minor)
               VALUES (?, ?, 0)`, RECON, idOf('tabs', 'TAB-000010')),
    'once it is sealed'
  );
  rejects(() => run('UPDATE reconciliations SET cash_minor = 24200 WHERE id = ?', RECON),
          'only permitted change');
  rejects(() => run('UPDATE reconciliations SET finalized_at = ? WHERE id = ?',
                    T0 + 76 * HOUR, RECON), 'only permitted change');
  rejects(() => run('DELETE FROM reconciliations WHERE id = ?', RECON), 'append-only');
});

check('held balance is derived, never stored (§7.4)', () => {
  // liabilities minus what has been settled on FINALIZED rows.
  const held = get(
    `SELECT (SELECT COALESCE(SUM(liability_minor), 0) FROM tab_payments WHERE waiter_id = ?)
          - (SELECT COALESCE(SUM(cash_minor + non_cash_minor + written_off_minor), 0)
               FROM reconciliations WHERE waiter_id = ? AND finalized_at IS NOT NULL)
            AS held`, W1, W1
  ).held;
  // 242.00 owed on the tab, nothing on the comp, 200.00 handed over.
  assert(held === 4200, `held balance should be 4200, got ${held}`);

  // And no column anywhere stores it.
  const stored = db.prepare("PRAGMA table_info('staff')").all().map((c) => c.name);
  assert(!stored.some((c) => c.includes('balance')),
         'a stored balance is a cache that drifts — the same mistake as stored stock');
});

// ---------------------------------------------------------------------------
// §10 — the audit chain, reports, backups
// ---------------------------------------------------------------------------

const GENESIS = '0'.repeat(64);
const fakeHash = (n) => String(n).padStart(64, '0').replace(/^0/, 'a');

check('audit: the chain starts from genesis and extends in order (§10.1)', () => {
  const append = (seq, prev, row) => run(
    `INSERT INTO audit_log (sequence_no, staff_id, action, entity_type, entity_id,
                            new_value, shift_id, created_at, prev_hash, row_hash)
     VALUES (?, ?, 'SHIFT_OPENED', 'shift', ?, '{}', ?, ?, ?, ?)`,
    seq, C1, S4, S4, T0 + 72 * HOUR, prev, row
  );

  rejects(() => append(2, GENESIS, fakeHash(1)), 'in order');       // starts at 1
  rejects(() => append(1, fakeHash(9), fakeHash(1)), 'in order');   // wrong genesis
  append(1, GENESIS, fakeHash(1));
  rejects(() => append(2, GENESIS, fakeHash(2)), 'in order');       // must link to row 1
  append(2, fakeHash(1), fakeHash(2));

  // A hash that is not a hash would make verification meaningless.
  rejects(() => append(3, fakeHash(2), 'not-a-hash'), 'CHECK');
  rejects(() => append(3, fakeHash(2), fakeHash(1).toUpperCase()), 'CHECK');
});

check('audit: an editable audit log audits nothing (§11.1)', () => {
  rejects(() => run("UPDATE audit_log SET action = 'SOMETHING_ELSE' WHERE sequence_no = 1"),
          'append-only');
  rejects(() => run('DELETE FROM audit_log WHERE sequence_no = 1'), 'append-only');
});

check('shift reports: a final report belongs to a closed night (§9.3)', () => {
  const report = (shiftId, provisional, at) => run(
    `INSERT INTO shift_reports (shift_id, is_provisional, report_json, rendered_text,
                                generated_at, generated_by)
     VALUES (?, ?, '{"revenue_minor":24200}', 'SHIFT REPORT\n', ?, ?)`,
    shiftId, provisional, at, C1
  );

  // The X-report is the same document run mid-shift, and several may exist.
  report(S4, 1, T0 + 75 * HOUR);
  report(S4, 1, T0 + 76 * HOUR);
  rejects(() => report(S4, 0, T0 + 76 * HOUR), 'belongs to a closed shift');

  const s3 = idOf('shifts', 'SHIFT-000003');
  report(s3, 0, T0 + 61 * HOUR);
  rejects(() => report(s3, 0, T0 + 62 * HOUR), 'UNIQUE');   // exactly one final

  rejects(() => run("UPDATE shift_reports SET rendered_text = 'edited'"), 'append-only');
  rejects(() => run('DELETE FROM shift_reports'), 'append-only');
});

check('backups: a failed backup is kept, because the pattern is the signal (D17)', () => {
  run(`INSERT INTO backups (shift_id, target_path, size_bytes, outcome, detail, created_at, created_by)
       VALUES (?, '/media/usb/servepoint-2026-08-13.db', 4096, 'VERIFIED', '', ?, ?)`,
      idOf('shifts', 'SHIFT-000003'), T0 + 61 * HOUR, C1);
  run(`INSERT INTO backups (target_path, size_bytes, outcome, detail, created_at)
       VALUES ('/media/usb/servepoint-2026-08-14.db', 4096, 'FAILED', 'target not mounted', ?)`,
      T0 + 76 * HOUR);
  rejects(() => run("DELETE FROM backups WHERE outcome = 'FAILED'"), 'append-only');
});

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

const width = 62;
console.log('\nServePoint — schema check');
console.log('─'.repeat(width));
console.log(`migrations: ${applied.join(', ')}`);
console.log(`sqlite:     ${get('SELECT sqlite_version() AS v').v}`);
console.log('─'.repeat(width));

if (failures.length > 0) {
  for (const f of failures) {
    console.log(`  FAIL  ${f.name}`);
    console.log(`        ${f.message}`);
  }
  console.log('─'.repeat(width));
}
console.log(`${passed} passed, ${failures.length} failed\n`);
process.exit(failures.length === 0 ? 0 : 1);
