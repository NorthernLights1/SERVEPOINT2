/**
 * The two screens that stand in front of everything else: signing in, and the
 * one-time setup that has to happen before a venue can trade at all.
 */

import { useEffect, useState } from "react";

import {
  api,
  type AccountView,
  type BootstrapView,
  type ServePointError,
  type SettingChange,
  type Session,
} from "../api";
import { Banner, Card, Field, PinPad, Segmented, SwitchRow } from "../ui";

const MAX_PIN = 8;

/* ========================================================================== */
/* Signing in                                                                  */
/* ========================================================================== */

export function SignIn({
  boot,
  onSignedIn,
}: {
  boot: BootstrapView;
  onSignedIn: (session: Session) => void;
}) {
  const [account, setAccount] = useState<AccountView | undefined>(boot.accounts[0]);
  const [pin, setPin] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function submit() {
    if (!account || pin.length < 4 || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      onSignedIn(await api.signIn(account.staffId, pin));
    } catch (raw) {
      setError((raw as ServePointError).message);
      // Always clear. Leaving a rejected PIN on screen invites the same wrong
      // digits again, and each retry costs more than the last.
      setPin("");
      setBusy(false);
    }
  }

  // A till usually has a keyboard even when it has a touchscreen. Whichever is
  // to hand should work without the operator thinking about it.
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key >= "0" && event.key <= "9") {
        setPin((current) => (current.length < MAX_PIN ? current + event.key : current));
      } else if (event.key === "Backspace") {
        setPin((current) => current.slice(0, -1));
      } else if (event.key === "Enter") {
        void submit();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <div className="gate">
      <div className="gate__panel">
        <div className="gate__head">
          <div className="brand__mark" aria-hidden="true">
            S
          </div>
          <h1>{boot.venue.configured ? boot.venue.name : "ServePoint"}</h1>
          <p className="muted">{boot.businessDateLabel}</p>
        </div>

        <div className="accountlist">
          {boot.accounts.map((entry) => (
            <button
              key={entry.staffId}
              type="button"
              className="account"
              aria-pressed={entry.staffId === account?.staffId}
              onClick={() => {
                setAccount(entry);
                setPin("");
                setError(undefined);
              }}
            >
              <span className="avatar" aria-hidden="true">
                {entry.name.slice(0, 1).toUpperCase()}
              </span>
              <span className="account__text">
                <span>{entry.name}</span>
                <span className="faint" style={{ fontSize: 12.5 }}>
                  {entry.role === "OWNER" ? "Owner — reports and settings" : "Cashier — the till"}
                </span>
              </span>
            </button>
          ))}
        </div>

        {error && <Banner tone="bad">{error}</Banner>}

        <Card>
          <PinPad
            length={pin.length}
            max={MAX_PIN}
            disabled={busy || !account}
            onDigit={(digit) =>
              setPin((current) => (current.length < MAX_PIN ? current + digit : current))
            }
            onBackspace={() => setPin((current) => current.slice(0, -1))}
            onSubmit={() => void submit()}
          />
        </Card>
      </div>
    </div>
  );
}

/* ========================================================================== */
/* First run                                                                   */
/* ========================================================================== */

interface Draft {
  businessName: string;
  address: string;
  phone: string;
  tin: string;
  currency: string;
  taxEnabled: boolean;
  taxRate: string;
  taxInclusive: string;
  serviceEnabled: boolean;
  serviceRate: string;
  dayStart: string;
  dayEnd: string;
  referenceMode: string;
  ownerName: string;
  ownerPin: string;
  cashierName: string;
  cashierPin: string;
}

const EMPTY: Draft = {
  businessName: "",
  address: "",
  phone: "",
  tin: "",
  currency: "",
  taxEnabled: false,
  taxRate: "15",
  taxInclusive: "1",
  serviceEnabled: true,
  serviceRate: "10",
  dayStart: "18:00",
  dayEnd: "06:00",
  referenceMode: "TABLE",
  ownerName: "",
  ownerPin: "",
  cashierName: "",
  cashierPin: "",
};

const STEPS = ["The venue", "What you charge", "Who uses it"];

/** Percent typed by a person, as the basis points the database stores. */
function toBasisPoints(percent: string): string {
  const value = Number(percent.replace(",", ".").trim());
  if (!Number.isFinite(value) || value < 0) return "0";
  return String(Math.round(value * 100));
}

