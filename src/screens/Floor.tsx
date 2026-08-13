/**
 * The trading screens.
 *
 * The parts that need a night's trading behind them are honest about not
 * having one yet. **Nothing here invents a number.** A dashboard showing
 * plausible-looking figures that came from nowhere is how people stop trusting
 * a report, and the point of this rebuild is a set of screens the owner
 * believes.
 */

import { useCallback, useEffect, useState } from "react";

import type {
  BillView,
  BootstrapView,
  FloorView,
  InventoryView,
  OverviewView,
  PlacedOrder,
  ReportsView,
  SettledBill,
  StrandedPrint,
} from "../api";
import { api, reason, ServePointError } from "../api";
import { Banner, Card, Chip, Empty, Field, Loading, PageHead } from "../ui";
import { OrderCorrection } from "./OrderCorrection";

/** Quantities cross to Rust as thousandths. This is a unit, not a calculation. */
const MILLI = 1000;

/* ========================================================================== */

export function Overview({ boot }: { boot: BootstrapView }) {
  const shift = boot.openShift;
  const [tonight, setTonight] = useState<OverviewView>();

  useEffect(() => {
    // A failure here is not worth a banner: the screen is a summary, and the
    // empty states below already say the honest thing.
    api.overviewView().then(setTonight).catch(() => undefined);
  }, []);

  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Overview"
          title={shift ? `Trading — ${shift.businessDateLabel}` : "Nothing is trading"}
          blurb={
            shift
              ? `Opened by ${shift.openedBy}. Everything below is this night only.`
              : `The next night to open is ${boot.businessDateLabel}.`
          }
          aside={
            shift ? (
              <Chip tone={shift.overdue ? "warn" : "good"}>
                {shift.overdue ? "Overdue — still open" : shift.code}
              </Chip>
            ) : (
              <Chip tone="quiet">Closed</Chip>
            )
          }
        />

        {shift?.overdue && (
          <Banner tone="warn" title="This shift should have been closed">
            It is past the hour this venue closes its trading night. Sales made now still count
            towards {shift.businessDateLabel}, which is usually not what anyone intends.
          </Banner>
        )}

        <Card
          title="Tonight"
          blurb="What has been billed, and what is still sitting on open tabs. Deliberately two figures — the second is money nobody has been asked for yet."
        >
          {!tonight || tonight.quiet ? (
            <Empty glyph="◑" title="No trading recorded yet">
              Revenue, top waiter, top seller and low stock appear here as soon as the first order
              is issued. Until the till has served somebody there is genuinely nothing to show — and
              showing an invented figure would be worse than showing none.
            </Empty>
          ) : (
            <dl className="totals">
              <dt>Settled ({tonight.tabsSettled} tabs)</dt>
              <dd>{tonight.settled}</dd>
              <dt>Still running ({tonight.tabsOpen} tabs)</dt>
              <dd>{tonight.openTabs}</dd>
              {tonight.topWaiter && (
                <>
                  <dt>Busiest — {tonight.topWaiter.name}</dt>
                  <dd>{tonight.topWaiter.amount}</dd>
                </>
              )}
            </dl>
          )}
        </Card>

        <div className="grid grid--halves">
          <Card title="Moving fastest" blurb="What has actually gone out of the door tonight.">
            {!tonight || tonight.topSellers.length === 0 ? (
              <Empty glyph="↗" title="Nothing issued yet">
                This lists what has been poured tonight, in the quantity it went out. It fills in
                from the first order.
              </Empty>
            ) : (
              <ul className="rows">
                {tonight.topSellers.map((seller) => (
                  <li key={seller.name} className="row row--static">
                    <span className="row__main">
                      <strong>{seller.name}</strong>
                    </span>
                    <span className="row__value">{seller.quantity}</span>
                  </li>
                ))}
              </ul>
            )}
          </Card>

          <Card title="Running low" blurb="Stock that will not last the night.">
            {!tonight || tonight.lowStock.length === 0 ? (
              <Empty glyph="▽" title="Nothing is running low">
                Anything that falls below the level you set for it shows here — ahead of it running
                out, not after.
              </Empty>
            ) : (
              <ul className="rows">
                {tonight.lowStock.map((line) => (
                  <li key={line.name} className="row row--static">
                    <span className="row__main">
                      <strong>{line.name}</strong>
                      <span className="muted">{line.unit.toLowerCase()}</span>
                    </span>
                    <span className="row__value">
                      {line.onHand}
                      <Chip tone="warn">Low</Chip>
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>
      </div>
    </div>
  );
}

/* ========================================================================== */

export function Till() {
  const [view, setView] = useState<FloorView>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  const [float, setFloat] = useState("");
  const [waiterId, setWaiterId] = useState("");
  const [reference, setReference] = useState("");
  const [contact, setContact] = useState("");
  const [tabId, setTabId] = useState<number>();
  const [draft, setDraft] = useState<Record<number, number>>({});
  const [placed, setPlaced] = useState<PlacedOrder>();
  /** Bumping this remounts Recovery, which is how a fresh stranded print appears. */
  const [strandedSince, setStrandedSince] = useState(0);

  useEffect(() => {
    api.floorView().then(setView).catch((raw) => setError(reason(raw)));
  }, []);

  /** Every write returns the whole screen, so there is one way to refresh it. */
  const run = useCallback(async (work: () => Promise<FloorView>) => {
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
  }, []);

  if (!view) {
    return error ? (
      <div className="page">
        <div className="page__inner">
          <Banner tone="bad" title="The till could not be read">
            {error}
          </Banner>
        </div>
      </div>
    ) : (
      <Loading what="the floor" />
    );
  }

  const tab = view.tabs.find((candidate) => candidate.id === tabId);
  const ordered = Object.entries(draft).filter(([, units]) => units > 0);

  async function send() {
    if (!tab) return;
    setBusy(true);
    setError(undefined);
    try {
      const result = await api.placeOrder(
        tab.id,
        ordered.map(([saleItemId, units]) => ({
          saleItemId: Number(saleItemId),
          quantityMilli: units * MILLI,
        })),
      );
      setPlaced(result);
      setDraft({});
      setView(await api.floorView());
    } catch (raw) {
      if (raw instanceof ServePointError && raw.kind === "PRINT_PENDING") {
        setStrandedSince((count) => count + 1);
      }
      setError(reason(raw));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Till"
          title="Tabs"
          blurb={`Drinks go onto a tab, identified here by ${view.tabPrompt.toLowerCase()}.`}
          aside={
            view.shift ? (
              <Chip tone={view.shift.overdue ? "warn" : "good"}>{view.shift.code}</Chip>
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

        {placed && (
          <Banner tone="good" title="Sent to the bar">
            {placed.slips.map((slip) => `${slip.destination} ${slip.receiptNumber}`).join(", ")} —
            keep the slip. This tab now stands at {placed.tabTotal}.
          </Banner>
        )}

        <Recovery key={strandedSince} />

        {!view.shift ? (
          <Card
            title="Open the night"
            blurb="Count the float into the drawer first. Everything sold tonight belongs to this night."
          >
            <Field label="Opening float" help="What is in the drawer before anybody is served.">
              <input
                inputMode="decimal"
                value={float}
                onChange={(event) => setFloat(event.target.value)}
                placeholder="500.00"
              />
            </Field>
            <button
              type="button"
              className="btn btn--primary"
              disabled={busy || float.trim() === ""}
              onClick={() => run(() => api.openShift(float))}
            >
              Open the night
            </button>
          </Card>
        ) : (
          <>
            <Card title="Open a tab" blurb="A tab is held in a waiter's name until it is settled.">
              <Field label="Waiter">
                <select value={waiterId} onChange={(event) => setWaiterId(event.target.value)}>
                  <option value="">Choose a waiter</option>
                  {view.waiters.map((waiter) => (
                    <option key={waiter.id} value={waiter.id}>
                      {waiter.name}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label={view.tabPrompt}>
                <input value={reference} onChange={(event) => setReference(event.target.value)} />
              </Field>
              {view.wantsContact && (
                <Field label="Phone" help="Optional, but it is what the tab is looked up by later.">
                  <input value={contact} onChange={(event) => setContact(event.target.value)} />
                </Field>
              )}
              <button
                type="button"
                className="btn btn--primary"
                disabled={busy || waiterId === "" || reference.trim() === ""}
                onClick={async () => {
                  const ok = await run(() =>
                    api.openTab(Number(waiterId), reference, contact || undefined),
                  );
                  if (ok) {
                    setReference("");
                    setContact("");
                  }
                }}
              >
                Open the tab
              </button>
            </Card>

            <Card title="Open tabs" flush>
              {view.tabs.length === 0 ? (
                <Empty glyph="▤" title="No tabs are open">
                  Open one above, and it will hold every drink until somebody settles it.
                </Empty>
              ) : (
                <ul className="rows">
                  {view.tabs.map((candidate) => (
                    <li key={candidate.id}>
                      <button
                        type="button"
                        className="row"
                        aria-pressed={candidate.id === tabId}
                        onClick={() => {
                          setTabId(candidate.id);
                          setPlaced(undefined);
                        }}
                      >
                        <span className="row__main">
                          <strong>{candidate.label}</strong>
                          <span className="muted">
                            {candidate.waiter} · {candidate.code}
                          </span>
                        </span>
                        <span className="row__value">{candidate.runningTotal}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </Card>

            {tab && (
              <Card
                title={`Ring up — ${tab.label}`}
                blurb="A sale is refused outright when there is not enough stock to pour it."
                aside={<Chip tone="quiet">{tab.runningTotal}</Chip>}
              >
                {view.menu.length === 0 ? (
                  <Empty glyph="▦" title="Nothing is sellable yet">
                    A drink needs a recipe and a price before it can be rung up. The owner sets both
                    on the Settings screen.
                  </Empty>
                ) : (
                  <>
                    <ul className="rows">
                      {view.menu.map((item) => (
                        <li key={item.saleItemId} className="row row--static">
                          <span className="row__main">
                            <strong>{item.name}</strong>
                            <span className="muted">
                              {item.category} · {item.price}
                            </span>
                          </span>
                          <span className="stepper">
                            <button
                              type="button"
                              aria-label={`One fewer ${item.name}`}
                              onClick={() =>
                                setDraft((current) => ({
                                  ...current,
                                  [item.saleItemId]: Math.max(
                                    0,
                                    (current[item.saleItemId] ?? 0) - 1,
                                  ),
                                }))
                              }
                            >
                              −
                            </button>
                            <span>{draft[item.saleItemId] ?? 0}</span>
                            <button
                              type="button"
                              aria-label={`One more ${item.name}`}
                              onClick={() =>
                                setDraft((current) => ({
                                  ...current,
                                  [item.saleItemId]: (current[item.saleItemId] ?? 0) + 1,
                                }))
                              }
                            >
                              +
                            </button>
                          </span>
                        </li>
                      ))}
                    </ul>
                    <button
                      type="button"
                      className="btn btn--primary"
                      disabled={busy || ordered.length === 0}
                      onClick={send}
                    >
                      Send to the bar
                    </button>
                  </>
                )}
              </Card>
            )}

            {tab && (
              <OrderCorrection
                tabId={tab.id}
                menu={view.menu}
                onChanged={setView}
                onPrintPending={() => setStrandedSince((count) => count + 1)}
              />
            )}

            {tab && (
              <Settle
                key={tab.id}
                tabId={tab.id}
                onSettled={async () => {
                  setTabId(undefined);
                  setPlaced(undefined);
                  setView(await api.floorView());
                }}
              />
            )}
          </>
        )}
      </div>
    </div>
  );
}

/**
 * Orders whose paper never came out.
 *
 * Nothing is decided here on the operator's behalf. They tell the till what
 * actually happened at the printer, because only they were standing there —
 * and the answer changes whether the stock moved.
 */
function Recovery() {
  const [prints, setPrints] = useState<StrandedPrint[]>([]);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .recoveryView()
      .then((view) => setPrints(view.prints))
      .catch((raw) => setError(reason(raw)));
  }, []);

  async function resolve(work: () => Promise<{ prints: StrandedPrint[] }>) {
    setBusy(true);
    setError(undefined);
    try {
      setPrints((await work()).prints);
    } catch (raw) {
      setError(reason(raw));
    } finally {
      setBusy(false);
    }
  }

  if (prints.length === 0) return null;

  return (
    <Card
      title="A slip did not print"
      blurb="These numbers are already issued and the stock behind them is held. The night cannot close until each one is answered."
      aside={<Chip tone="warn">{prints.length}</Chip>}
    >
      {error && (
        <Banner tone="bad" title="That was refused">
          {error}
        </Banner>
      )}
      {prints.map((stuck) => (
        <div key={stuck.orderId}>
          <p>
            <strong>{stuck.tabLabel}</strong> · {stuck.waiter} ·{" "}
            {stuck.receiptNumbers.join(", ")}
          </p>
          <div className="grid grid--halves">
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => resolve(() => api.resolveHandwritten(stuck.orderId))}
            >
              I wrote it by hand
            </button>
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => resolve(() => api.resolveNonPrint(stuck.orderId))}
            >
              Nothing came out
            </button>
          </div>
        </div>
      ))}
    </Card>
  );
}

/**
 * The bill, and then the money.
 *
 * Before the tab closes these figures are calculated; afterwards they are read
 * back from the frozen payment. The two are shown in different places on
 * purpose — what the customer paid is never recalculated.
 */
function Settle({ tabId, onSettled }: { tabId: number; onSettled: () => Promise<void> }) {
  const [bill, setBill] = useState<BillView>();
  const [settled, setSettled] = useState<SettledBill>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [comp, setComp] = useState("");
  const [tin, setTin] = useState("");
  const [writingOff, setWritingOff] = useState(false);

  useEffect(() => {
    api
      .tabBill(tabId)
      .then(setBill)
      .catch((raw) => setError(reason(raw)));
  }, [tabId]);

  if (settled) {
    return (
      <Card
        title={settled.comped ? "Written off" : "Settled"}
        blurb={`${settled.label} · ${settled.code}`}
        aside={<Chip tone={settled.comped ? "warn" : "good"}>{settled.total}</Chip>}
      >
        <dl className="totals">
          <dt>Subtotal</dt>
          <dd>{settled.subtotal}</dd>
          <dt>Service</dt>
          <dd>{settled.serviceCharge}</dd>
          <dt>Tax</dt>
          <dd>{settled.tax}</dd>
          <dt>Total</dt>
          <dd>{settled.total}</dd>
          <dt>{settled.waiter} now owes</dt>
          <dd>{settled.liability}</dd>
        </dl>
        {settled.compReason && (
          <p className="field__help">Written off: {settled.compReason}</p>
        )}
        <button type="button" className="btn btn--primary" onClick={() => void onSettled()}>
          Done
        </button>
      </Card>
    );
  }

  if (!bill) {
    return error ? (
      <Banner tone="bad" title="The bill could not be worked out">
        {error}
      </Banner>
    ) : (
      <Loading what="the bill" />
    );
  }

  async function settle() {
    setBusy(true);
    setError(undefined);
    try {
      setSettled(
        await api.settleTab(tabId, writingOff ? comp : undefined, tin || undefined),
      );
    } catch (raw) {
      setError(reason(raw));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card
      title="The bill"
      blurb="Every drink issued to this tab. Settling it closes the tab and puts what was taken against the waiter."
      aside={<Chip tone="quiet">{bill.total}</Chip>}
    >
      {error && (
        <Banner tone="bad" title="That was refused">
          {error}
        </Banner>
      )}

      <dl className="totals">
        <dt>{bill.taxExtracted ? "Menu total" : "Subtotal"}</dt>
        <dd>{bill.taxExtracted ? bill.lineTotal : bill.net}</dd>
        {bill.taxExtracted && (
          <>
            <dt>Subtotal, tax taken out</dt>
            <dd>{bill.net}</dd>
          </>
        )}
        {bill.showService && (
          <>
            <dt>{bill.serviceLabel}</dt>
            <dd>{bill.serviceCharge}</dd>
          </>
        )}
        {bill.showTax && (
          <>
            <dt>{bill.taxLabel}</dt>
            <dd>{bill.tax}</dd>
          </>
        )}
        <dt>Total</dt>
        <dd>{bill.total}</dd>
      </dl>

      {bill.asksCustomerTin && (
        <Field label="Customer TIN" help="Only if the customer asks for it on the receipt.">
          <input value={tin} onChange={(event) => setTin(event.target.value)} />
        </Field>
      )}

      {bill.compsEnabled && (
        <>
          <button type="button" className="btn" onClick={() => setWritingOff(!writingOff)}>
            {writingOff ? "Charge it after all" : "Write this bill off"}
          </button>
          {writingOff && (
            <Field
              label="Why"
              help="A written-off bill is recorded with its reason and never quietly disappears."
            >
              <input value={comp} onChange={(event) => setComp(event.target.value)} />
            </Field>
          )}
        </>
      )}

      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || (writingOff && comp.trim() === "")}
        onClick={settle}
      >
        {writingOff ? "Write it off" : `Take ${bill.total} and close`}
      </button>
    </Card>
  );
}

export function Inventory() {
  const [view, setView] = useState<InventoryView>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    api.inventoryView().then(setView).catch((raw) => setError(reason(raw)));
  }, []);

  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Inventory"
          title="What is on the shelf"
          blurb="Stock is worked out from every movement ever recorded, never from a stored figure that can drift."
          aside={view ? <Chip tone="quiet">{view.totalValue}</Chip> : undefined}
        />
        {error && (
          <Banner tone="bad" title="The shelf could not be read">
            {error}
          </Banner>
        )}
        {!view && !error && <Loading what="the shelf" />}
        {view && (
          <Card flush>
            {view.lines.length === 0 ? (
              <Empty glyph="▦" title="Nothing has been counted in">
                Deliveries, counts and adjustments will appear here. Cocktails and shots draw down
                their ingredients through a recipe, so a bottle sold by the shot and a bottle sold
                whole come out of the same shelf.
              </Empty>
            ) : (
              <ul className="rows">
                {view.lines.map((line) => (
                  <li key={line.productId} className="row row--static">
                    <span className="row__main">
                      <strong>{line.name}</strong>
                      <span className="muted">
                        {line.category} · {line.unit.toLowerCase()}
                      </span>
                    </span>
                    <span className="row__value">
                      {line.tracked ? `${line.onHand} · ${line.value}` : "not tracked"}
                      {line.low && <Chip tone="warn">Low</Chip>}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        )}
      </div>
    </div>
  );
}

export function Reports() {
  const [view, setView] = useState<ReportsView>();
  const [error, setError] = useState<string>();

  const load = useCallback((shiftId?: number) => {
    api
      .reportsView(shiftId)
      .then((next) => {
        setView(next);
        setError(undefined);
      })
      .catch((raw) => setError(reason(raw)));
  }, []);

  useEffect(() => load(), [load]);

  if (!view) {
    return error ? (
      <div className="page">
        <div className="page__inner">
          <Banner tone="bad" title="The report could not be read">
            {error}
          </Banner>
        </div>
      </div>
    ) : (
      <Loading what="the reports" />
    );
  }

  const report = view.showing;

  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Reports"
          title={report ? `${report.businessDateLabel} — ${report.shiftCode}` : "What the numbers say"}
          blurb={
            report
              ? `Opened ${report.openedAtLabel} by ${report.openedBy}, closed ${report.closedAtLabel} by ${report.closedBy}. Stored when the night was sealed and read back unchanged.`
              : "Read on screen. Printing a report on till paper is off unless somebody switches it on."
          }
          aside={
            view.nights.length > 1 ? (
              <select
                className="select"
                value={view.showingShiftId ?? ""}
                onChange={(event) => load(Number(event.target.value))}
              >
                {view.nights.map((night) => (
                  <option key={night.shiftId} value={night.shiftId}>
                    {night.businessDateLabel} · {night.code}
                  </option>
                ))}
              </select>
            ) : (
              report && <Chip tone="quiet">Closed</Chip>
            )
          }
        />

        {error && (
          <Banner tone="bad" title="That night could not be opened">
            {error}
          </Banner>
        )}

        {!report ? (
          <Card
            title="Popular drinks, and how they sell"
            blurb="By shot, by bottle and inside cocktails — separately, because they are different businesses."
          >
            {view.nights.length === 0 ? (
              <Empty glyph="▥" title="No nights closed yet">
                Reports are built from closed shifts, so the first one appears after the first night
                is reconciled and closed. A closed night is then read back exactly as it was stored,
                never recalculated — so a report cannot quietly change months later.
              </Empty>
            ) : (
              <Empty glyph="▥" title="That night has no stored report">
                It was sealed before this till began storing reports, and the figures are
                deliberately not reconstructed now — a report assembled after the fact is not the
                document that was signed. Nights closed from here on are stored as they are sealed.
              </Empty>
            )}
          </Card>
        ) : (
          <>
            <Card
              title="What was billed"
              blurb="What customers were charged across the night. Kept apart from the drawer below, because they answer different questions."
            >
              <dl className="totals">
                <dt>Gross sales ({report.tabsSettled} tabs)</dt>
                <dd>{report.grossSales}</dd>
                <dt>Service</dt>
                <dd>{report.serviceCharge}</dd>
                <dt>Tax</dt>
                <dd>{report.tax}</dd>
                <dt>Total billed</dt>
                <dd>{report.totalBilled}</dd>
              </dl>
            </Card>

            <Card
              title="The drawer"
              blurb="What the ledger says should have been in it, against what was counted out of it."
            >
              <dl className="totals">
                <dt>Opening float</dt>
                <dd>{report.openingFloat}</dd>
                <dt>Cash from waiters</dt>
                <dd>{report.cashFromWaiters}</dd>
                <dt>Other movements</dt>
                <dd>{report.otherMovements}</dd>
                <dt>Expected</dt>
                <dd>{report.expectedCash}</dd>
                <dt>Counted</dt>
                <dd>{report.countedCash}</dd>
              </dl>
              {report.balanced ? (
                <Chip tone="good">Balanced</Chip>
              ) : (
                <Chip tone="warn">
                  {report.over ? "Over" : "Short"} by {report.variance}
                </Chip>
              )}
            </Card>

            <div className="grid grid--halves">
              <Card title="Who handed over what" blurb="Every waiter who settled on the night.">
                {report.waiters.length === 0 ? (
                  <Empty glyph="◇" title="Nobody settled">
                    No waiter carried money on this night.
                  </Empty>
                ) : (
                  <ul className="rows">
                    {report.waiters.map((waiter) => (
                      <li key={waiter.name} className="row row--static">
                        <span className="row__main">
                          <strong>{waiter.name}</strong>
                          <span className="muted">expected {waiter.expected}</span>
                        </span>
                        <span className="row__value">{waiter.cash}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </Card>

              <Card title="What sold" blurb="What actually left the bar, by value.">
                {report.items.length === 0 ? (
                  <Empty glyph="↗" title="Nothing was issued">
                    No order was issued on this night.
                  </Empty>
                ) : (
                  <ul className="rows">
                    {report.items.map((item) => (
                      <li key={item.name} className="row row--static">
                        <span className="row__main">
                          <strong>{item.name}</strong>
                          <span className="muted">×{item.quantity}</span>
                        </span>
                        <span className="row__value">{item.value}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </Card>
            </div>

            <Card
              title="Exceptions"
              blurb="Everything on this night that somebody has to be able to answer for."
            >
              <dl className="totals">
                <dt>Corrections</dt>
                <dd>{report.corrections}</dd>
                <dt>Voids</dt>
                <dd>{report.voids}</dd>
                <dt>Comped tabs ({report.compedTabs})</dt>
                <dd>{report.compedValue}</dd>
                <dt>Written off</dt>
                <dd>{report.writtenOff}</dd>
                <dt>Settled without cash</dt>
                <dd>{report.nonCash}</dd>
              </dl>
            </Card>

            {view.renderedText && (
              <Card
                title="The paper"
                blurb="The report exactly as it was stored on the night — the same text a printed copy carries."
              >
                <details>
                  <summary className="muted">Show what was signed</summary>
                  <pre className="paper">{view.renderedText}</pre>
                </details>
              </Card>
            )}
          </>
        )}
      </div>
    </div>
  );
}
