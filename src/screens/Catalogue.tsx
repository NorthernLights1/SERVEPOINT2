/**
 * What the venue is made of — the screen a new venue is commissioned on.
 *
 * Three things in the order they depend on each other: what you buy, what you
 * sell, and who carries it. Nothing here computes a price; every figure shown
 * was formatted by Rust and every figure typed is sent as text for Rust to
 * read.
 */

import { useState } from "react";
import { useEffect } from "react";

import type {
  BaseUnit,
  Destination,
  Measure,
  ProductForm,
  ProductLine,
  SaleItemLine,
  SetupView,
} from "../api";
import { api, ServePointError } from "../api";
import { Banner, Card, Chip, Empty, Field, Loading } from "../ui";

const MILLI = 1000;

function reason(error: unknown): string {
  return error instanceof ServePointError
    ? error.message
    : "Something went wrong and the reason was not readable.";
}

/** Whole units as thousandths. A unit conversion, not a calculation. */
function toMilli(text: string): number {
  const units = Number(text);
  return Number.isFinite(units) ? Math.round(units * MILLI) : 0;
}

type Section = {
  view: SetupView;
  busy: boolean;
  run: (work: () => Promise<SetupView>) => Promise<boolean>;
};

export function Catalogue() {
  const [view, setView] = useState<SetupView>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

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
          <Banner tone="bad" title="The catalogue could not be read">
            {error}
          </Banner>
        </div>
      </div>
    ) : (
      <Loading what="the catalogue" />
    );
  }

  return (
    <div className="page">
      <div className="page__inner">
        <div className="pagehead">
          <div className="pagehead__text">
            <span className="eyebrow">Catalogue</span>
            <h1>What this venue is made of</h1>
            <p className="muted">
              A bottle on the shelf and a drink on the menu are different things. A recipe converts
              between them, and that separation is what makes the stock come out right.
            </p>
          </div>
          <Chip tone={view.canTrade ? "good" : "warn"}>
            {view.canTrade ? "Ready to trade" : "Not ready"}
          </Chip>
        </div>

        {error && (
          <Banner tone="bad" title="That was refused">
            {error}
          </Banner>
        )}

        {view.missing.length > 0 && (
          <Banner tone="warn" title="The till cannot serve anybody yet">
            <ul>
              {view.missing.map((step) => (
                <li key={step}>{step}</li>
              ))}
            </ul>
          </Banner>
        )}

        <Products view={view} busy={busy} run={run} />
        <SaleItems view={view} busy={busy} run={run} />
        <Staff view={view} busy={busy} run={run} />
      </div>
    </div>
  );
}

/* -------------------------------------------------------------------------- */

/**
 * A category is chosen from the ones already in use, or typed to start a new
 * one.
 *
 * `<datalist>` is the browser's own combobox: it filters as you type, works
 * from the keyboard, and still accepts a value that is not on the list — which
 * is exactly what is wanted, because the first drink of a new category has to
 * be able to name it. Typing a category that already exists is what produced
 * "Whiskey", "whiskey" and "Whisky" as three separate groups.
 */
function CategoryField({
  id,
  value,
  used,
  help,
  onChange,
}: {
  id: string;
  value: string;
  used: string[];
  help: string;
  onChange: (next: string) => void;
}) {
  const known = [...new Set(used.map((one) => one.trim()).filter(Boolean))].sort((a, b) =>
    a.localeCompare(b),
  );
  return (
    <Field label="Category" help={help}>
      <input
        list={id}
        value={value}
        autoComplete="off"
        placeholder={known.length > 0 ? "Choose or type a new one" : "Type the first one"}
        onChange={(event) => onChange(event.target.value)}
      />
      <datalist id={id}>
        {known.map((one) => (
          <option key={one} value={one} />
        ))}
      </datalist>
    </Field>
  );
}

