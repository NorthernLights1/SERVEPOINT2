/**
 * The only door between this window and the application.
 *
 * Every money figure below is a **string**, already formatted by Rust. That is
 * deliberate and it is the rule this file exists to enforce: nothing here adds,
 * multiplies, rounds or formats an amount. A total worked out in JavaScript is
 * a total that will one day disagree with the printed receipt, and the receipt
 * is the thing the customer is holding.
 *
 * If a screen needs a number it does not have, the answer is a new field on a
 * Rust view — never arithmetic in a component.
 */

import { invoke } from "@tauri-apps/api/core";

export type Role = "OWNER" | "CASHIER";

export interface Session {
  staffId: number;
  code: string;
  name: string;
  role: Role;
}

export interface VenueView {
  name: string;
  address: string;
  phone: string;
  tin: string;
  currencyCode: string;
  configured: boolean;
}

export interface ShiftView {
  id: number;
  code: string;
  businessDate: string;
  businessDateLabel: string;
  openedBy: string;
  overdue: boolean;
}

export interface AccountView {
  staffId: number;
  name: string;
  role: Role;
}

export interface BootstrapView {
  setupCompleted: boolean;
  session: Session | null;
  venue: VenueView;
  accounts: AccountView[];
  tabPrompt: string;
  businessDate: string;
  businessDateLabel: string;
  openShift: ShiftView | null;
  schemaVersion: number;
}

export interface BillPreview {
  lineTotal: string;
  net: string;
  serviceCharge: string;
  tax: string;
  total: string;
  serviceLabel: string;
  taxLabel: string;
  showService: boolean;
  showTax: boolean;
  taxExtracted: boolean;
}

export interface AuditView {
  intact: boolean;
  entries: number;
  headline: string;
  detail: string;
}

export type FieldKind =
  | "toggle"
  | "rate"
  | "integer"
  | "time"
  | "text"
  | "multiline"
  | "choice";

export interface Choice {
  value: string;
  label: string;
}

export interface SettingField {
  key: string;
  value: string;
  kind: FieldKind;
  label: string;
  help: string;
  choices: Choice[];
  reveals: string[];
}

export interface SettingGroup {
  id: string;
  title: string;
  blurb: string;
  fields: SettingField[];
}

export interface SettingsView {
  groups: SettingGroup[];
  preview: BillPreview;
  audit: AuditView;
}

export interface SettingChange {
  key: string;
  value: string;
}

export interface SetupRequest {
  ownerName: string;
  ownerPin: string;
  cashierName: string;
  cashierPin: string;
  changes: SettingChange[];
}

/** What every failed command produces. `kind` is for branching, `message` is for people. */
export class ServePointError extends Error {
  readonly kind: string;

  constructor(kind: string, message: string) {
    super(message);
    this.name = "ServePointError";
    this.kind = kind;
  }
}

/** True when this page is running in a plain browser rather than the desktop app. */
export const inDesktopApp = "__TAURI_INTERNALS__" in window;

function normalise(raw: unknown): ServePointError {
  if (raw instanceof ServePointError) return raw;
  if (typeof raw === "object" && raw !== null && "kind" in raw && "message" in raw) {
    const { kind, message } = raw as { kind: unknown; message: unknown };
    return new ServePointError(String(kind), String(message));
  }
  // An error shape we did not design for. Say so plainly rather than
  // rendering "[object Object]" at somebody standing behind a bar.
  return new ServePointError(
    "UNKNOWN",
    typeof raw === "string" ? raw : "Something went wrong and the reason was not readable.",
  );
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!inDesktopApp) {
    throw new ServePointError(
      "NO_BACKEND",
      "This page is open in a browser. ServePoint runs as a desktop application — start it with `npm run app`.",
    );
  }
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    throw normalise(raw);
  }
}

export const api = {
  bootstrap: () => call<BootstrapView>("cmd_bootstrap"),
  signIn: (staffId: number, pin: string) => call<Session>("cmd_sign_in", { staffId, pin }),
  signOut: () => call<void>("cmd_sign_out"),
  completeSetup: (request: SetupRequest) => call<Session>("cmd_complete_setup", { request }),
  readSettings: () => call<SettingsView>("cmd_read_settings"),
  writeSettings: (changes: SettingChange[]) =>
    call<SettingsView>("cmd_write_settings", { changes }),
  verifyAudit: () => call<AuditView>("cmd_verify_audit"),
};
