-- 0001_core.sql — migration tracking, sequences, settings, staff
--
-- Conventions (10-business-logic-for-port.md §1), applied everywhere below:
--   money       INTEGER, minor units (santim). 12.50 -> 1250. Never float.
--   quantity    INTEGER, thousandths of a base unit. 2.5 shots -> 2500.
--   rates       INTEGER, basis points. 15% -> 1500. 100% -> 10000.
--   timestamps  INTEGER, UTC epoch milliseconds.
--   business    TEXT 'YYYY-MM-DD', derived at write time, never recomputed (§1.3).
--   booleans    INTEGER 0/1 with a CHECK.
--   enums       TEXT with a CHECK — deliberately readable in a DB browser (§1).
--   ids         INTEGER surrogate PK; human-facing codes are separate columns.
--   deletion    does not exist. `active` flags control visibility.

CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    applied_at  INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- Sequences (§1.4)
--
-- A number MUST be allocated inside the same transaction as the row that
-- consumes it. Read-then-write leaves a window where the same number is handed
-- out twice — for receipts that means two customers holding paper with the same
-- number. Increment and read are one statement:
--
--   UPDATE sequences SET next_value = next_value + 1
--    WHERE name = ? RETURNING next_value - 1;
-- ---------------------------------------------------------------------------

CREATE TABLE sequences (
    name        TEXT    PRIMARY KEY
                        CHECK (name IN ('TAB','SHIFT','ISSUE_RECEIPT','CUSTOMER_RECEIPT')),
    next_value  INTEGER NOT NULL CHECK (next_value >= 1)
);

INSERT INTO sequences (name, next_value) VALUES
    ('TAB', 1), ('SHIFT', 1), ('ISSUE_RECEIPT', 1), ('CUSTOMER_RECEIPT', 1);

-- ---------------------------------------------------------------------------
-- Settings (§12)
--
-- Nothing about the business is compiled in. Typed key/value rows, so adding a
-- setting is a data change. Every write is audited old -> new and never
-- restates historical transactions — rates are snapshotted onto each
-- transaction as applied.
-- ---------------------------------------------------------------------------

CREATE TABLE settings (
    key         TEXT    PRIMARY KEY,
    value       TEXT    NOT NULL,
    value_type  TEXT    NOT NULL
                        CHECK (value_type IN ('STRING','INTEGER','BOOLEAN','RATE','TIME')),
    updated_at  INTEGER NOT NULL,
    updated_by  INTEGER          -- staff.id; NULL for the initial seed
);

-- A settings key is never deleted; it is changed. Deleting one would make the
-- audit trail of old -> new meaningless.
CREATE TRIGGER settings_no_delete BEFORE DELETE ON settings
BEGIN SELECT RAISE(ABORT, 'settings rows are never deleted'); END;

-- Defaults (§12), with the departures recorded in 11-port-decisions.md:
--
--   D8   Receipt identity ships blank, not Tonic-specific. The first-run wizard
--        fills these. A club that never sets them prints a headerless receipt,
--        which is visibly wrong — better than silently printing another club's
--        name and TIN onto a fiscal document.
--   D9   inventory.allow_negative is GONE and has no replacement. Insufficient
--        stock always blocks the sale, and there is deliberately no key here to
--        turn that off. The relief valve is a single-product stock correction,
--        which moves the count rather than waiving the check.
--   D23  printing.report_enabled defaults OFF. The report is always generated
--        and stored at close; it is read on screen. Bar issue slips are not
--        reporting and always print — that is how drinks get released.
--   D24  Bank accounts and the EthSwitch QR print at the foot of the customer
--        receipt when configured, and are omitted entirely when not.
--   D25  Customer TIN is optional per tab; prompting for it is a setting, and
--        it is irrelevant when tax is off.
--
-- D15: tabs.age_warning_days, payments.partial_enabled, reporting.show_cost and
-- locale.rounding are read by nothing (§14). The rows stay — the intent is
-- worth keeping visible — but they get no UI toggle until the enforcement
-- behind them exists. An inert safety switch reads as working configuration.

INSERT INTO settings (key, value, value_type, updated_at, updated_by) VALUES
    ('tax.enabled',                       '0',     'BOOLEAN', 0, NULL),
    ('tax.rate_bp',                       '1500',  'RATE',    0, NULL),
    ('tax.inclusive',                     '1',     'BOOLEAN', 0, NULL),
    ('service_charge.enabled',            '1',     'BOOLEAN', 0, NULL),
    ('service_charge.rate_bp',            '1000',  'RATE',    0, NULL),
    ('shift.day_start',                   '18:00', 'TIME',    0, NULL),
    ('shift.day_end',                     '06:00', 'TIME',    0, NULL),
    ('tabs.reference_mode',               'TABLE', 'STRING',  0, NULL),
    ('tabs.age_warning_days',             '3',     'INTEGER', 0, NULL),
    ('tabs.ask_customer_tin',             '0',     'BOOLEAN', 0, NULL),
    ('payments.comps_enabled',            '0',     'BOOLEAN', 0, NULL),
    ('payments.partial_enabled',          '0',     'BOOLEAN', 0, NULL),
    ('payments.bank_accounts',            '',      'STRING',  0, NULL),
    ('payments.qr_enabled',               '0',     'BOOLEAN', 0, NULL),
    ('printing.report_enabled',           '0',     'BOOLEAN', 0, NULL),
    ('printing.customer_receipt_enabled', '1',     'BOOLEAN', 0, NULL),
    ('reporting.show_cost',               '0',     'BOOLEAN', 0, NULL),
    ('receipt.business_name',             '',      'STRING',  0, NULL),
    ('receipt.address',                   '',      'STRING',  0, NULL),
    ('receipt.phone',                     '',      'STRING',  0, NULL),
    ('receipt.tin',                       '',      'STRING',  0, NULL),
    ('receipt.footer',                    '',      'STRING',  0, NULL),
    ('receipt.chars_per_line',            '48',    'INTEGER', 0, NULL),
    ('locale.currency_code',              '',      'STRING',  0, NULL),
    ('locale.rounding',                   'NONE',  'STRING',  0, NULL),
    ('setup.completed',                   '0',     'BOOLEAN', 0, NULL);

