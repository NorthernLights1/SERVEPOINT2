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

/** The sentence to show somebody when a command was refused. */
export function reason(error: unknown): string {
  return error instanceof ServePointError
    ? error.message
    : "Something went wrong and the reason was not readable.";
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

/* -------------------------------------------------------------------------- */
/* The till                                                                    */
/* -------------------------------------------------------------------------- */

export interface TabLine {
  id: number;
  code: string;
  /** How this venue refers to the tab — "Table 7", a name, a reference. */
  label: string;
  waiter: string;
  runningTotal: string;
}

export interface WaiterLine {
  id: number;
  name: string;
}

export interface MenuLine {
  saleItemId: number;
  name: string;
  category: string;
  price: string;
}

export interface FloorView {
  shift: ShiftView | null;
  tabs: TabLine[];
  waiters: WaiterLine[];
  menu: MenuLine[];
  tabPrompt: string;
  /** True when this venue also wants a phone beside the customer's name. */
  wantsContact: boolean;
}

export interface StockLine {
  productId: number;
  name: string;
  category: string;
  unit: string;
  onHand: string;
  value: string;
  low: boolean;
  tracked: boolean;
}

/** One past delivery. The batch is the record's own id — nobody types one. */
export interface DeliveryLine {
  batch: number;
  name: string;
  quantity: string;
  unit: string;
  cost: string;
  received: string;
}

export interface InventoryView {
  lines: StockLine[];
  totalValue: string;
  /** What came in recently, newest first. */
  deliveries: DeliveryLine[];
}

/** A crate arriving. The per-unit rate is derived from the total, never typed. */
export interface DeliveryForm {
  productId: number;
  quantityMilli: number;
  totalCost: string;
}

export interface SlipLine {
  receiptNumber: string;
  destination: string;
  text: string;
}

export interface PlacedOrder {
  orderId: number;
  slips: SlipLine[];
  tabTotal: string;
}

export interface OrderLineInput {
  saleItemId: number;
  quantityMilli: number;
}

export interface CorrectableOrderLine {
  lineId: number;
  saleItemId: number;
  name: string;
  quantityMilli: number;
  quantity: string;
  unitPrice: string;
  lineTotal: string;
}

export interface CorrectionProductChange {
  productId: number;
  name: string;
  unit: string;
  before: string;
  after: string;
  maximumReturnMilli: number;
  maximumReturn: string;
}

export interface CorrectableOrderView {
  orderId: number;
  originalTotal: string;
  lines: CorrectableOrderLine[];
  voidChanges: CorrectionProductChange[];
}

export interface CorrectionLineInput {
  originalLineId: number | null;
  saleItemId: number;
  quantityMilli: number;
}

export interface CorrectionPreview {
  originalTotal: string;
  replacementTotal: string;
  productChanges: CorrectionProductChange[];
}

export interface ReturnInput {
  productId: number;
  returnedMilli: number;
  note: string;
}

export interface CorrectionResult {
  view: FloorView;
  slips: SlipLine[];
}

/** An order whose paper never came out. Its numbers are already burned. */
export interface StrandedPrint {
  orderId: number;
  tabLabel: string;
  waiter: string;
  receiptNumbers: string[];
  rangAt: number;
}

export interface RecoveryView {
  prints: StrandedPrint[];
}

/** What the customer is about to be asked for, before anything is frozen. */
export interface BillView {
  tabId: number;
  code: string;
  label: string;
  waiter: string;
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
  compsEnabled: boolean;
  asksCustomerTin: boolean;
}

/** The frozen bill, read back after the tab has closed. */
export interface SettledBill {
  tabId: number;
  code: string;
  label: string;
  waiter: string;
  subtotal: string;
  serviceCharge: string;
  tax: string;
  total: string;
  liability: string;
  comped: boolean;
  compReason: string | null;
}

/* -------------------------------------------------------------------------- */
/* What the venue is made of                                                   */
/* -------------------------------------------------------------------------- */

export type BaseUnit = "BOTTLE" | "SHOT" | "UNIT";
export type Destination = "BAR" | "KITCHEN";

export interface StaffLine {
  id: number;
  code: string;
  name: string;
  role: "OWNER" | "CASHIER" | "WAITER";
  active: boolean;
}

export interface ProductLine {
  id: number;
  /** The menu entry selling this one for one, when there is one. */
  saleItemId: number | null;
  /** What it sells for, already formatted. Null when it is stock only. */
  price: string | null;
  /**
   * The same amount unformatted — "1200.00" — which is what Rust reads back.
   * An edit box filled from `price` would send "1,200.00 ETB" and be refused.
   */
  priceValue: string | null;
  contentMeasure: Measure;
  /** How much one counted unit holds, formatted: "750". Empty when none. */
  contentPerUnit: string;
  code: string;
  name: string;
  category: string;
  baseUnit: string;
  baseUnitsPerPack: string;
  unitsPerPurchasePack: number;
  lowStockThreshold: string;
  tracksInventory: boolean;
  destination: string;
  active: boolean;
  onHand: string;
}

export interface RecipeLineView {
  productId: number;
  measure: Measure;
  /** The amount read back in that measure — "30" for 30ml. Empty when none. */
  measureQuantity: string;
  name: string;
  quantityMilli: number;
  quantity: string;
}

export interface SaleItemLine {
  id: number;
  /**
   * Set when this entry is one shelf item sold one for one. The Items card
   * already shows it, so the composed card leaves it out.
   */
  fromProductId: number | null;
  code: string;
  name: string;
  category: string;
  active: boolean;
  /** Null until somebody prices it. */
  price: string | null;
  /** The same amount unformatted, for an edit box. See {@link ProductLine}. */
  priceValue: string | null;
  recipe: RecipeLineView[];
  sellable: boolean;
}

export interface SetupView {
  staff: StaffLine[];
  products: ProductLine[];
  saleItems: SaleItemLine[];
  canTrade: boolean;
  /** What is still missing, in the order it needs doing. */
  missing: string[];
}

/**
 * Codes are absent on purpose. The till allocates PRD-, ITM- and STF- numbers
 * itself, the same way it allocates BR- numbers, so nothing on this side of
 * the boundary invents one.
 */
export interface StaffForm {
  name: string;
  role: string;
  pin?: string | null;
}

export interface ProductForm {
  /**
   * What it sells for, as typed: "120.00". Omitted or blank means stock only —
   * on the shelf, never ordered by name.
   */
  salePrice?: string | null;
  /** What one counted unit holds, so recipes can be written in ml or grams. */
  contentMeasure?: Measure;
  /** Thousandths of that measure in one counted unit: 750000 for a 750ml bottle. */
  contentPerUnitMilli?: number;
  name: string;
  category: string;
  baseUnit: BaseUnit;
  baseUnitsPerPack: number;
  unitsPerPurchasePack: number;
  lowStockThreshold: number;
  tracksInventory: boolean;
  destination: Destination;
  active?: boolean;
}

export interface SaleItemForm {
  name: string;
  category: string;
  active?: boolean;
}

export type Measure = "NONE" | "ML" | "GRAM";

export interface RecipeLineForm {
  productId: number;
  /**
   * Counted units, or the product's own measure when inMeasure is set:
   * 30000 with inMeasure means 30ml. Rust does the division, not this side.
   */
  quantityMilli: number;
  inMeasure?: boolean;
}


/* -------------------------------------------------------------------------- */
/* End of the night                                                            */
/* -------------------------------------------------------------------------- */

export type SettleMethod = "CASH" | "NON_CASH" | "WRITE_OFF";

export interface TabDue {
  tabId: number;
  code: string;
  label: string;
  amount: string;
}

export interface WaiterSettlement {
  waiterId: number;
  name: string;
  tabs: TabDue[];
  /** What settling right now would clear. */
  due: string;
  /** Everything this waiter still holds, including any earlier shortfall. */
  held: string;
  owesAnything: boolean;
  /** A shortfall carried in from a previous night, with no tabs behind it. */
  oldBalance: boolean;
}

export interface NamedAmount {
  name: string;
  amount: string;
}

export interface NamedQuantity {
  name: string;
  quantity: string;
}

export interface LowStockLine {
  name: string;
  onHand: string;
  unit: string;
}

export interface OverviewView {
  shift: ShiftView | null;
  /** Billed on tabs settled tonight. */
  settled: string;
  /** Still running on tabs nobody has been asked to pay. Never added to the above. */
  openTabs: string;
  tabsSettled: number;
  tabsOpen: number;
  topWaiter: NamedAmount | null;
  topSellers: NamedQuantity[];
  lowStock: LowStockLine[];
  quiet: boolean;
}

export interface ReportWaiterLine {
  name: string;
  expected: string;
  cash: string;
  nonCash: string;
  writtenOff: string;
  shortfall: string;
}

export interface ReportItemLine {
  name: string;
  quantity: string;
  value: string;
}

/**
 * A night, frozen at the moment it closed.
 *
 * This is the one view in the system that is *not* recalculated on read. It is
 * stored when the night is sealed and handed back unchanged for ever after, so
 * what this screen shows is what was signed. Every figure below arrived
 * already formatted, as usual — but here it was formatted on the night in
 * question, not today.
 */
export interface ShiftReport {
  venueName: string;
  shiftCode: string;
  businessDate: string;
  businessDateLabel: string;
  openedAtLabel: string;
  openedBy: string;
  closedAtLabel: string;
  closedBy: string;

  tabsSettled: number;
  grossSales: string;
  serviceCharge: string;
  tax: string;
  totalBilled: string;

  openingFloat: string;
  cashFromWaiters: string;
  otherMovements: string;
  expectedCash: string;
  countedCash: string;
  /** The difference as a positive figure — `over` says which way. */
  variance: string;
  over: boolean;
  balanced: boolean;

  nonCash: string;
  writtenOff: string;

  waiters: ReportWaiterLine[];
  items: ReportItemLine[];

  compedTabs: number;
  compedValue: string;
  corrections: number;
  voids: number;
}

export interface ReportNight {
  shiftId: number;
  code: string;
  businessDate: string;
  businessDateLabel: string;
}

export interface ReportsView {
  nights: ReportNight[];
  /** The night being read. Null only when no night has ever closed. */
  showing: ShiftReport | null;
  showingShiftId: number | null;
  /** The exact stored paper, for reading what was signed. */
  renderedText: string | null;
}

/** What the night came to, once the drawer has been counted. */
export interface ClosedNight {
  code: string;
  businessDateLabel: string;
  expected: string;
  counted: string;
  /** The difference as a positive figure — `over` says which way. */
  variance: string;
  over: boolean;
  balanced: boolean;
}

export interface ReconciliationView {
  shift: ShiftView | null;
  waiters: WaiterSettlement[];
  expectedCash: string;
  canBeginClosing: boolean;
  /** Why the night cannot begin closing. Null once it can. */
  blocker: string | null;
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

  floorView: () => call<FloorView>("cmd_floor_view"),
  inventoryView: () => call<InventoryView>("cmd_inventory_view"),
  receiveDelivery: (form: DeliveryForm) =>
    call<InventoryView>("cmd_receive_delivery", { form }),
  openShift: (openingFloat: string) => call<FloorView>("cmd_open_shift", { openingFloat }),
  openTab: (waiterId: number, reference: string, contact?: string) =>
    call<FloorView>("cmd_open_tab", { waiterId, reference, contact: contact ?? null }),
  placeOrder: (tabId: number, lines: OrderLineInput[]) =>
    call<PlacedOrder>("cmd_place_order", { tabId, lines }),
  correctionOrder: (tabId: number, receiptNumber: string) =>
    call<CorrectableOrderView>("cmd_correction_order", { tabId, receiptNumber }),
  previewCorrection: (orderId: number, lines: CorrectionLineInput[]) =>
    call<CorrectionPreview>("cmd_preview_correction", { orderId, lines }),
  correctOrder: (
    orderId: number,
    receiptNumber: string,
    reason: string,
    lines: CorrectionLineInput[],
    returns: ReturnInput[],
  ) =>
    call<CorrectionResult>("cmd_correct_order", {
      orderId,
      receiptNumber,
      reason,
      lines,
      returns,
    }),
  voidOrder: (orderId: number, receiptNumber: string, reason: string, returns: ReturnInput[]) =>
    call<FloorView>("cmd_void_order", { orderId, receiptNumber, reason, returns }),
  tabBill: (tabId: number) => call<BillView>("cmd_tab_bill", { tabId }),
  settleTab: (tabId: number, compReason?: string, customerTin?: string) =>
    call<SettledBill>("cmd_settle_tab", {
      tabId,
      compReason: compReason ?? null,
      customerTin: customerTin ?? null,
    }),
  recoveryView: () => call<RecoveryView>("cmd_recovery_view"),
  resolveHandwritten: (orderId: number) =>
    call<RecoveryView>("cmd_resolve_handwritten", { orderId }),
  resolveNonPrint: (orderId: number) => call<RecoveryView>("cmd_resolve_non_print", { orderId }),

  overviewView: () => call<OverviewView>("cmd_overview_view"),
  /** Omit the night to read the most recent one. */
  reportsView: (shiftId?: number) =>
    call<ReportsView>("cmd_reports_view", { shiftId: shiftId ?? null }),
  reconciliationView: () => call<ReconciliationView>("cmd_reconciliation_view"),
  settleWaiter: (waiterId: number, method: SettleMethod, amount: string, reason?: string) =>
    call<ReconciliationView>("cmd_settle_waiter", {
      waiterId,
      method,
      amount,
      reason: reason ?? null,
    }),
  beginClosing: () => call<ReconciliationView>("cmd_begin_closing"),
  closeNight: (countedCash: string) => call<ClosedNight>("cmd_close_night", { countedCash }),

  setupView: () => call<SetupView>("cmd_setup_view"),
  addStaff: (form: StaffForm) => call<SetupView>("cmd_add_staff", { form }),
  setStaffActive: (staffId: number, active: boolean) =>
    call<SetupView>("cmd_set_staff_active", { staffId, active }),
  addProduct: (form: ProductForm) => call<SetupView>("cmd_add_product", { form }),
  sellProduct: (productId: number, price: string) =>
    call<SetupView>("cmd_sell_product", { productId, price }),
  editProduct: (productId: number, form: ProductForm) =>
    call<SetupView>("cmd_edit_product", { productId, form }),
  addSaleItem: (form: SaleItemForm) => call<SetupView>("cmd_add_sale_item", { form }),
  editSaleItem: (saleItemId: number, form: SaleItemForm) =>
    call<SetupView>("cmd_edit_sale_item", { saleItemId, form }),
  setRecipe: (saleItemId: number, lines: RecipeLineForm[]) =>
    call<SetupView>("cmd_set_recipe", { saleItemId, lines }),
  /** Take a shelf item off the catalogue, or bring it back. Never a delete. */
  setProductActive: (productId: number, active: boolean) =>
    call<SetupView>("cmd_set_product_active", { productId, active }),
  /** Take a drink off the menu, or put it back. Never a delete. */
  setSaleItemActive: (saleItemId: number, active: boolean) =>
    call<SetupView>("cmd_set_sale_item_active", { saleItemId, active }),
  setPrice: (saleItemId: number, price: string) =>
    call<SetupView>("cmd_set_price", { saleItemId, price }),
};
