/**
 * End of the night.
 *
 * Each waiter hands over what they took, then the drawer is counted. The
 * counted figure is never adjusted towards what the ledger expects — the
 * difference between the two is the whole reason for counting.
 */

import { useEffect, useState } from "react";

import type { ClosedNight, ReconciliationView, SettleMethod, WaiterSettlement } from "../api";
import { api, reason } from "../api";
import { Banner, Card, Chip, Empty, Field, Loading, PageHead } from "../ui";

export function EndOfDay() {
  const [view, setView] = useState<ReconciliationView>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .reconciliationView()
      .then(setView)
      .catch((raw) => setError(reason(raw)));
  }, []);

  async function run(work: () => Promise<ReconciliationView>) {
    setBusy(true);
    setError(undefined);
    try {
      setView(await work());
      return true;
    } catch (raw) {
      setError(reason(raw));
      return false;
    } finally {
      setBusy(false);
    }
  }

  if (!view) {
    return error ? (
      <div className="page">
        <div className="page__inner">
          <Banner tone="bad" title="The night could not be read">
            {error}
          </Banner>
        </div>
      </div>
    ) : (
      <Loading what="the night" />
    );
  }

  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="End of day"
          title="Reconcile the night"
          blurb="Each waiter hands over what they took. What they owe comes from the tabs in their name, and nothing else."
          aside={
            view.shift ? (
              <Chip tone="good">{view.shift.code}</Chip>
            ) : (
              <Chip tone="quiet">Closed</Chip>
            )
          }
        />

        {error && (
          <Banner tone="bad" title="That was refused">
            {error}
          </Banner>
        )}

        {!view.shift ? (
          <Card>
            <Empty glyph="○" title="No night is open">
              Reconciliation happens at the end of a trading night. Open one on the till first.
            </Empty>
          </Card>
        ) : (
          <>
            <Card
              title="The drawer"
              blurb="What should be in it, worked out from every movement recorded tonight — the float, and everything handed over since."
              aside={<Chip tone="quiet">{view.expectedCash}</Chip>}
            >
              {view.shift && view.blocker === "This night has already begun closing." ? (
                <CountTheDrawer expected={view.expectedCash} />
              ) : view.blocker ? (
                <Banner tone="warn" title="The night cannot close yet">
                  {view.blocker}
                </Banner>
              ) : (
                <>
                  <Banner tone="good" title="Everything is settled">
                    Nobody is holding money and every print is resolved. Closing stops the night
                    taking any more trade.
                  </Banner>
                  <button
                    type="button"
                    className="btn btn--primary"
                    disabled={busy}
                    onClick={() => run(() => api.beginClosing())}
                  >
                    Begin closing the night
                  </button>
                </>
              )}
            </Card>

            {view.waiters.map((waiter) => (
              <Settlement key={waiter.waiterId} waiter={waiter} busy={busy} run={run} />
            ))}
          </>
        )}
      </div>
    </div>
  );
}

/**
 * The physical count, and what it says.
 *
 * The counted figure is never adjusted towards what the ledger expects. A
 * drawer that is short is a fact the owner needs — quietly correcting it here
 * is how a venue stops being able to trust any of these numbers.
 */
function CountTheDrawer({ expected }: { expected: string }) {
  const [counted, setCounted] = useState("");
  const [closed, setClosed] = useState<ClosedNight>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  if (closed) {
    return (
      <>
        <Banner
          tone={closed.balanced ? "good" : "warn"}
          title={
            closed.balanced
              ? `${closed.code} closed and balanced`
              : `${closed.code} closed ${closed.variance} ${closed.over ? "over" : "short"}`
          }
        >
          {closed.businessDateLabel} is sealed. The ledger expected {closed.expected} and{" "}
          {closed.counted} was counted.
          {!closed.balanced &&
            " The difference is recorded as it stands — nothing was adjusted to make it match."}
        </Banner>
        <p className="muted">Open the next night on the till when you are ready.</p>
      </>
    );
  }

  return (
    <>
      <Banner tone="warn" title="This night is closing">
        It is taking no more trade. Count the drawer and enter what is physically in it.
      </Banner>
      {error && (
        <Banner tone="bad" title="That was refused">
          {error}
        </Banner>
      )}
      <Field
        label="Counted cash"
        help={`The ledger expects ${expected}. Enter what you actually counted, even if it differs — especially if it differs.`}
      >
        <input
          inputMode="decimal"
          value={counted}
          onChange={(event) => setCounted(event.target.value)}
        />
      </Field>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || counted.trim() === ""}
        onClick={async () => {
          setBusy(true);
          setError(undefined);
          try {
            setClosed(await api.closeNight(counted));
          } catch (raw) {
            setError(reason(raw));
          } finally {
            setBusy(false);
          }
        }}
      >
        Close the night
      </button>
    </>
  );
}

function Settlement({
  waiter,
  busy,
  run,
}: {
  waiter: WaiterSettlement;
  busy: boolean;
  run: (work: () => Promise<ReconciliationView>) => Promise<boolean>;
}) {
  const [method, setMethod] = useState<SettleMethod>("CASH");
  const [amount, setAmount] = useState("");
  const [reasonText, setReasonText] = useState("");

  if (!waiter.owesAnything) {
    return (
      <Card title={waiter.name} aside={<Chip tone="good">Settled</Chip>}>
        <p className="muted">Holding nothing. Everything in their name has been handed over.</p>
      </Card>
    );
  }

  return (
    <Card
      title={waiter.name}
      blurb={
        waiter.oldBalance
          ? "Carried over from an earlier night. There are no tabs left behind this — it is a shortfall they still hold."
          : "Every closed tab in their name. Settling clears all of them together."
      }
      aside={<Chip tone="warn">{waiter.due}</Chip>}
    >
      {waiter.tabs.length > 0 && (
        <ul className="rows">
          {waiter.tabs.map((tab) => (
            <li key={tab.tabId} className="row row--static">
              <span className="row__main">
                <strong>{tab.label}</strong>
                <span className="muted">{tab.code}</span>
              </span>
              <span className="row__value">{tab.amount}</span>
            </li>
          ))}
        </ul>
      )}

      <Field label="How it was handed over">
        <select value={method} onChange={(event) => setMethod(event.target.value as SettleMethod)}>
          <option value="CASH">Cash</option>
          <option value="NON_CASH">Transfer or card</option>
          <option value="WRITE_OFF">Written off</option>
        </select>
      </Field>
      <Field
        label="How much"
        help={`${waiter.due} is owed. Less than that is recorded as a shortfall they still carry — never quietly forgiven.`}
      >
        <input
          inputMode="decimal"
          value={amount}
          onChange={(event) => setAmount(event.target.value)}
          placeholder={waiter.due}
        />
      </Field>
      {method === "WRITE_OFF" && (
        <Field label="Why" help="A write-off is recorded with its reason and stays in the record.">
          <input value={reasonText} onChange={(event) => setReasonText(event.target.value)} />
        </Field>
      )}

      <button
        type="button"
        className="btn btn--primary"
        disabled={
          busy || amount.trim() === "" || (method === "WRITE_OFF" && reasonText.trim() === "")
        }
        onClick={async () => {
          const ok = await run(() =>
            api.settleWaiter(waiter.waiterId, method, amount, reasonText || undefined),
          );
          if (ok) {
            setAmount("");
            setReasonText("");
          }
        }}
      >
        Settle {waiter.name}
      </button>
    </Card>
  );
}
