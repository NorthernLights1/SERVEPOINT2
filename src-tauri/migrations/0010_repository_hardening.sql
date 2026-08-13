-- 0010_repository_hardening.sql — freeze historical facts exposed by the
-- repository integration, add the D25 receipt snapshot, and make a failed
-- first customer-receipt print retryable under the same fiscal number.
--
-- This is deliberately forward-only and additive for installed v9 databases:
-- one nullable column is added without a backfill, and the remainder is
-- trigger metadata. Existing rows are neither rewritten nor reinterpreted.

-- ---------------------------------------------------------------------------
-- Catalogue history (§2.3, §2.4)
-- ---------------------------------------------------------------------------

CREATE TRIGGER recipes_facts_frozen BEFORE UPDATE ON recipes
WHEN NEW.sale_item_id   <> OLD.sale_item_id
  OR NEW.version        <> OLD.version
  OR NEW.effective_from <> OLD.effective_from
  OR NEW.created_by     IS NOT OLD.created_by
BEGIN SELECT RAISE(ABORT, 'recipe version facts are immutable'); END;

-- A line is assembled only by INSERT while a new version is unreferenced.
-- Once written it is never edited or removed, even while that version remains
-- current. Changing a recipe means closing it and inserting the next version.
DROP TRIGGER recipe_lines_frozen_upd;
CREATE TRIGGER recipe_lines_frozen_upd BEFORE UPDATE ON recipe_lines
BEGIN SELECT RAISE(ABORT, 'recipe lines are immutable'); END;

DROP TRIGGER recipe_lines_frozen_del;
CREATE TRIGGER recipe_lines_frozen_del BEFORE DELETE ON recipe_lines
BEGIN SELECT RAISE(ABORT, 'recipe lines are immutable'); END;

-- The schema has no recipe DRAFT state, so initial multi-line assembly remains
-- possible inside the creating transaction. The first order-line snapshot is
-- the irreversible boundary: no further line may be appended afterwards.
CREATE TRIGGER recipe_lines_referenced_frozen BEFORE INSERT ON recipe_lines
WHEN EXISTS (SELECT 1 FROM order_lines WHERE recipe_id = NEW.recipe_id)
BEGIN SELECT RAISE(ABORT, 'cannot append to a referenced recipe version'); END;

CREATE TRIGGER prices_facts_frozen BEFORE UPDATE ON prices
WHEN NEW.sale_item_id   <> OLD.sale_item_id
  OR NEW.price_minor    <> OLD.price_minor
  OR NEW.effective_from <> OLD.effective_from
  OR NEW.created_by     IS NOT OLD.created_by
BEGIN SELECT RAISE(ABORT, 'price facts are immutable'); END;

-- ---------------------------------------------------------------------------
-- Shift, tab, and order identity (§4–§6, §11.2)
-- ---------------------------------------------------------------------------

CREATE TRIGGER shifts_expected_end_frozen BEFORE UPDATE OF expected_end_at ON shifts
WHEN NEW.expected_end_at <> OLD.expected_end_at
BEGIN SELECT RAISE(ABORT, 'shifts: expected end is immutable'); END;

CREATE TRIGGER tabs_reference_fields_frozen BEFORE UPDATE ON tabs
WHEN NEW.table_no       IS NOT OLD.table_no
  OR NEW.customer_name  IS NOT OLD.customer_name
  OR NEW.customer_phone IS NOT OLD.customer_phone
  OR NEW.custom_ref     IS NOT OLD.custom_ref
BEGIN SELECT RAISE(ABORT, 'tabs: raw reference fields are immutable'); END;

CREATE TRIGGER tabs_opening_actor_frozen BEFORE UPDATE OF opened_by ON tabs
WHEN NEW.opened_by <> OLD.opened_by
BEGIN SELECT RAISE(ABORT, 'tabs: opening actor is immutable'); END;

-- D25 permits the optional TIN to be captured at the cashier's discretion,
-- including as part of the OPEN -> CLOSED write. It freezes once the bill has
-- closed, rather than at tab open like the customer reference.
CREATE TRIGGER tabs_closed_tin_frozen BEFORE UPDATE OF customer_tin ON tabs
WHEN OLD.status IN ('CLOSED','RECONCILED')
 AND NEW.customer_tin IS NOT OLD.customer_tin
