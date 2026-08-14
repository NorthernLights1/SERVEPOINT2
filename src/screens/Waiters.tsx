/**
 * Who works here.
 *
 * Its own screen rather than a third card on the warehouse, because adding a
 * waiter is what a new venue does first and what it does every time somebody
 * starts — it has nothing to do with what is on the shelf.
 *
 * Nobody is ever deleted. A name is on receipts, orders and reconciliations
 * that have to stay readable forever, so leaving is `active = 0`.
 */

import { useEffect, useState } from "react";

import type { SetupView } from "../api";
import { api, ServePointError } from "../api";
import { Banner, Card, Chip, Empty, Field, Loading } from "../ui";

function reason(error: unknown): string {
  return error instanceof ServePointError
    ? error.message
    : "Something went wrong and the reason was not readable.";
}

export function Waiters() {
  const [view, setView] = useState<SetupView>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [role, setRole] = useState("WAITER");
  const [pin, setPin] = useState("");

  const signsIn = role !== "WAITER";

  useEffect(() => {
    api
      .setupView()
      .then(setView)
      .catch((raw) => setError(reason(raw)));
  }, []);

  async function run(work: () => Promise<SetupView>) {
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
          <Banner tone="bad" title="The team could not be read">
            {error}
          </Banner>
        </div>
      </div>
    ) : (
      <Loading what="the team" />
    );
  }

  return (
    <div className="page">
      <div className="page__inner">
        <div className="pagehead">
          <div className="pagehead__text">
            <span className="eyebrow">Floor team</span>
            <h1>Who works here</h1>
            <p className="muted">
              Waiters hold tabs and never sign in. Cashiers work the till. An owner can do both.
            </p>
          </div>
        </div>

        {error && (
          <Banner tone="bad" title="That was refused">
            {error}
          </Banner>
        )}

        <Card
          title="The team"
          blurb="A tab is always opened in a waiter's name, so there has to be at least one."
          aside={
            <button type="button" className="btn" onClick={() => setOpen(!open)}>
              {open ? "Cancel" : "Add"}
            </button>
          }
        >
          {open && (
            <>
              <div className="grid grid--halves">
                <Field label="Name">
                  <input value={name} onChange={(event) => setName(event.target.value)} />
                </Field>
                <Field label="Role">
                  <select value={role} onChange={(event) => setRole(event.target.value)}>
                    <option value="WAITER">Waiter</option>
                    <option value="CASHIER">Cashier</option>
                    <option value="OWNER">Owner</option>
                  </select>
                </Field>
                {signsIn && (
                  <Field label="PIN" help="Four digits. They will need it to sign in.">
                    <input
                      inputMode="numeric"
                      value={pin}
                      onChange={(event) => setPin(event.target.value)}
                    />
                  </Field>
                )}
              </div>
              <button
                type="button"
                className="btn btn--primary"
                disabled={busy || name.trim() === ""}
                onClick={async () => {
                  const ok = await run(() =>
                    api.addStaff({ name, role, pin: signsIn ? pin : null }),
                  );
                  if (ok) {
                    setName("");
                    setPin("");
                    setOpen(false);
                  }
                }}
              >
                Add them
              </button>
            </>
          )}

          {view.staff.length === 0 ? (
            <Empty glyph="♙" title="Nobody on the floor yet">
              Add a waiter. Until there is one, no tab can be opened in anybody's name and the
              till cannot serve.
            </Empty>
          ) : (
            <ul className="rows">
              {view.staff.map((person) => (
                <li key={person.id} className="row row--static">
                  <span className="row__main">
                    <strong>{person.name}</strong>
                    <span className="muted">
                      {person.code} · {person.role.toLowerCase()}
                    </span>
                  </span>
                  <span className="row__value">
                    {!person.active && <Chip tone="quiet">Left</Chip>}
                    <button
                      type="button"
                      className="btn"
                      disabled={busy}
                      onClick={() => run(() => api.setStaffActive(person.id, !person.active))}
                    >
                      {person.active ? "They have left" : "Bring back"}
                    </button>
                  </span>
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>
    </div>
  );
}
