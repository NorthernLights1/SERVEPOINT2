import { useEffect, useRef, useState, type FormEvent } from "react";

import type {
  CorrectableOrderView,
  CorrectionLineInput,
  CorrectionPreview,
  CorrectionProductChange,
  FloorView,
  MenuLine,
  ReturnInput,
} from "../api";
import { api, reason, ServePointError } from "../api";
import { Banner, Card, Chip, Field } from "../ui";

/** Quantities cross to Rust as thousandths. This is a unit, not a calculation. */
const MILLI = 1000;

type Mode = "correct" | "void";
type ReturnedDraft = { quantity: string; note: string };

function toMilli(quantity: string) {
  return Math.round(Number(quantity) * MILLI);
}

function blankReturns(changes: CorrectionProductChange[]): Record<number, ReturnedDraft> {
  return Object.fromEntries(
    changes
      .filter((change) => change.maximumReturnMilli > 0)
      .map((change) => [change.productId, { quantity: "", note: "" }]),
  );
}

export function OrderCorrection(props: {
  tabId: number;
  menu: MenuLine[];
  onChanged: (view: FloorView) => void;
  onPrintPending: () => void;
}) {
  return <OrderCorrectionForTab key={props.tabId} {...props} />;
}

function OrderCorrectionForTab({
  tabId,
  menu,
  onChanged,
  onPrintPending,
}: {
  tabId: number;
  menu: MenuLine[];
  onChanged: (view: FloorView) => void;
  onPrintPending: () => void;
}) {
  const [receiptNumber, setReceiptNumber] = useState("");
  const [order, setOrder] = useState<CorrectableOrderView>();
  const [mode, setMode] = useState<Mode>();
  const [quantities, setQuantities] = useState<Record<number, string>>({});
  const [added, setAdded] = useState<Record<number, string>>({});
  const [preview, setPreview] = useState<CorrectionPreview>();
  const [returned, setReturned] = useState<Record<number, ReturnedDraft>>({});
  const [reasonText, setReasonText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [status, setStatus] = useState("");
  const receiptInput = useRef<HTMLInputElement>(null);
  const restoreReceiptFocus = useRef(false);

  useEffect(() => {
    if (!order && restoreReceiptFocus.current) {
      restoreReceiptFocus.current = false;
      receiptInput.current?.focus();
    }
  }, [order]);

  const available = order
    ? menu.filter(
        (item) => !order.lines.some((line) => line.saleItemId === item.saleItemId),
      )
    : [];
  const correctionLines: CorrectionLineInput[] = order
    ? [
        ...order.lines.flatMap((line) => {
          const quantityMilli = toMilli(quantities[line.lineId] ?? "");
          return quantityMilli > 0
            ? [
                {
                  originalLineId: line.lineId,
                  saleItemId: line.saleItemId,
                  quantityMilli,
                },
              ]
            : [];
        }),
        ...available.flatMap((item) => {
          const quantityMilli = toMilli(added[item.saleItemId] ?? "");
          return quantityMilli > 0
            ? [{ originalLineId: null, saleItemId: item.saleItemId, quantityMilli }]
            : [];
        }),
      ]
    : [];
  const hasCorrectionChange =
    !!order &&
    (order.lines.some(
      (line) => toMilli(quantities[line.lineId] ?? "") !== line.quantityMilli,
    ) || available.some((item) => toMilli(added[item.saleItemId] ?? "") > 0));
  const changes = mode === "void" ? order?.voidChanges : preview?.productChanges;
  const reducedChanges = changes?.filter((change) => change.maximumReturnMilli > 0) ?? [];

  function clearOrder() {
    restoreReceiptFocus.current = true;
    setOrder(undefined);
    setMode(undefined);
    setQuantities({});
    setAdded({});
    setPreview(undefined);
    setReturned({});
    setReasonText("");
  }

  function invalidatePreview() {
    setPreview(undefined);
    setReturned({});
    setError(undefined);
    setStatus("");
  }

  async function lookup(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setError(undefined);
    setStatus("Looking up the typed BR number.");
    try {
      const typedNumber = receiptNumber.trim();
      const found = await api.correctionOrder(tabId, typedNumber);
      setReceiptNumber(typedNumber);
      setOrder(found);
      setMode(undefined);
      setQuantities(
        Object.fromEntries(found.lines.map((line) => [line.lineId, line.quantity])),
      );
      setAdded({});
      setPreview(undefined);
      setReturned({});
      setReasonText("");
      setStatus(`Order found. Its original total is ${found.originalTotal}. Choose an action.`);
    } catch (raw) {
      setStatus("");
      setError(reason(raw));
    } finally {
      setBusy(false);
    }
  }

  async function requestPreview(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!order) return;
    setBusy(true);
    setError(undefined);
    setStatus("Working out the correction preview.");
    try {
      const next = await api.previewCorrection(order.orderId, correctionLines);
      setPreview(next);
      setReturned(blankReturns(next.productChanges));
      setStatus("Preview ready. Record what came back and why the order changed.");
    } catch (raw) {
      setStatus("");
      setError(reason(raw));
    } finally {
      setBusy(false);
    }
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!order || !mode || !changes) return;
    const returns: ReturnInput[] = reducedChanges.map((change) => ({
      productId: change.productId,
      returnedMilli: toMilli(returned[change.productId]?.quantity ?? "0"),
      note: returned[change.productId]?.note.trim() ?? "",
    }));

    setBusy(true);
    setError(undefined);
    setStatus(mode === "void" ? "Voiding the order." : "Recording the correction.");
    try {
      if (mode === "void") {
        const view = await api.voidOrder(
          order.orderId,
          receiptNumber,
          reasonText.trim(),
          returns,
        );
        onChanged(view);
        clearOrder();
        setReceiptNumber("");
        setStatus("Order voided.");
        return;
      }
      const result = await api.correctOrder(
        order.orderId,
        receiptNumber,
        reasonText.trim(),
        correctionLines,
        returns,
      );
      onChanged(result.view);
      const printed = result.slips.map((slip) => slip.receiptNumber).join(", ");
      clearOrder();
      setReceiptNumber("");
      setStatus(
        printed
          ? `Correction recorded and replacement slip ${printed} printed.`
          : "Correction recorded.",
      );
    } catch (raw) {
      if (raw instanceof ServePointError && raw.kind === "PRINT_PENDING") {
        clearOrder();
        setReceiptNumber("");
        onPrintPending();
      }
      setStatus("");
      setError(reason(raw));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card
      title="Correct or void an order"
      blurb="The bartender returns the issue slip. Type its BR number here; the till never lists or fills one in."
      aside={order ? <Chip tone="quiet">{order.originalTotal}</Chip> : undefined}
    >
      {error && (
        <div role="alert">
          <Banner tone="bad" title="That was refused">
            {error}
          </Banner>
        </div>
      )}

      {status && (
        <p className="muted" role="status" aria-live="polite">
          {status}
        </p>
      )}

      <form className="grid" onSubmit={lookup} aria-busy={busy}>
        <div className="field">
          <label className="field__label" htmlFor={`correction-receipt-${tabId}`}>
            BR number
          </label>
          <input
            ref={receiptInput}
            id={`correction-receipt-${tabId}`}
            className="input"
            value={receiptNumber}
            required
            autoComplete="off"
            spellCheck={false}
            disabled={busy || !!order}
            aria-describedby={`correction-receipt-help-${tabId}`}
            onChange={(event) => setReceiptNumber(event.target.value)}
          />
          <span id={`correction-receipt-help-${tabId}`} className="field__help">
            Type the number from the returned paper slip. It is not available as a list.
          </span>
        </div>
        {order ? (
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => {
              clearOrder();
              setReceiptNumber("");
              setError(undefined);
              setStatus("");
            }}
          >
            Use another slip
          </button>
        ) : (
          <button
            type="submit"
            className="btn"
            disabled={busy || receiptNumber.trim() === ""}
          >
            Find order
          </button>
        )}
      </form>

      {order && (
        <>
          <ul className="rows">
            {order.lines.map((line) => (
              <li key={line.lineId} className="row row--static">
                <span className="row__main">
                  <strong>{line.name}</strong>
                  <span className="muted">
                    {line.quantity} at {line.unitPrice}
                  </span>
                </span>
                <span className="row__value">{line.lineTotal}</span>
              </li>
            ))}
          </ul>

          <Field label="Choose what to do">
            <select
              className="select"
              value={mode ?? ""}
              disabled={busy}
              onChange={(event) => {
                const next = event.target.value as Mode | "";
                setMode(next || undefined);
                setPreview(undefined);
                setReturned(next === "void" ? blankReturns(order.voidChanges) : {});
                setReasonText("");
                setError(undefined);
                setStatus(
                  next === "correct"
                    ? "Edit the quantities, then request a preview."
                    : next === "void"
                      ? "Record what came back and why the whole order is being voided."
                      : "",
                );
              }}
            >
              <option value="">Choose correction or void</option>
              <option value="correct">Correct the order</option>
              <option value="void">Void the whole order</option>
            </select>
          </Field>
        </>
      )}

      {order && mode === "correct" && (
        <form className="grid" onSubmit={requestPreview} aria-busy={busy}>
          <h3>Correct the original lines</h3>
          <ul className="rows">
            {order.lines.map((line, index) => (
              <li key={line.lineId} className="row row--static">
                <span className="row__main">
                  <strong>{line.name}</strong>
                  <span className="muted">
                    Originally {line.quantity} · {line.unitPrice} each
                  </span>
                </span>
                <Field
                  label={`Line ${index + 1}: new quantity for ${line.name}, originally ${line.quantity} at ${line.unitPrice}`}
                >
                  <input
                    className="input input--num input--short"
                    type="number"
                    inputMode="decimal"
                    min="0"
                    step="0.001"
                    required
                    disabled={busy}
                    value={quantities[line.lineId] ?? ""}
                    onChange={(event) => {
                      setQuantities((current) => ({
                        ...current,
                        [line.lineId]: event.target.value,
                      }));
                      invalidatePreview();
                    }}
                  />
                </Field>
              </li>
            ))}
          </ul>

          {available.length > 0 && (
            <>
              <h3>Add a current menu item</h3>
              <ul className="rows">
                {available.map((item, index) => (
                  <li key={item.saleItemId} className="row row--static">
                    <span className="row__main">
                      <strong>{item.name}</strong>
                      <span className="muted">
                        {item.category} · {item.price}
                      </span>
                    </span>
                    <Field
                      label={`Menu item ${index + 1}: quantity of ${item.name}, ${item.category} at ${item.price}, to add`}
                    >
                      <input
                        className="input input--num input--short"
                        type="number"
                        inputMode="decimal"
                        min="0"
                        step="0.001"
                        disabled={busy}
                        value={added[item.saleItemId] ?? ""}
                        onChange={(event) => {
                          setAdded((current) => ({
                            ...current,
                            [item.saleItemId]: event.target.value,
                          }));
                          invalidatePreview();
                        }}
                      />
                    </Field>
                  </li>
                ))}
              </ul>
            </>
          )}

          <p className="field__help">
            Keep at least one item on a correction. To remove the whole order, choose Void.
          </p>
          <button
            type="submit"
            className="btn"
            disabled={busy || !hasCorrectionChange || correctionLines.length === 0}
          >
            Preview correction
          </button>
        </form>
      )}

      {order && mode === "void" && (
        <Banner tone="warn" title="The whole order will be removed">
          This cannot be undone. Record what physically came back before permanently voiding it.
        </Banner>
      )}

      {preview && mode === "correct" && (
        <dl className="totals">
          <dt>Original total</dt>
          <dd>{preview.originalTotal}</dd>
          <dt>Corrected total</dt>
          <dd>{preview.replacementTotal}</dd>
        </dl>
      )}

      {changes && (
        <>
          {changes.length > 0 ? (
            <ul className="rows">
              {changes.map((change) => (
                <li key={change.productId} className="row row--static">
                  <span className="row__main">
                    <strong>{change.name}</strong>
                    <span className="muted">{change.unit}</span>
                  </span>
                  <Chip tone={change.maximumReturnMilli > 0 ? "warn" : "quiet"}>
                    Before {change.before}; after {change.after}
                  </Chip>
                </li>
              ))}
            </ul>
          ) : (
            <Banner>There is no tracked stock behind this order.</Banner>
          )}

          <form className="grid" onSubmit={submit} aria-busy={busy}>
            {reducedChanges.length > 0 && (
              <>
                <h3>What physically came back</h3>
                <p className="field__help">
                  Anything removed from the bill but not returned is recorded as written off.
                </p>
                {reducedChanges.map((change, index) => (
                  <div key={change.productId} className="grid">
                    <div className="field">
                      <label
                        className="field__label"
                        htmlFor={`return-${tabId}-${change.productId}`}
                      >
                        Product {index + 1}: returned {change.name}
                      </label>
                      <input
                        id={`return-${tabId}-${change.productId}`}
                        className="input input--num"
                        type="number"
                        inputMode="decimal"
                        min="0"
                        max={change.maximumReturnMilli / MILLI}
                        step="0.001"
                        required
                        disabled={busy}
                        value={returned[change.productId]?.quantity ?? ""}
                        aria-describedby={`return-help-${tabId}-${change.productId}`}
                        onChange={(event) =>
                          setReturned((current) => ({
                            ...current,
                            [change.productId]: {
                              quantity: event.target.value,
                              note: current[change.productId]?.note ?? "",
                            },
                          }))
                        }
                      />
                      <span
                        id={`return-help-${tabId}-${change.productId}`}
                        className="field__help"
                      >
                        Maximum {change.maximumReturn} {change.unit}. Enter zero if none came back.
                      </span>
                    </div>
                    <Field label={`Product ${index + 1}: note for ${change.name} (optional)`}>
                      <input
                        className="input"
                        value={returned[change.productId]?.note ?? ""}
                        disabled={busy}
                        onChange={(event) =>
                          setReturned((current) => ({
                            ...current,
                            [change.productId]: {
                              quantity: current[change.productId]?.quantity ?? "",
                              note: event.target.value,
                            },
                          }))
                        }
                      />
                    </Field>
                  </div>
                ))}
              </>
            )}

            <Field label={mode === "void" ? "Reason for void" : "Reason for correction"}>
              <textarea
                className="textarea"
                required
                disabled={busy}
                value={reasonText}
                onChange={(event) => setReasonText(event.target.value)}
              />
            </Field>
            <button
              type="submit"
              className={mode === "void" ? "btn btn--danger" : "btn btn--primary"}
              disabled={busy || reasonText.trim() === ""}
            >
              {mode === "void" ? "Void order permanently" : "Correct order and print replacement"}
            </button>
          </form>
        </>
      )}
    </Card>
  );
}
