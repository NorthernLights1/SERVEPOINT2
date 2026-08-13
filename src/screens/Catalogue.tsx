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

import type { ProductLine, SaleItemLine, SetupView } from "../api";
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
  const [perPack, setPerPack] = useState("1");
  const [perPurchase, setPerPurchase] = useState("24");
  const [threshold, setThreshold] = useState("3");
  const [destination, setDestination] = useState<"BAR" | "KITCHEN">("BAR");
  const [counting, setCounting] = useState<ProductLine>();
  const [pricing, setPricing] = useState<ProductLine>();
  // A club sells bottles, so the common case is the default: what goes on the
  // shelf is what you sell. Untick it for a mixer nobody orders by name.
  const [onMenu, setOnMenu] = useState(true);
  const [price, setPrice] = useState("");

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
              label="Units per pack"
              help="A bottle poured as shots holds more than one — 24 shots to a bottle of gin."
            >
              <input
                inputMode="decimal"
                value={perPack}
                onChange={(event) => setPerPack(event.target.value)}
              />
            </Field>
            <Field label="Bought in packs of">
              <input
                inputMode="numeric"
                value={perPurchase}
                onChange={(event) => setPerPurchase(event.target.value)}
              />
            </Field>
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
                  name,
                  category,
                  baseUnit: unit,
                  baseUnitsPerPack: toMilli(perPack),
                  unitsPerPurchasePack: Number(perPurchase) || 0,
                  lowStockThreshold: toMilli(threshold),
                  tracksInventory: true,
                  destination,
                }),
              );
              if (ok) {
                setName("");
                setPrice("");
                setOnMenu(true);
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
                </span>
              </span>
              <span className="row__value">
                {product.price ?? <Chip tone="quiet">Not sold</Chip>}
                {product.saleItemId === null && (
                  <button
                    type="button"
                    className="btn"
                    onClick={() => setPricing(pricing?.id === product.id ? undefined : product)}
                  >
                    Sell it
                  </button>
                )}
                <button
                  type="button"
                  className="btn"
                  onClick={() => setCounting(counting?.id === product.id ? undefined : product)}
                >
                  Count in
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}

      {pricing && (
        <StartSelling
          product={pricing}
          busy={busy}
          run={run}
          onDone={() => setPricing(undefined)}
        />
      )}

      {counting && (
        <OpeningStock
          product={counting}
          busy={busy}
          run={run}
          onDone={() => setCounting(undefined)}
        />
      )}
    </Card>
  );
}

/** Put a stock-only item on the menu after the fact, sold one for one. */
function StartSelling({
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
  const [price, setPrice] = useState("");

  return (
    <>
      <Field
        label={`What one ${product.name.toLowerCase()} sells for`}
        help="It goes on the menu under its own name, drawing one off the shelf per sale."
      >
        <input
          inputMode="decimal"
          value={price}
          onChange={(event) => setPrice(event.target.value)}
        />
      </Field>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || price.trim() === ""}
        onClick={async () => {
          const ok = await run(() => api.sellProduct(product.id, price));
          if (ok) onDone();
        }}
      >
        Put it on the menu
      </button>
    </>
  );
}

function OpeningStock({
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
  const [quantity, setQuantity] = useState("");
  const [cost, setCost] = useState("");

  return (
    <>
      <Field
        label={`How many ${product.name.toLowerCase()} are on the shelf now`}
        help="The first count. Everything after this goes through deliveries and stock counts."
      >
        <input
          inputMode="decimal"
          value={quantity}
          onChange={(event) => setQuantity(event.target.value)}
        />
      </Field>
      <Field
        label="What one cost"
        help="Used to value the shelf and work out what each drink actually earns."
      >
        <input inputMode="decimal" value={cost} onChange={(event) => setCost(event.target.value)} />
      </Field>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || quantity.trim() === "" || cost.trim() === ""}
        onClick={async () => {
          const ok = await run(() =>
            api.addOpeningStock({
              productId: product.id,
              quantityMilli: toMilli(quantity),
              unitCost: cost,
            }),
          );
          if (ok) onDone();
        }}
      >
        Count it in
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
            <li key={item.id}>
              <button
                type="button"
                className="row"
                aria-pressed={item.id === editingId}
                onClick={() => setEditingId(editingId === item.id ? undefined : item.id)}
              >
                <span className="row__main">
                  <strong>{item.name}</strong>
                  <span className="muted">
                    {item.category} ·{" "}
                    {item.recipe.length > 0
                      ? item.recipe.map((line) => `${line.quantity} ${line.name}`).join(" + ")
                      : "no recipe"}
                  </span>
                </span>
                <span className="row__value">
                  {item.price ?? "no price"}
                  {!item.sellable && <Chip tone="warn">Not sellable</Chip>}
                </span>
              </button>
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
  const [lines, setLines] = useState(
    item.recipe.map((line) => ({
      productId: line.productId,
      quantity: String(line.quantityMilli / MILLI),
    })),
  );
  const [price, setPrice] = useState(item.price ?? "");

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
          <Field label="How much">
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
                })),
              ),
            )
          }
        >
          Save the recipe
        </button>
      </div>

      <Field label="Price" help="What the customer is charged for one.">
        <input inputMode="decimal" value={price} onChange={(event) => setPrice(event.target.value)} />
      </Field>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || price.trim() === ""}
        onClick={() => run(() => api.setPrice(item.id, price))}
      >
        Save the price
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