function Products({ view, busy, run }: Section) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [category, setCategory] = useState("");
  const [unit, setUnit] = useState<"BOTTLE" | "SHOT" | "UNIT">("BOTTLE");
  const [threshold, setThreshold] = useState("3");
  const [destination, setDestination] = useState<"BAR" | "KITCHEN">("BAR");
  const [editing, setEditing] = useState<ProductLine>();
  // A club sells bottles, so the common case is the default: what goes on the
  // shelf is what you sell. Untick it for a mixer nobody orders by name.
  const [onMenu, setOnMenu] = useState(true);
  const [price, setPrice] = useState("");
  // What one counted unit holds. Left as NONE for anything handed over whole,
  // which is most of a club's list.
  const [measure, setMeasure] = useState<Measure>("NONE");
  const [perUnit, setPerUnit] = useState("750");

  return (
    <Card
      title="Items"
      blurb="What sits on the shelf, and what it sells for. For almost everything a club pours those are the same thing."
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
            <CategoryField
              id="product-categories"
              value={category}
              used={view.products.map((product) => product.category)}
              help="How the shelf is grouped — Whiskey, Beer, Mixers."
              onChange={setCategory}
            />
            <Field label="Counted in">
              <select value={unit} onChange={(event) => setUnit(event.target.value as typeof unit)}>
                <option value="BOTTLE">Bottles</option>
                <option value="SHOT">Shots</option>
                <option value="UNIT">Units</option>
              </select>
            </Field>
            <Field
              label="What one contains"
              help="Set this only when it is poured or weighed out. A whole bottle handed over needs nothing here."
            >
              <select
                value={measure}
                onChange={(event) => setMeasure(event.target.value as Measure)}
              >
                <option value="NONE">Sold whole — nothing to measure</option>
                <option value="ML">Millilitres</option>
                <option value="GRAM">Grams</option>
              </select>
            </Field>
            {measure !== "NONE" && (
              <Field
                label={measure === "ML" ? "Millilitres in one" : "Grams in one"}
                help="A 750ml bottle is 750. Recipes are then written in that measure."
              >
                <input
                  inputMode="decimal"
                  value={perUnit}
                  onChange={(event) => setPerUnit(event.target.value)}
                />
              </Field>
            )}
            <Field label="Warn below" help="When this many are left, it is flagged as low.">
              <input
                inputMode="decimal"
                value={threshold}
                onChange={(event) => setThreshold(event.target.value)}
              />
            </Field>
            <Field label="Handed over at">
              <select
                value={destination}
                onChange={(event) => setDestination(event.target.value as typeof destination)}
              >
                <option value="BAR">The bar</option>
                <option value="KITCHEN">The kitchen</option>
              </select>
            </Field>
            {onMenu && (
              <Field
                label="Sale price"
                help="What one costs the customer. What you paid is recorded when you count it in."
              >
                <input
                  inputMode="decimal"
                  value={price}
                  onChange={(event) => setPrice(event.target.value)}
                />
              </Field>
            )}
          </div>

          <label className="check">
            <input
              type="checkbox"
              checked={onMenu}
              onChange={(event) => setOnMenu(event.target.checked)}
            />
            <span>
              Sell this on the menu
              <span className="field__help">
                Leave it ticked for anything ordered by name. Untick it for something poured into
                other drinks, like a mixer.
              </span>
            </span>
          </label>

          <button
            type="button"
            className="btn btn--primary"
            disabled={busy || name.trim() === "" || (onMenu && price.trim() === "")}
            onClick={async () => {
              const ok = await run(() =>
                api.addProduct({
                  salePrice: onMenu ? price : null,
                  contentMeasure: measure,
                  contentPerUnitMilli: measure === "NONE" ? 0 : toMilli(perUnit),
                  name,
                  category,
                  baseUnit: unit,
                  // Both are inert: nothing in the codebase multiplies by
                  // either, and "what one contains" above is the field that
                  // actually converts. Sent as one so the CHECKs hold.
                  baseUnitsPerPack: MILLI,
                  unitsPerPurchasePack: 1,
                  lowStockThreshold: toMilli(threshold),
                  tracksInventory: true,
                  destination,
                }),
              );
              if (ok) {
                setName("");
                setPrice("");
                setOnMenu(true);
                setMeasure("NONE");
                setOpen(false);
              }
            }}
          >
            Add it
          </button>
        </>
      )}

      {view.products.length === 0 ? (
        <Empty glyph="▦" title="Nothing on the shelf yet">
          Start with a bottle of something. Ticking "sell this on the menu" puts it on the till at
          the same time.
        </Empty>
      ) : (
        <ul className="rows">
          {view.products.map((product) => (
            <li key={product.id} className="row row--static">
              <span className="row__main">
                <strong>{product.name}</strong>
                <span className="muted">
                  {product.code} · {product.category} · {product.onHand} on the shelf
                  {product.contentMeasure !== "NONE" &&
                    ` · ${product.contentPerUnit}${product.contentMeasure === "ML" ? "ml" : "g"} each`}
                </span>
              </span>
              <span className="row__value">
                {!product.active && <Chip tone="quiet">Removed</Chip>}
                {product.price ?? <Chip tone="quiet">Not sold</Chip>}
                {product.active && (
                  <button
                    type="button"
                    className="btn"
                    onClick={() => setEditing(editing?.id === product.id ? undefined : product)}
                  >
                    {product.saleItemId === null ? "Sell it" : "Edit"}
                  </button>
                )}
                <button
                  type="button"
                  className={product.active ? "btn btn--danger" : "btn"}
                  disabled={busy}
                  onClick={() => run(() => api.setProductActive(product.id, !product.active))}
                >
                  {product.active ? "Remove" : "Bring back"}
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}

      {editing && (
        <EditProduct
          key={editing.id}
          product={editing}
          busy={busy}
          run={run}
          onDone={() => setEditing(undefined)}
        />
      )}
    </Card>
  );
}

/**
 * The form Rust wants, rebuilt from the line it sent.
 *
 * Everything except the name is handed straight back, so an edit that changes
 * one field cannot quietly restate the rest — and `sale_price` is left out
 * because an update ignores it. Pricing is its own command, further down.
 */
function formOf(product: ProductLine, name: string): ProductForm {
  return {
    name,
    category: product.category,
    baseUnit: product.baseUnit as BaseUnit,
    baseUnitsPerPack: toMilli(product.baseUnitsPerPack),
    unitsPerPurchasePack: product.unitsPerPurchasePack,
    lowStockThreshold: toMilli(product.lowStockThreshold),
    tracksInventory: product.tracksInventory,
    destination: product.destination as Destination,
    contentMeasure: product.contentMeasure,
    contentPerUnitMilli:
      product.contentMeasure === "NONE" ? 0 : toMilli(product.contentPerUnit),
    active: product.active,
  };
}

/**
 * Rename something on the shelf, and say what it sells for.
 *
 * The same panel does both jobs a shelf item can need. For one that is not on
 * the menu yet, typing a price is what puts it there — which is why the row's
 * button still says "Sell it" until it has one.
 */
function EditProduct({
  product,
  busy,
  run,
  onDone,
}: {
  product: ProductLine;
  busy: boolean;
  run: Section["run"];
  onDone: () => void;
}) {
  const [name, setName] = useState(product.name);
  // Filled from the plain figure, never the formatted one: "1,200.00 ETB"
  // would go back as something Rust refuses to read.
  const [price, setPrice] = useState(product.priceValue ?? "");
  const onMenu = product.saleItemId !== null;

  return (
    <>
      <h3>{product.name}</h3>
      <div className="grid grid--halves">
        <Field label="Name" help="It is renamed on the shelf and on the till together.">
          <input value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field
          label="Sale price"
          help={
            onMenu
              ? "Changing it prices from now on. What was already charged stands."
              : "It goes on the menu under its own name, drawing one off the shelf per sale. Leave it blank to keep it off."
          }
        >
          <input
            inputMode="decimal"
            value={price}
            onChange={(event) => setPrice(event.target.value)}
          />
        </Field>
      </div>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || name.trim() === ""}
        onClick={async () => {
          const ok = await run(async () => {
            const after = await api.editProduct(product.id, formOf(product, name));
            if (price.trim() === "" || price.trim() === product.priceValue) return after;
            // A first price is what puts it on the menu; a later one is a
            // reprice of the entry already there.
            return product.saleItemId === null
              ? api.sellProduct(product.id, price)
              : api.setPrice(product.saleItemId, price);
          });
          if (ok) onDone();
        }}
      >
        Save
      </button>
    </>
  );
}

/* -------------------------------------------------------------------------- */

/**
 * Drinks made of more than one thing, or of part of a thing.
 *
 * Anything sold one for one off the shelf is already an Items row and is
 * deliberately not repeated here — listing it twice is the duplication this
 * screen was reorganised to remove.
 */
function SaleItems({ view, busy, run }: Section) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [category, setCategory] = useState("");
  const [editingId, setEditingId] = useState<number>();

  const composed = view.saleItems.filter((item) => item.fromProductId === null);
  const current = composed.find((item) => item.id === editingId);

  return (
    <Card
      title="Composed drinks"
      blurb="A cocktail, or a spirit poured by the shot — anything whose recipe is more than one bottle off the shelf. Sellable once it has a recipe and a price."
      aside={
        <button type="button" className="btn" onClick={() => setOpen(!open)}>
          {open ? "Cancel" : "Add"}
        </button>
      }
    >
      {open && (
        <>
          <div className="grid grid--halves">
            <Field label="Name" help="What it is called when somebody orders it.">
              <input value={name} onChange={(event) => setName(event.target.value)} />
            </Field>
            <CategoryField
              id="sale-item-categories"
              value={category}
              used={view.saleItems.map((item) => item.category)}
              help="How it is grouped on the till — Bottles, Shots, Cocktails."
              onChange={setCategory}
            />
          </div>
          <button
            type="button"
            className="btn btn--primary"
            disabled={busy || name.trim() === ""}
            onClick={async () => {
              const ok = await run(() => api.addSaleItem({ name, category }));
              if (ok) {
                setName("");
                setOpen(false);
              }
            }}
          >
            Add it
          </button>
        </>
      )}

      {composed.length === 0 ? (
        <Empty glyph="▤" title="Nothing on the menu yet">
          Add a drink, then tell it what it is made of and what it costs.
        </Empty>
      ) : (
        <ul className="rows">
          {composed.map((item) => (
            <li key={item.id} className="row row--static">
              <button
                type="button"
                className="row__main"
                aria-pressed={item.id === editingId}
                onClick={() => setEditingId(editingId === item.id ? undefined : item.id)}
              >
                <strong>{item.name}</strong>
                <span className="muted">
                  {item.category} ·{" "}
                  {item.recipe.length > 0
                    ? item.recipe.map((line) => `${line.quantity} ${line.name}`).join(" + ")
                    : "no recipe"}
                </span>
              </button>
              <span className="row__value">
                {item.price ?? "no price"}
                {!item.active ? (
                  <Chip tone="quiet">Removed</Chip>
                ) : (
                  !item.sellable && <Chip tone="warn">Not sellable</Chip>
                )}
                <button
                  type="button"
                  className={item.active ? "btn btn--danger" : "btn"}
                  disabled={busy}
                  onClick={() => run(() => api.setSaleItemActive(item.id, !item.active))}
                >
                  {item.active ? "Remove" : "Bring back"}
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}

      {current && (
        <Composition key={current.id} item={current} view={view} busy={busy} run={run} />
      )}
    </Card>
  );
}

function Composition({ item, view, busy, run }: Section & { item: SaleItemLine }) {
  // A measured product is edited in its own measure — 30, meaning 30ml — and
  // Rust converts. An unmeasured one is edited in whole counted units.
  const [lines, setLines] = useState(
    item.recipe.map((line) => ({
      productId: line.productId,
      quantity: line.measure === "NONE" ? String(line.quantityMilli / MILLI) : line.measureQuantity,
    })),
  );
  const measureOf = (productId: number) =>
    view.products.find((product) => product.id === productId)?.contentMeasure ?? "NONE";
  const unitLabel = (productId: number) => {
    const measure = measureOf(productId);
    return measure === "ML" ? "millilitres" : measure === "GRAM" ? "grams" : "whole units";
  };
  const [name, setName] = useState(item.name);
  // The plain figure, not the formatted one — see EditProduct.
  const [price, setPrice] = useState(item.priceValue ?? "");

  return (
    <>
      <h3>{item.name}</h3>
      <p className="field__help">
        One {item.name.toLowerCase()} takes the following off the shelf. A cocktail can draw on
        several things, and the same bottle may appear twice.
      </p>

      {lines.map((line, index) => (
        // The index is the identity here: these rows are only ever appended to
        // or edited in place, never reordered.
        <div className="grid grid--halves" key={index}>
          <Field label="Takes">
            <select
              value={line.productId}
              onChange={(event) =>
                setLines((current) =>
                  current.map((existing, at) =>
                    at === index
                      ? { ...existing, productId: Number(event.target.value) }
                      : existing,
                  ),
                )
              }
            >
              <option value={0}>Choose</option>
              {view.products.map((product) => (
                <option key={product.id} value={product.id}>
                  {product.name}
                </option>
              ))}
            </select>
          </Field>
          <Field label={`How much, in ${unitLabel(line.productId)}`}>
            <input
              inputMode="decimal"
              value={line.quantity}
              onChange={(event) =>
                setLines((current) =>
                  current.map((existing, at) =>
                    at === index ? { ...existing, quantity: event.target.value } : existing,
                  ),
                )
              }
            />
          </Field>
        </div>
      ))}

      <div className="grid grid--halves">
        <button
          type="button"
          className="btn"
          onClick={() => setLines((current) => [...current, { productId: 0, quantity: "1" }])}
        >
          Add an ingredient
        </button>
        <button
          type="button"
          className="btn btn--primary"
          disabled={busy || lines.length === 0 || lines.some((line) => line.productId === 0)}
          onClick={() =>
            run(() =>
              api.setRecipe(
                item.id,
                lines.map((line) => ({
                  productId: line.productId,
                  quantityMilli: toMilli(line.quantity),
                  inMeasure: measureOf(line.productId) !== "NONE",
                })),
              ),
            )
          }
        >
          Save the recipe
        </button>
      </div>

      <div className="grid grid--halves">
        <Field label="Name" help="What it is called when somebody orders it.">
          <input value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field label="Price" help="What the customer is charged for one.">
          <input
            inputMode="decimal"
            value={price}
            onChange={(event) => setPrice(event.target.value)}
          />
        </Field>
      </div>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || name.trim() === ""}
        onClick={() =>
          run(async () => {
            const after = await api.editSaleItem(item.id, {
              name,
              category: item.category,
              active: item.active,
            });
            return price.trim() === "" || price.trim() === item.priceValue
              ? after
              : api.setPrice(item.id, price);
          })
        }
      >
        Save the name and price
      </button>
    </>
  );
}

/* -------------------------------------------------------------------------- */

function Staff({ view, busy, run }: Section) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [role, setRole] = useState("WAITER");
  const [pin, setPin] = useState("");

  const signsIn = role !== "WAITER";

  return (
    <Card
      title="Who works here"
      blurb="Waiters hold tabs and never sign in. Cashiers work the till."
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
    </Card>
  );
}
