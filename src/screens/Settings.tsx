/**
 * Settings — the screen the whole product hinges on.
 *
 * Nothing on it is written by hand. The groups, labels, help text and control
 * types all come from Rust, so a setting can never exist in the database
 * without a way to change it here. That was one of the named failures of the
 * previous build: problems that could not be fixed from the interface.
 *
 * The one arithmetic this file does is turning basis points into a percentage
 * for a text box and back again, which is a unit for a form control rather
 * than a money calculation. Every amount on the screen — including the live
 * example bill — arrives as text that Rust has already worked out.
 */

import { useEffect, useMemo, useState } from "react";

import {
  api,
  type BillPreview,
  type ServePointError,
  type SettingChange,
  type SettingField,
  type SettingsView,
} from "../api";
import { Banner, Card, Chip, Field, Loading, Segmented, Switch, Toast } from "../ui";

/** `1500` reads as `15`; `1250` reads as `12.5`. */
function rateToPercent(basisPoints: string): string {
  const value = Number(basisPoints);
  if (!Number.isFinite(value)) return "0";
  return String(value / 100);
}

function percentToRate(percent: string): string {
  const value = Number(percent.replace(",", ".").trim());
  if (!Number.isFinite(value) || value < 0) return "0";
  return String(Math.round(value * 100));
}

export function Settings() {
  const [view, setView] = useState<SettingsView>();
  const [edits, setEdits] = useState<Record<string, string>>({});
  const [error, setError] = useState<string>();
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .readSettings()
      .then(setView)
      .catch((raw) => setError((raw as ServePointError).message));
  }, []);

  const dirty = useMemo(() => Object.keys(edits), [edits]);

  if (error && !view) {
    return (
      <div className="page">
        <div className="page__inner">
          <Banner tone="bad" title="Settings could not be opened">
            {error}
          </Banner>
        </div>
      </div>
    );
  }

  if (!view) return <Loading what="Reading the settings…" />;

  const valueOf = (field: SettingField) => edits[field.key] ?? field.value;

  function change(key: string, value: string, original: string) {
    setEdits((current) => {
      const next = { ...current };
      // Typing something and then typing it back is not a change. Dropping it
      // here keeps the save bar honest about what is actually pending.
      if (value === original) delete next[key];
      else next[key] = value;
      return next;
    });
  }

  async function save() {
    setBusy(true);
    setError(undefined);
    const changes: SettingChange[] = dirty.map((key) => ({ key, value: edits[key] ?? "" }));
    try {
      setView(await api.writeSettings(changes));
      setEdits({});
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2600);
    } catch (raw) {
      setError((raw as ServePointError).message);
    } finally {
      setBusy(false);
    }
  }

  /** Keys a switched-off toggle is hiding. */
  const hidden = new Set<string>();
  for (const group of view.groups) {
    for (const field of group.fields) {
      if (field.kind === "toggle" && field.reveals.length > 0 && valueOf(field) !== "1") {
        for (const key of field.reveals) hidden.add(key);
      }
    }
  }

  return (
    <div className="page">
      <div className="page__inner">
        <div className="pagehead">
          <div className="pagehead__text">
            <span className="eyebrow">Settings</span>
            <h1>How this venue trades</h1>
            <p className="muted">
              Every one of these belongs to this venue and none of it is fixed in the program.
              Changes take effect on the next bill — never on one that has already been printed.
            </p>
          </div>
        </div>

        {error && (
          <Banner tone="bad" title="That was not saved">
            {error}
          </Banner>
        )}

        <div className="grid grid--halves" style={{ alignItems: "start" }}>
          <div style={{ display: "flex", flexDirection: "column", gap: 24 }}>
            {view.groups.map((group) => (
              <Card key={group.id} title={group.title} blurb={group.blurb || undefined}>
                {group.fields
                  .filter((field) => !hidden.has(field.key))
                  .map((field) => (
                    <Control
                      key={field.key}
                      field={field}
                      value={valueOf(field)}
                      onChange={(next) => change(field.key, next, field.value)}
                    />
                  ))}
              </Card>
            ))}
          </div>

          <div
            style={{ display: "flex", flexDirection: "column", gap: 24, position: "sticky", top: 0 }}
          >
            <Card
              title="What a bill looks like"
              blurb="Worked out by the same code that prices a real order, so this cannot drift from what actually prints."
            >
              <Preview preview={view.preview} pending={dirty.length > 0} />
            </Card>

            <Card
              title="The record"
              aside={
                <Chip tone={view.audit.intact ? "good" : "bad"}>
                  {view.audit.intact ? "Intact" : "Altered"}
                </Chip>
              }
            >
              <p style={{ fontWeight: 600 }}>{view.audit.headline}</p>
              <p className="field__help">{view.audit.detail}</p>
            </Card>
          </div>
        </div>

        {dirty.length > 0 && (
          <div className="savebar">
            <span className="savebar__text">
              {dirty.length === 1 ? "One change" : `${dirty.length} changes`} not saved yet.
            </span>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={busy}
              onClick={() => setEdits({})}
            >
              Discard
            </button>
            <button
              type="button"
              className="btn btn--primary"
              disabled={busy}
              onClick={() => void save()}
            >
              {busy ? "Saving…" : "Save changes"}
            </button>
          </div>
        )}
      </div>

      {saved && <Toast tone="good">Saved.</Toast>}
    </div>
  );
}