BEGIN SELECT RAISE(ABORT, 'tabs: a closed customer TIN is immutable'); END;

-- Search is case-insensitive, so identity must be too. Triggers avoid making
-- an upgrade fail if a legacy v9 database already contains a case-only pair;
-- they prevent any new ambiguity while leaving existing data recoverable.
CREATE TRIGGER tabs_open_label_nocase_ins BEFORE INSERT ON tabs
WHEN NEW.status = 'OPEN'
 AND EXISTS (SELECT 1 FROM tabs
              WHERE status = 'OPEN'
                AND display_label = NEW.display_label COLLATE NOCASE
                AND display_label <> NEW.display_label)
BEGIN SELECT RAISE(ABORT, 'tabs: case-insensitive reference already belongs to an open tab'); END;

CREATE TRIGGER orders_chain_root_frozen BEFORE UPDATE OF root_order_id ON orders
WHEN NEW.root_order_id IS NOT OLD.root_order_id
BEGIN SELECT RAISE(ABORT, 'orders: order chain root is immutable'); END;

CREATE TRIGGER orders_issued_at_once BEFORE UPDATE OF issued_at ON orders
WHEN NEW.issued_at IS NOT OLD.issued_at
 AND (   OLD.status = NEW.status
      OR (OLD.status = 'DRAFT'    AND NEW.status IN ('PRINTING','ABANDONED'))
      OR (OLD.status = 'PRINTING' AND NEW.status = 'DRAFT')
      OR (OLD.status = 'ISSUED'   AND NEW.status IN ('REPLACED','VOIDED')))
BEGIN SELECT RAISE(ABORT, 'orders: issued time is immutable once stamped'); END;

CREATE TRIGGER orders_void_facts_once BEFORE UPDATE ON orders
WHEN (NEW.void_reason IS NOT OLD.void_reason
   OR NEW.voided_at   IS NOT OLD.voided_at
   OR NEW.voided_by   IS NOT OLD.voided_by)
 AND NOT (OLD.status = 'ISSUED' AND NEW.status = 'VOIDED'
          AND OLD.void_reason IS NULL AND OLD.voided_at IS NULL AND OLD.voided_by IS NULL)
BEGIN SELECT RAISE(ABORT, 'orders: void facts are immutable once stamped'); END;

-- ---------------------------------------------------------------------------
-- Customer receipt snapshots and retry (§6.9, D10, D25)
-- ---------------------------------------------------------------------------

ALTER TABLE receipts ADD COLUMN customer_tin TEXT
    CHECK (receipt_type = 'CUSTOMER' OR customer_tin IS NULL);

CREATE TRIGGER receipts_customer_tin_frozen BEFORE UPDATE OF customer_tin ON receipts
WHEN NEW.customer_tin IS NOT OLD.customer_tin
BEGIN SELECT RAISE(ABORT, 'receipts: customer TIN is immutable'); END;

-- Attempt 2 on a customer document can mean either a genuine reprint of paper
-- that succeeded, or a retry of the FIRST print after a resolved failure. The
-- latter keeps the existing CR number: allocating another fiscal identity
-- would produce a gap and two documents for one frozen bill.
DROP TRIGGER receipt_prints_reprint_rules;
CREATE TRIGGER receipt_prints_reprint_rules BEFORE INSERT ON receipt_prints
WHEN NEW.print_no > 1
 AND (
      TRIM(NEW.reason) = ''
      OR (
           (SELECT receipt_type FROM receipts WHERE id = NEW.receipt_id) = 'ISSUE'
           AND (SELECT status FROM receipts WHERE id = NEW.receipt_id)
                 NOT IN ('PRINTED','FAILED')
         )
      OR (
           (SELECT receipt_type FROM receipts WHERE id = NEW.receipt_id) = 'CUSTOMER'
           AND NOT (
                (SELECT status FROM receipts WHERE id = NEW.receipt_id) = 'PRINTED'
                OR (
                     (SELECT status FROM receipts WHERE id = NEW.receipt_id)
                         IN ('PENDING','FAILED')
                     AND (SELECT outcome FROM receipt_prints
                           WHERE receipt_id = NEW.receipt_id
                           ORDER BY print_no DESC LIMIT 1) = 'FAILED'
                   )
             )
         )
     )
BEGIN SELECT RAISE(ABORT, 'reprints need a reason and a printed or failed receipt'); END;
