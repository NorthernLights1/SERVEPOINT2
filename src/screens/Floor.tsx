/**
 * The trading screens.
 *
 * The parts that need a night's trading behind them are honest about not
 * having one yet. **Nothing here invents a number.** A dashboard showing
 * plausible-looking figures that came from nowhere is how people stop trusting
 * a report, and the point of this rebuild is a set of screens the owner
 * believes.
 */

import type { ReactNode } from "react";

import type { BootstrapView } from "../api";
import { Banner, Card, Chip, Empty } from "../ui";

function PageHead({
  eyebrow,
  title,
  blurb,
  aside,
}: {
  eyebrow: string;
  title: string;
  blurb: string;
  aside?: ReactNode;
}) {
  return (
    <div className="pagehead">
      <div className="pagehead__text">
        <span className="eyebrow">{eyebrow}</span>
        <h1>{title}</h1>
        <p className="muted">{blurb}</p>
      </div>
      {aside}
    </div>
  );
}

/* ========================================================================== */

export function Overview({ boot }: { boot: BootstrapView }) {
  const shift = boot.openShift;

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
          blurb="Revenue, sales, the busiest waiter and the drinks moving fastest."
        >
          <Empty glyph="◑" title="No trading recorded yet">
            Revenue, top waiter, top seller and low stock appear here as soon as the first order is
            issued. Until the till has served somebody there is genuinely nothing to show — and
            showing an invented figure would be worse than showing none.
          </Empty>
        </Card>

        <div className="grid grid--halves">
          <Card title="What to prioritise" blurb="Which drinks earn the most per bottle poured.">
            <Empty glyph="↗" title="Needs a week of trading">
              This ranks each drink by what it actually contributes, not by how many go out of the
              door. It needs purchase costs and a few nights of sales before the ranking says
              anything true.
            </Empty>
          </Card>

          <Card title="Running low" blurb="Stock that will not last the night.">
            <Empty glyph="▽" title="Nothing counted in yet">
              Once deliveries are recorded and a stock count exists, anything about to run out shows
              here — ahead of it running out.
            </Empty>
          </Card>
        </div>
      </div>
    </div>
  );
}

/* ========================================================================== */

export function Till({ boot }: { boot: BootstrapView }) {
  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Till"
          title="Tabs"
          blurb={`Drinks go onto a tab, identified here by ${boot.tabPrompt.toLowerCase()}.`}
        />
        <Card>
          <Empty glyph="▤" title="The till is not built yet">
            Opening tabs, taking orders and printing issue slips are the next thing to be built. The
            rules underneath them are already in place — including the one that matters most: a sale
            is refused outright when there is not enough stock to pour it.
          </Empty>
        </Card>
      </div>
    </div>
  );
}

export function Inventory() {
  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Inventory"
          title="What is on the shelf"
          blurb="Stock is worked out from every movement ever recorded, never from a stored figure that can drift."
        />
        <Card>
          <Empty glyph="▦" title="Nothing has been counted in">
            Deliveries, counts and adjustments will appear here. Cocktails and shots draw down their
            ingredients through a recipe, so a bottle sold by the shot and a bottle sold whole come
            out of the same shelf.
          </Empty>
        </Card>
      </div>
    </div>
  );
}

export function EndOfDay({ boot }: { boot: BootstrapView }) {
  const shift = boot.openShift;
  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="End of day"
          title="Reconcile the night"
          blurb="Each waiter hands over what they took. What they owe comes from the tabs in their name, and nothing else."
          aside={shift ? <Chip tone="good">{shift.code}</Chip> : <Chip tone="quiet">Closed</Chip>}
        />
        {shift ? (
          <Card>
            <Empty glyph="◫" title="Reconciliation is not built yet">
              This is where each waiter is settled one at a time, and where the counted cash is
              compared with what the ledger expects. Nothing a customer paid can reach the cash
              drawer without passing through this screen — which is what stops the drawer looking
              short for no reason anybody can explain.
            </Empty>
          </Card>
        ) : (
          <Card>
            <Empty glyph="○" title="No night is open">
              Reconciliation happens at the end of a trading night. Open one first.
            </Empty>
          </Card>
        )}
      </div>
    </div>
  );
}

export function Reports() {
  return (
    <div className="page">
      <div className="page__inner">
        <PageHead
          eyebrow="Reports"
          title="What the numbers say"
          blurb="Read on screen. Printing a report on till paper is off unless somebody switches it on."
        />
        <Card
          title="Popular drinks, and how they sell"
          blurb="By shot, by bottle and inside cocktails — separately, because they are different businesses."
        >
          <Empty glyph="▥" title="No nights closed yet">
            Reports are built from closed shifts, so the first one appears after the first night is
            reconciled and closed. A closed night is then read back exactly as it was stored, never
            recalculated — so a report cannot quietly change months later.
          </Empty>
        </Card>
      </div>
    </div>
  );
}