/* -------------------------------------------------------------------------- */

function Control({
  field,
  value,
  onChange,
}: {
  field: SettingField;
  value: string;
  onChange: (next: string) => void;
}) {
  switch (field.kind) {
    case "toggle":
      return (
        <div className="switchrow">
          <div className="switchrow__text">
            <span className="field__label">{field.label}</span>
            <span className="field__help">{field.help}</span>
          </div>
          <Switch
            label={field.label}
            checked={value === "1"}
            onChange={(next) => onChange(next ? "1" : "0")}
          />
        </div>
      );

    case "choice":
      return (
        <Field label={field.label} help={field.help}>
          <Segmented label={field.label} value={value} onChange={onChange} options={field.choices} />
        </Field>
      );

    case "rate":
      return (
        <Field label={field.label} help={field.help}>
          <div className="inputgroup">
            <input
              className="input input--num input--short"
              inputMode="decimal"
              value={rateToPercent(value)}
              onChange={(event) => onChange(percentToRate(event.target.value))}
            />
            <span className="inputgroup__suffix">%</span>
          </div>
        </Field>
      );

    case "integer":
      return (
        <Field label={field.label} help={field.help}>
          <input
            className="input input--num input--short"
            inputMode="numeric"
            value={value}
            onChange={(event) => onChange(event.target.value.replace(/\D/g, ""))}
          />
        </Field>
      );

    case "time":
      return (
        <Field label={field.label} help={field.help}>
          <input
            className="input input--num input--short"
            type="time"
            value={value}
            onChange={(event) => onChange(event.target.value)}
          />
        </Field>
      );

    case "multiline":
      return (
        <Field label={field.label} help={field.help}>
          <textarea
            className="textarea"
            value={value}
            onChange={(event) => onChange(event.target.value)}
          />
        </Field>
      );

    case "text":
      return (
        <Field label={field.label} help={field.help}>
          <input
            className="input"
            value={value}
            onChange={(event) => onChange(event.target.value)}
          />
        </Field>
      );
  }
}

function Preview({ preview, pending }: { preview: BillPreview; pending: boolean }) {
  return (
    <>
      {pending && (
        <Banner tone="warn">
          This still shows the saved settings. Save to see the change here.
        </Banner>
      )}
      <div className="slip">
        <div className="slip__row">
          <span>Drinks</span>
          <span>{preview.lineTotal}</span>
        </div>
        <hr className="slip__rule" />
        {preview.taxExtracted && (
          <div className="slip__row">
            <span>Subtotal</span>
            <span>{preview.net}</span>
          </div>
        )}
        {preview.showService && (
          <div className="slip__row">
            <span>{preview.serviceLabel}</span>
            <span>{preview.serviceCharge}</span>
          </div>
        )}
        {preview.showTax && (
          <div className="slip__row">
            <span>{preview.taxLabel}</span>
            <span>{preview.tax}</span>
          </div>
        )}
        <hr className="slip__rule" />
        <div className="slip__row slip__total">
          <span>Total</span>
          <span>{preview.total}</span>
        </div>
      </div>
      {preview.taxExtracted && (
        <p className="field__help">
          Menu prices already include VAT, so the subtotal is lower than the drinks total — the tax
          was inside the price all along, and it is itemised because it has to be.
        </p>
      )}
      {!preview.showTax && !preview.showService && (
        <p className="field__help">Nothing is added. Customers pay exactly what is on the menu.</p>
      )}
    </>
  );
}