-- Shape and domain checks on settings values. A value_type of STRING cannot
-- express "one of these four words", and a typo in tabs.reference_mode changes
-- behaviour silently. BOOLEAN is stored as the text '0'/'1'; RATE and INTEGER
-- as decimal digits; TIME as 'HH:mm'.
--
-- Note there is no inventory.stock_policy case here, and must not be: revised
-- D9 removed the key entirely. Insufficient stock always blocks the sale.
CREATE TRIGGER settings_value_valid BEFORE UPDATE OF value, value_type ON settings
WHEN NOT (
     CASE NEW.value_type
       WHEN 'BOOLEAN' THEN NEW.value IN ('0','1')
       WHEN 'TIME'    THEN NEW.value GLOB '[0-2][0-9]:[0-5][0-9]'
       WHEN 'RATE'    THEN NEW.value GLOB '[0-9]*' AND NEW.value NOT GLOB '*[^0-9]*'
       WHEN 'INTEGER' THEN NEW.value GLOB '[0-9]*' AND NEW.value NOT GLOB '*[^0-9]*'
       ELSE 1
     END
   AND CASE NEW.key
       WHEN 'tabs.reference_mode'
            THEN NEW.value IN ('TABLE','CUSTOMER_NAME','CUSTOMER_PHONE','CUSTOM')
       ELSE 1
     END
)
BEGIN SELECT RAISE(ABORT, 'settings: value invalid for this key or type'); END;

-- ---------------------------------------------------------------------------
-- Staff (§0.3, revised by D22)
--
-- Two roles authenticate, and only two:
--
--   CASHIER  operates the till — tabs, orders, end of day. Cannot open Settings.
--   OWNER    reads Overview, Reports and stored shift reports, and is the ONLY
--            role that may change settings. Does not operate the till.
--
-- §0.3 said the cashier was the only system user and the owner never logged in.
-- D22 supersedes that. It is NOT the manager permission layer §0.3 rejects:
-- nothing about a void, correction or write-off now requires a second person.
-- It gates reading and configuration, not operations.
--
-- The waiter remains a master record rather than a user — every tab belongs to
-- exactly one, but a waiter never logs in. BARTENDER is permitted by the CHECK
-- so that modelling on-duty bartenders later is a data change rather than a
-- schema change (§14), even though nothing writes it today.
-- ---------------------------------------------------------------------------

CREATE TABLE staff (
    id          INTEGER PRIMARY KEY,
    code        TEXT    NOT NULL UNIQUE,
    full_name   TEXT    NOT NULL CHECK (TRIM(full_name) <> ''),
    role        TEXT    NOT NULL CHECK (role IN ('OWNER','CASHIER','WAITER','BARTENDER')),
    active      INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
    pin_hash    TEXT,           -- OWNER and CASHIER only; nobody else authenticates
    pin_salt    TEXT,
    created_at  INTEGER NOT NULL
);

-- An owner or cashier without a PIN could not log in; a waiter with one implies
-- a login that does not exist. Catching this at the database keeps the role
-- model from drifting as the UI grows.
CREATE TRIGGER staff_pin_matches_role_ins BEFORE INSERT ON staff
WHEN (    NEW.role IN ('OWNER','CASHIER')
      AND (NEW.pin_hash IS NULL OR NEW.pin_salt IS NULL))
  OR (    NEW.role NOT IN ('OWNER','CASHIER')
      AND (NEW.pin_hash IS NOT NULL OR NEW.pin_salt IS NOT NULL))
BEGIN SELECT RAISE(ABORT, 'staff: owners and cashiers carry a PIN, nobody else does'); END;

-- The last active owner may not be deactivated. D22 puts settings behind the
-- owner role, so removing every owner would lock the venue out of its own tax
-- rate with no recovery path on an offline machine.
CREATE TRIGGER staff_keep_one_owner BEFORE UPDATE OF active ON staff
WHEN OLD.role = 'OWNER' AND OLD.active = 1 AND NEW.active = 0
 AND (SELECT COUNT(*) FROM staff WHERE role = 'OWNER' AND active = 1) <= 1
BEGIN SELECT RAISE(ABORT, 'staff: the last active owner cannot be deactivated'); END;

CREATE TRIGGER staff_no_delete BEFORE DELETE ON staff
BEGIN SELECT RAISE(ABORT, 'staff are deactivated, never deleted'); END;

CREATE INDEX staff_active_role ON staff(role, active);
