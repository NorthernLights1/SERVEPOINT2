-- 0011_backup_reconciliation_hardening.sql
-- Narrow integrity fixes found while wiring Phase-1 services.

CREATE TABLE backups_new (
    id           INTEGER PRIMARY KEY,
    shift_id     INTEGER REFERENCES shifts(id),
    target_path  TEXT    NOT NULL CHECK (TRIM(target_path) <> ''),
    size_bytes   INTEGER NOT NULL CHECK (size_bytes >= 0),
    outcome      TEXT    NOT NULL CHECK (outcome IN ('VERIFIED','FAILED')),
    detail       TEXT    NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    created_by   INTEGER REFERENCES staff(id),
    CHECK (outcome = 'FAILED' OR size_bytes > 0)
);

INSERT INTO backups_new
    (id, shift_id, target_path, size_bytes, outcome, detail, created_at, created_by)
SELECT id, shift_id, target_path, size_bytes, outcome, detail, created_at, created_by
  FROM backups;

DROP TRIGGER backups_no_update;
DROP TRIGGER backups_no_delete;
DROP INDEX backups_by_time;
DROP TABLE backups;
ALTER TABLE backups_new RENAME TO backups;

CREATE INDEX backups_by_time ON backups(created_at);
CREATE TRIGGER backups_no_update BEFORE UPDATE ON backups
BEGIN SELECT RAISE(ABORT, 'backups is append-only'); END;
CREATE TRIGGER backups_no_delete BEFORE DELETE ON backups
BEGIN SELECT RAISE(ABORT, 'backups is append-only'); END;

CREATE UNIQUE INDEX cash_movements_one_per_reconciliation
    ON cash_movements(reconciliation_id) WHERE reconciliation_id IS NOT NULL;

CREATE TRIGGER cash_movements_reconciliation_exact
BEFORE INSERT ON cash_movements
WHEN NEW.movement_type = 'RECONCILIATION'
 AND NOT EXISTS (
       SELECT 1 FROM reconciliations r
        WHERE r.id = NEW.reconciliation_id
          AND r.shift_id = NEW.shift_id
          AND r.cashier_id = NEW.created_by
          AND r.cash_minor = NEW.amount_minor
          AND r.cash_minor > 0
          -- Finalization and drawer entry use one timestamp in one caller-owned
          -- transaction. Requiring the sealed row first means a committed cash
          -- movement can never point at a draft settlement.
          AND r.finalized_at = NEW.created_at)
BEGIN SELECT RAISE(ABORT, 'cash: reconciliation movement must match the finalized settlement and time'); END;