export function Setup({ onDone }: { onDone: (session: Session) => void }) {
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<Draft>(EMPTY);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setDraft((current) => ({ ...current, [key]: value }));

  const canContinue =
    step === 0
      ? draft.businessName.trim().length > 0
      : step === 1
        ? true
        : draft.ownerName.trim().length > 0 &&
          draft.cashierName.trim().length > 0 &&
          draft.ownerPin.length >= 4 &&
          draft.cashierPin.length >= 4;

  async function finish() {
    setBusy(true);
    setError(undefined);
    const changes: SettingChange[] = [
      { key: "receipt.business_name", value: draft.businessName.trim() },
      { key: "receipt.address", value: draft.address.trim() },
      { key: "receipt.phone", value: draft.phone.trim() },
      { key: "receipt.tin", value: draft.tin.trim() },
      { key: "locale.currency_code", value: draft.currency.trim().toUpperCase() },
      { key: "tax.enabled", value: draft.taxEnabled ? "1" : "0" },
      { key: "tax.rate_bp", value: toBasisPoints(draft.taxRate) },
      { key: "tax.inclusive", value: draft.taxInclusive },
      { key: "service_charge.enabled", value: draft.serviceEnabled ? "1" : "0" },
      { key: "service_charge.rate_bp", value: toBasisPoints(draft.serviceRate) },
      { key: "shift.day_start", value: draft.dayStart },
      { key: "shift.day_end", value: draft.dayEnd },
      { key: "tabs.reference_mode", value: draft.referenceMode },
    ];
    try {
      onDone(
        await api.completeSetup({
          ownerName: draft.ownerName.trim(),
          ownerPin: draft.ownerPin,
          cashierName: draft.cashierName.trim(),
          cashierPin: draft.cashierPin,
          changes,
        }),
      );
    } catch (raw) {
      setError((raw as ServePointError).message);
      setBusy(false);
    }
  }

  return (
    <div className="gate">
      <div className="gate__panel gate__panel--wide">
        <div className="gate__head">
          <div className="brand__mark" aria-hidden="true">
            S
          </div>
          <h1>Set up this till</h1>
          <p className="muted">
            A few minutes, once. Everything here can be changed later from Settings.
          </p>
        </div>

        <div className="steps">
          {STEPS.map((name, index) => (
            <div key={name} style={{ display: "flex", alignItems: "center", gap: 8 }}>
              {index > 0 && <span className="step__bar" />}
              <span
                className={
                  index === step ? "step step--now" : index < step ? "step step--done" : "step"
                }
              >
                <span className="step__num">{index < step ? "✓" : index + 1}</span>
                {name}
              </span>
            </div>
          ))}
        </div>

        {error && <Banner tone="bad">{error}</Banner>}

        {step === 0 && (
          <Card
            title="The venue"
            blurb="This is what prints at the top of every receipt. Left blank, receipts print with no header — which is better than printing the wrong name on a tax document."
          >
            <Field label="Business name">
              <input
                className="input"
                value={draft.businessName}
                autoFocus
                placeholder="The name above the door"
                onChange={(event) => set("businessName", event.target.value)}
              />
            </Field>
            <Field label="Address" help="Optional.">
              <input
                className="input"
                value={draft.address}
                onChange={(event) => set("address", event.target.value)}
              />
            </Field>
            <Field label="Phone" help="Optional.">
              <input
                className="input"
                value={draft.phone}
                onChange={(event) => set("phone", event.target.value)}
              />
            </Field>
            <Field label="TIN" help="The venue's own tax identification number. Optional.">
              <input
                className="input"
                value={draft.tin}
                onChange={(event) => set("tin", event.target.value)}
              />
            </Field>
            <Field
              label="Currency code"
              help="Three letters, printed after every amount. ETB, KES, NGN, and so on."
            >
              <input
                className="input input--short"
                value={draft.currency}
                maxLength={3}
                placeholder="ETB"
                onChange={(event) => set("currency", event.target.value.toUpperCase())}
              />
            </Field>
          </Card>
        )}

        {step === 1 && (
          <>
            <Card
              title="VAT and service charge"
              blurb="Both are optional and both can be changed at any time. Whatever is switched off here never appears on a bill at all."
            >
              <SwitchRow
                label="Charge VAT"
                help="Switch this off if the venue is not VAT registered. No VAT line will be printed."
                checked={draft.taxEnabled}
                onChange={(next) => set("taxEnabled", next)}
              />
              {draft.taxEnabled && (
                <>
                  <Field label="VAT rate">
                    <div className="inputgroup">
                      <input
                        className="input input--num input--short"
                        inputMode="decimal"
                        value={draft.taxRate}
                        onChange={(event) => set("taxRate", event.target.value)}
                      />
                      <span className="inputgroup__suffix">%</span>
                    </div>
                  </Field>
                  <Field
                    label="Menu prices"
                    help="This is a fact about the price list, not a preference. Get it wrong and every total is out by the VAT rate while looking perfectly normal."
                  >
                    <Segmented
                      label="Menu prices"
                      value={draft.taxInclusive}
                      onChange={(next) => set("taxInclusive", next)}
                      options={[
                        { value: "1", label: "Already include VAT" },
                        { value: "0", label: "VAT added on top" },
                      ]}
                    />
                  </Field>
                </>
              )}
              <SwitchRow
                label="Add a service charge"
                help="Added before VAT is worked out, because a service charge is itself taxable."
                checked={draft.serviceEnabled}
                onChange={(next) => set("serviceEnabled", next)}
              />
              {draft.serviceEnabled && (
                <Field label="Service charge">
                  <div className="inputgroup">
                    <input
                      className="input input--num input--short"
                      inputMode="decimal"
                      value={draft.serviceRate}
                      onChange={(event) => set("serviceRate", event.target.value)}
                    />
                    <span className="inputgroup__suffix">%</span>
                  </div>
                </Field>
              )}
            </Card>

            <Card
              title="The trading night"
              blurb="A club sells past midnight, so the calendar day is the wrong unit. A sale at 02:00 belongs to the night that started the evening before."
            >
              <Field
                label="A new trading night starts at"
                help="This single setting decides which night every sale, report and reconciliation belongs to."
              >
                <input
                  className="input input--num input--short"
                  type="time"
                  value={draft.dayStart}
                  onChange={(event) => set("dayStart", event.target.value)}
                />
              </Field>
              <Field
                label="and should be closed by"
                help="Only used to warn that a shift was left open. It never changes which night a sale belongs to."
              >
                <input
                  className="input input--num input--short"
                  type="time"
                  value={draft.dayEnd}
                  onChange={(event) => set("dayEnd", event.target.value)}
                />
              </Field>
              <Field label="A tab is identified by">
                <Segmented
                  label="A tab is identified by"
                  value={draft.referenceMode}
                  onChange={(next) => set("referenceMode", next)}
                  options={[
                    { value: "TABLE", label: "Table number" },
                    { value: "CUSTOMER_NAME", label: "Customer name" },
                    { value: "CUSTOMER_PHONE", label: "Phone" },
                    { value: "CUSTOM", label: "Something else" },
                  ]}
                />
              </Field>
            </Card>
          </>
        )}

        {step === 2 && (
          <Card
            title="Who uses it"
            blurb="Two accounts, because they do different jobs. The owner reads reports and changes settings; the cashier runs the till. Waiters do not sign in — they are named on tabs."
          >
            <Field label="Owner's name">
              <input
                className="input"
                value={draft.ownerName}
                autoFocus
                onChange={(event) => set("ownerName", event.target.value)}
              />
            </Field>
            <Field
              label="Owner's PIN"
              help="Four to eight digits. Avoid 1234 and 0000 — those are the first two anybody tries."
            >
              <input
                className="input input--num input--short"
                type="password"
                inputMode="numeric"
                maxLength={MAX_PIN}
                value={draft.ownerPin}
                onChange={(event) =>
                  set("ownerPin", event.target.value.replace(/\D/g, "").slice(0, MAX_PIN))
                }
              />
            </Field>
            <Field label="Cashier's name">
              <input
                className="input"
                value={draft.cashierName}
                onChange={(event) => set("cashierName", event.target.value)}
              />
            </Field>
            <Field label="Cashier's PIN" help="Must be different from the owner's.">
              <input
                className="input input--num input--short"
                type="password"
                inputMode="numeric"
                maxLength={MAX_PIN}
                value={draft.cashierPin}
                onChange={(event) =>
                  set("cashierPin", event.target.value.replace(/\D/g, "").slice(0, MAX_PIN))
                }
              />
            </Field>
          </Card>
        )}

        <div style={{ display: "flex", gap: 12 }}>
          {step > 0 && (
            <button
              type="button"
              className="btn btn--ghost"
              disabled={busy}
              onClick={() => setStep((current) => current - 1)}
            >
              Back
            </button>
          )}
          <div style={{ flex: 1 }} />
          {step < STEPS.length - 1 ? (
            <button
              type="button"
              className="btn btn--primary btn--big"
              disabled={!canContinue}
              onClick={() => setStep((current) => current + 1)}
            >
              Continue
            </button>
          ) : (
            <button
              type="button"
              className="btn btn--primary btn--big"
              disabled={!canContinue || busy}
              onClick={() => void finish()}
            >
              {busy ? "Setting up…" : "Finish setup"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
