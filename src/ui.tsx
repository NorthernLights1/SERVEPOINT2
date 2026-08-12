/**
 * The parts every screen is built from.
 *
 * Nothing here knows what a tab or a shift is — these are shapes and controls,
 * and they take text that Rust has already written. Keeping them ignorant is
 * what stops a currency symbol or a rounding rule quietly appearing in the
 * webview.
 */

import type { ReactNode } from "react";

export type Tone = "quiet" | "good" | "warn" | "bad" | "info";

/**
 * An optional prop that may also be passed explicitly as `undefined`.
 *
 * `exactOptionalPropertyTypes` is on, which normally means "omitted" and
 * "present but undefined" are different things. For presentational props they
 * are not — a card with no blurb and a card whose blurb is undefined look
 * identical — and forcing every caller to build props conditionally would add
 * noise for no benefit.
 */
type Optional<T> = T | undefined;

/* -------------------------------------------------------------------------- */

export function Card({
  title,
  blurb,
  aside,
  flush,
  children,
}: {
  title?: Optional<string>;
  blurb?: Optional<string>;
  aside?: Optional<ReactNode>;
  flush?: Optional<boolean>;
  children: ReactNode;
}) {
  return (
    <section className="card">
      {title && (
        <header className="card__head">
          <div>
            <h2>{title}</h2>
            {blurb && <p className="field__help">{blurb}</p>}
          </div>
          {aside}
        </header>
      )}
      <div className={flush ? "card__body card__body--flush" : "card__body"}>{children}</div>
    </section>
  );
}

export function Stat({
  label,
  value,
  note,
}: {
  label: string;
  value: string;
  note?: Optional<string>;
}) {
  return (
    <div className="stat">
      <span className="eyebrow">{label}</span>
      <span className="stat__value num">{value}</span>
      {note && <span className="stat__note">{note}</span>}
    </div>
  );
}

export function Chip({ tone = "quiet", children }: { tone?: Tone; children: ReactNode }) {
  const cls = tone === "info" ? "quiet" : tone;
  return (
    <span className={`chip chip--${cls}`}>
      {tone !== "quiet" && tone !== "info" && <span className="dot" />}
      {children}
    </span>
  );
}

const BANNER_GLYPH: Record<Tone, string> = {
  quiet: "•",
  info: "i",
  good: "✓",
  warn: "!",
  bad: "✕",
};

export function Banner({
  tone = "info",
  title,
  children,
  action,
}: {
  tone?: Optional<Tone>;
  title?: Optional<string>;
  children: ReactNode;
  action?: Optional<ReactNode>;
}) {
  const cls = tone === "quiet" ? "info" : tone;
  return (
    <div className={`banner banner--${cls}`}>
      <span className="banner__glyph" aria-hidden="true">
        {BANNER_GLYPH[tone]}
      </span>
      <div className="banner__text">
        {title && <span className="banner__title">{title}</span>}
        <span>{children}</span>
      </div>
      {action}
    </div>
  );
}

export function Empty({
  glyph = "○",
  title,
  children,
  action,
}: {
  glyph?: Optional<string>;
  title: string;
  children: ReactNode;
  action?: Optional<ReactNode>;
}) {
  return (
    <div className="empty">
      <span className="empty__glyph" aria-hidden="true">
        {glyph}
      </span>
      <span className="empty__title">{title}</span>
      <p className="empty__body">{children}</p>
      {action}
    </div>
  );
}

export function Loading({ what }: { what: string }) {
  return (
    <div className="loading">
      <div>
        <div className="spinner" />
        <span>{what}</span>
      </div>
    </div>
  );
}

/* -------------------------------------------------------------------------- */

export function Switch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      className="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
    />
  );
}

export function SwitchRow({
  label,
  help,
  checked,
  onChange,
}: {
  label: string;
  help: string;
  checked: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <div className="switchrow">
      <div className="switchrow__text">
        <span className="field__label">{label}</span>
        <span className="field__help">{help}</span>
      </div>
      <Switch checked={checked} onChange={onChange} label={label} />
    </div>
  );
}

export function Segmented({
  value,
  options,
  onChange,
  label,
}: {
  value: string;
  options: { value: string; label: string }[];
  onChange: (next: string) => void;
  label: string;
}) {
  return (
    <div className="seg" role="group" aria-label={label}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          className="seg__opt"
          aria-pressed={option.value === value}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function Field({
  label,
  help,
  error,
  children,
}: {
  label: string;
  help?: Optional<string>;
  error?: Optional<string>;
  children: ReactNode;
}) {
  return (
    <label className="field">
      <span className="field__label">{label}</span>
      {children}
      {error ? (
        <span className="field__error">{error}</span>
      ) : (
        help && <span className="field__help">{help}</span>
      )}
    </label>
  );
}

/* -------------------------------------------------------------------------- */

/**
 * A numeric keypad, because a till is often a touchscreen and because a PIN
 * typed on a full keyboard is a PIN read over a shoulder.
 *
 * The dots show length and nothing else — never the digits.
 */
export function PinPad({
  length,
  max,
  onDigit,
  onBackspace,
  onSubmit,
  disabled,
}: {
  length: number;
  max: number;
  onDigit: (digit: string) => void;
  onBackspace: () => void;
  onSubmit: () => void;
  disabled?: Optional<boolean>;
}) {
  return (
    <>
      <div className="pindots" aria-label={`${length} digits entered`}>
        {Array.from({ length: max }, (_, index) => (
          <span key={index} className={index < length ? "pindot pindot--on" : "pindot"} />
        ))}
      </div>
      <div className="keypad">
        {["1", "2", "3", "4", "5", "6", "7", "8", "9"].map((digit) => (
          <button
            key={digit}
            type="button"
            className="key"
            disabled={disabled}
            onClick={() => onDigit(digit)}
          >
            {digit}
          </button>
        ))}
        <button
          type="button"
          className="key key--quiet"
          disabled={disabled || length === 0}
          onClick={onBackspace}
        >
          Delete
        </button>
        <button type="button" className="key" disabled={disabled} onClick={() => onDigit("0")}>
          0
        </button>
        <button
          type="button"
          className="key key--quiet"
          disabled={disabled || length < 4}
          onClick={onSubmit}
        >
          Enter
        </button>
      </div>
    </>
  );
}

export function Toast({ tone, children }: { tone: "good" | "bad"; children: ReactNode }) {
  return (
    <div className={`toast toast--${tone}`} role="status">
      {children}
    </div>
  );
}
