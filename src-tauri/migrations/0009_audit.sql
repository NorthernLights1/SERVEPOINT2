-- 0009_audit.sql — the hash-chained audit log and the stored shift report
--
-- §10.1 is refreshingly honest, and the honesty is the design:
--
--   TAMPER-EVIDENCE, NOT TAMPER-PROOFING.
--
-- The database file sits on a machine the owner controls, and anyone with a
-- SQLite browser can rewrite it. Nothing here prevents that and nothing here
-- pretends to. What it buys is that an edit CANNOT BE HIDDEN: each row hashes
-- the one before it, so changing any row breaks every hash after it, and the
-- integrity report names the first broken row.
--
-- Triggers stop the APPLICATION from rewriting history. The chain catches
-- everybody else, after the fact. Those are different jobs and both are worth
-- having.

CREATE TABLE audit_log (
    id           INTEGER PRIMARY KEY,

    -- Dense and gapless. Verification walks rows in this order and fails on a
    -- gap, so a deleted row is as visible as an edited one.
    sequence_no  INTEGER NOT NULL UNIQUE CHECK (sequence_no >= 1),

    staff_id     INTEGER REFERENCES staff(id),     -- NULL for system actions
    action       TEXT    NOT NULL CHECK (TRIM(action) <> ''),
    entity_type  TEXT    NOT NULL CHECK (TRIM(entity_type) <> ''),
    entity_id    INTEGER,

    old_value    TEXT,
    new_value    TEXT,

    shift_id     INTEGER REFERENCES shifts(id),    -- NULL: the club was shut
    created_at   INTEGER NOT NULL,

    -- §10.1. Lowercase hex SHA-256; the genesis row carries 64 zeroes.
    --
    -- PORT NOTE, and this is a real defect being fixed rather than a style
    -- preference: the Java joined these fields with an EMPTY separator, so
    -- "ab" + "c" and "a" + "bc" hashed identically and adjacent fields could
    -- be shuffled undetectably. The Rust side uses a real separator and
    -- length-prefixes each field, with an explicit encoding for NULL.
    prev_hash    TEXT    NOT NULL CHECK (LENGTH(prev_hash) = 64
                                         AND prev_hash NOT GLOB '*[^0-9a-f]*'),
    row_hash     TEXT    NOT NULL UNIQUE CHECK (LENGTH(row_hash) = 64
                                         AND row_hash NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX audit_log_by_entity ON audit_log(entity_type, entity_id);
CREATE INDEX audit_log_by_shift  ON audit_log(shift_id, created_at);
CREATE INDEX audit_log_by_action ON audit_log(action, created_at);

-- The chain must be built forwards, one link at a time. Inserting out of order
-- or skipping a number would produce a log that fails its own verification the
-- moment anybody ran it — which looks exactly like tampering.
CREATE TRIGGER audit_log_chain_intact BEFORE INSERT ON audit_log
WHEN NEW.sequence_no <> 1 + COALESCE((SELECT MAX(sequence_no) FROM audit_log), 0)
  OR NEW.prev_hash <> COALESCE(
       (SELECT row_hash FROM audit_log ORDER BY sequence_no DESC LIMIT 1),
       '0000000000000000000000000000000000000000000000000000000000000000')
BEGIN SELECT RAISE(ABORT, 'audit: the chain must be extended in order, from the previous hash'); END;

-- §11.1. An audit row that can be edited audits nothing.
CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;

CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;

-- ---------------------------------------------------------------------------
-- Shift reports (§9.3) — append-only
--
-- §4.3: CLOSING THE SHIFT AND STORING ITS REPORT ARE ONE ALL-OR-NOTHING
-- COMMIT. If rendering or storage fails, the shift stays CLOSING. It must
-- never be possible to commit a closed night with its sole fraud-control
-- document missing.
--
-- Stored twice on purpose: report_json so the numbers stay queryable, and the
-- exact rendered text so a reprint months later reproduces what was signed —
-- even if corrections happened since. D11: past shifts are READ from here and
-- never recomputed, so the report and the paper can never drift apart.
-- ---------------------------------------------------------------------------

CREATE TABLE shift_reports (
    id             INTEGER PRIMARY KEY,
    shift_id       INTEGER NOT NULL REFERENCES shifts(id),

    -- §9.3: the X-report is this same document run mid-shift without closing,
    -- clearly marked provisional. Several may exist for one night; exactly one
    -- final report may.
    is_provisional INTEGER NOT NULL DEFAULT 0 CHECK (is_provisional IN (0,1)),

    report_json    TEXT    NOT NULL CHECK (TRIM(report_json) <> ''),
    rendered_text  TEXT    NOT NULL CHECK (TRIM(rendered_text) <> ''),

    generated_at   INTEGER NOT NULL,
    generated_by   INTEGER NOT NULL REFERENCES staff(id)
);

CREATE UNIQUE INDEX shift_reports_one_final
    ON shift_reports(shift_id) WHERE is_provisional = 0;

CREATE INDEX shift_reports_by_shift ON shift_reports(shift_id, generated_at);

-- A final report describes a closed night. Generating one for a shift still
-- trading would produce a document that is out of date before it is signed.
CREATE TRIGGER shift_reports_final_needs_closed_shift BEFORE INSERT ON shift_reports
WHEN NEW.is_provisional = 0
 AND (SELECT status FROM shifts WHERE id = NEW.shift_id) <> 'CLOSED'
BEGIN SELECT RAISE(ABORT, 'shift reports: a final report belongs to a closed shift'); END;

CREATE TRIGGER shift_reports_no_update BEFORE UPDATE ON shift_reports
BEGIN SELECT RAISE(ABORT, 'shift_reports is append-only — reprint it, never rebuild it'); END;

CREATE TRIGGER shift_reports_no_delete BEFORE DELETE ON shift_reports
BEGIN SELECT RAISE(ABORT, 'shift_reports is append-only — reprint it, never rebuild it'); END;

-- ---------------------------------------------------------------------------
-- Backups (§9.3, §13.4)
--
-- The specification says report generation triggers an automatic backup, and
-- also that backup is not yet built. D17 resolves the contradiction by
-- building it now: VACUUM INTO an external target, verified by reopening the
-- copy and reading it back, with the result recorded here. An unverified
-- backup is a belief, not a backup.
-- ---------------------------------------------------------------------------

CREATE TABLE backups (
    id           INTEGER PRIMARY KEY,
    shift_id     INTEGER REFERENCES shifts(id),
    target_path  TEXT    NOT NULL CHECK (TRIM(target_path) <> ''),
    size_bytes   INTEGER NOT NULL CHECK (size_bytes > 0),

    -- VERIFIED means the copy was reopened and read back. FAILED rows are kept
    -- deliberately: a run of failures is the signal that the backup target has
    -- gone away, and deleting them would hide exactly that.
    outcome      TEXT    NOT NULL CHECK (outcome IN ('VERIFIED','FAILED')),
    detail       TEXT    NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    created_by   INTEGER REFERENCES staff(id)
);

CREATE INDEX backups_by_time ON backups(created_at);

CREATE TRIGGER backups_no_update BEFORE UPDATE ON backups
BEGIN SELECT RAISE(ABORT, 'backups is append-only'); END;

CREATE TRIGGER backups_no_delete BEFORE DELETE ON backups
BEGIN SELECT RAISE(ABORT, 'backups is append-only'); END;
