/**
 * The warehouse — one list of what this venue buys and sells.
 *
 * **There is no separate menu.** A thing on the shelf is a thing you sell, and
 * the only question ever asked about it is whether it goes over the counter
 * whole or is poured by the measure. That replaced a screen with two cards —
 * shelf items and "composed drinks" — and a recipe editor that let an owner
 * build a menu entry with no recipe and no price, which is exactly how two
 * items on this venue's till came to be silently unsellable.
 *
 * The recipe still exists underneath, and is still what draws the stock down.
 * It is simply never more than one line, so nobody has to see it.
 */

import { useEffect, useState } from "react";

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
import { Inventory } from "./Floor";

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

type Run = (work: () => Promise<SetupView>) => Promise<boolean>;

/** What a product is sold as: the whole thing, and any measures poured from it. */
type Sold = { whole?: SaleItemLine; measures: SaleItemLine[] };

/**
 * Group the menu under the shelf item each entry draws from.
 *
 * Every recipe here is one line by construction, so the product it names is
 * the product it belongs to. A whole unit is `1000` thousandths; anything less
 * is a measure poured from it. This is filtering, not arithmetic — every
 * figure shown arrives from Rust already formatted.
 */
function soldAs(products: ProductLine[], items: SaleItemLine[]): Map<number, Sold> {
  const byProduct = new Map<number, Sold>();
  for (const product of products) byProduct.set(product.id, { measures: [] });
  for (const item of items) {
    const line = item.recipe[0];
    if (!line || item.recipe.length !== 1) continue;
    const entry = byProduct.get(line.productId);
    if (!entry) continue;
    if (line.quantityMilli === MILLI) entry.whole = item;
    else entry.measures.push(item);
  }
  return byProduct;
}

function measureLabel(item: SaleItemLine): string {
  const line = item.recipe[0];
  if (!line) return "";
  const unit = line.measure === "ML" ? "ml" : line.measure === "GRAM" ? "g" : "";
  return `${line.measureQuantity}${unit}`;
}

export function Warehouse() {
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
          <Banner tone="bad" title="The warehouse could not be read">
            {error}
          </Banner>
        </div>
      </div>
    ) : (
      <Loading what="the warehouse" />
    );
  }

  return (
    <div className="page">
      <div className="page__inner">
        <div className="pagehead">
          <div className="pagehead__text">
            <span className="eyebrow">Warehouse</span>
            <h1>What you buy and sell</h1>
            <p className="muted">
              One list. Each thing is counted on the shelf and sold over the counter — whole, by
              the shot, or both.
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

        <Items view={view} busy={busy} run={run} />
        <Orphans view={view} busy={busy} run={run} />
        <Inventory />
      </div>
    </div>
  );
}

/* -------------------------------------------------------------------------- */

/**
 * A category is chosen from the ones already in use, or typed to start a new
 * one. `<datalist>` is the browser's own combobox: it filters as you type and
 * still accepts something new, which is what lets the first item of a new
 * category name it. Typing one that already exists is what produced
 * "Whiskey", "whiskey" and "Whisky" as three groups.
 */
function CategoryField({
  value,
  used,
  onChange,
}: {
  value: string;
  used: string[];
  onChange: (next: string) => void;
}) {
  const known = [...new Set(used.map((one) => one.trim()).filter(Boolean))].sort((a, b) =>
    a.localeCompare(b),
  );
  return (
    <Field label="Category" help="How the list is grouped — Whiskey, Beer, Mixers.">
      <input
        list="warehouse-categories"
        value={value}
        autoComplete="off"
        placeholder={known.length > 0 ? "Choose or type a new one" : "Type the first one"}
        onChange={(event) => onChange(event.target.value)}
      />
      <datalist id="warehouse-categories">
        {known.map((one) => (
          <option key={one} value={one} />
        ))}
      </datalist>
    </Field>
  );
}

/**
 * The form Rust wants, rebuilt from the line it sent. Everything but the name
 * is handed straight back, so an edit that changes one field cannot quietly
 * restate the rest. `salePrice` is left out because an update ignores it.
 */
function formOf(
  product: ProductLine,
  name: string,
  measure: Measure,
  perUnit: string,
): ProductForm {
  return {
    name,
    category: product.category,
    baseUnit: product.baseUnit as BaseUnit,
    baseUnitsPerPack: toMilli(product.baseUnitsPerPack),
    unitsPerPurchasePack: product.unitsPerPurchasePack,
    lowStockThreshold: toMilli(product.lowStockThreshold),
    tracksInventory: product.tracksInventory,
    destination: product.destination as Destination,
    contentMeasure: measure,
    contentPerUnitMilli: measure === "NONE" ? 0 : toMilli(perUnit),
    active: product.active,
  };
}

function Items({ view, busy, run }: { view: SetupView; busy: boolean; run: Run }) {
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<ProductLine>();
  const [pouring, setPouring] = useState<ProductLine>();

  const sold = soldAs(view.products, view.saleItems);

  return (
    <Card
      title="Items"
      blurb="What sits on the shelf, and what it sells for."
      aside={
        <button type="button" className="btn" onClick={() => setAdding(!adding)}>
          {adding ? "Cancel" : "Add"}
        </button>
      }
    >
      {adding && <AddItem view={view} busy={busy} run={run} onDone={() => setAdding(false)} />}

      {view.products.length === 0 ? (
        <Empty glyph="▦" title="Nothing on the shelf yet">
          Start with a bottle of something. Give it a price and it is on the till at the same time.
        </Empty>
      ) : (
        <ul className="rows">
          {view.products.map((product) => {
            const entry = sold.get(product.id) ?? { measures: [] };
            const measured = product.contentMeasure !== "NONE";
            return (
              <li key={product.id} className="row row--static">
                <span className="row__main">
                  <strong>{product.name}</strong>
                  <span className="muted">
                    {product.category} · {product.onHand} on the shelf
                    {measured &&
                      ` · ${product.contentPerUnit}${product.contentMeasure === "ML" ? "ml" : "g"} each`}
                  </span>
                </span>
                <span className="row__value">
                  {!product.active && <Chip tone="quiet">Removed</Chip>}
                  {entry.whole ? entry.whole.price : <Chip tone="quiet">Not sold whole</Chip>}
                  {entry.measures.map((shot) => (
                    <Chip key={shot.id} tone="good">
                      {measureLabel(shot)} {shot.price}
                    </Chip>
                  ))}
                  {product.active && (
                    <>
                      <button
                        type="button"
                        className="btn"
                        onClick={() =>
                          setEditing(editing?.id === product.id ? undefined : product)
                        }
                      >
                        {entry.whole ? "Edit" : "Sell it"}
                      </button>
                      {measured && (
                        <button
                          type="button"
                          className="btn"
                          onClick={() =>
                            setPouring(pouring?.id === product.id ? undefined : product)
                          }
                        >
                          Add a shot
                        </button>
                      )}
                    </>
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
            );
          })}
        </ul>
      )}

      {editing && (
        <EditItem
          key={editing.id}
          product={editing}
          sold={sold.get(editing.id)?.whole}
          busy={busy}
          run={run}
          onDone={() => setEditing(undefined)}
        />
      )}

      {pouring && (
        <AddShot
          key={pouring.id}
          product={pouring}
          busy={busy}
          run={run}
          onDone={() => setPouring(undefined)}
        />
      )}
    </Card>
  );
}

/* -------------------------------------------------------------------------- */

/**
 * Menu entries left over from the old two-card catalogue.
 *
 * Anything whose recipe is not exactly one shelf item cannot appear in the
 * list above, and the till already refuses to sell it. Left unlisted it would
 * be a row that exists and no screen shows — which is how this venue ended up
 * with two priced items nobody could ring up and nothing saying why. They are
 * named here, with the reason, and can be taken off.
 */
function Orphans({ view, busy, run }: { view: SetupView; busy: boolean; run: Run }) {
  const shelf = new Set(view.products.map((product) => product.id));
  const stray = view.saleItems.filter(
    (item) =>
      item.active &&
      !(item.recipe.length === 1 && shelf.has(item.recipe[0]?.productId ?? -1)),
  );
  if (stray.length === 0) return null;

  return (
    <Card
      title="Not attached to the shelf"
      blurb="These are on the menu but draw from nothing the till can count, so they cannot be sold. Take them off, then add them again as an item or a shot above."
    >
      <ul className="rows">
        {stray.map((item) => (
          <li key={item.id} className="row row--static">
            <span className="row__main">
              <strong>{item.name}</strong>
              <span className="muted">
                {item.code} ·{" "}
                {item.recipe.length === 0
                  ? "nothing set — it was never finished"
                  : "made of more than one thing"}
              </span>
            </span>
            <span className="row__value">
              {item.price ?? <Chip tone="quiet">No price</Chip>}
              <Chip tone="warn">Cannot be sold</Chip>
              <button
                type="button"
                className="btn btn--danger"
                disabled={busy}
                onClick={() => run(() => api.setSaleItemActive(item.id, false))}
              >
                Take it off
              </button>
            </span>
          </li>
        ))}
      </ul>
    </Card>
  );
}

function AddItem({
  view,
  busy,
  run,
  onDone,
}: {
  view: SetupView;
  busy: boolean;
  run: Run;
  onDone: () => void;
}) {
  const [name, setName] = useState("");
  const [category, setCategory] = useState("");
  const [unit, setUnit] = useState<BaseUnit>("BOTTLE");
  const [threshold, setThreshold] = useState("3");
  const [price, setPrice] = useState("");
  // Only asked for when it will be poured. Most of a club's list is handed
  // over whole and needs nothing here.
  const [measure, setMeasure] = useState<Measure>("NONE");
  const [perUnit, setPerUnit] = useState("750");

  return (
    <>
      <div className="grid grid--halves">
        <Field label="Name">
          <input value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <CategoryField
          value={category}
          used={view.products.map((product) => product.category)}
          onChange={setCategory}
        />
        <Field label="Counted in">
          <select value={unit} onChange={(event) => setUnit(event.target.value as BaseUnit)}>
            <option value="BOTTLE">Bottles</option>
            <option value="SHOT">Shots</option>
            <option value="UNIT">Units</option>
          </select>
        </Field>
        <Field label="Price" help="What one costs the customer. Leave blank to only stock it.">
          <input
            inputMode="decimal"
            value={price}
            onChange={(event) => setPrice(event.target.value)}
          />
        </Field>
        <Field label="Warn below" help="When this many are left, it is flagged as low.">
          <input
            inputMode="decimal"
            value={threshold}
            onChange={(event) => setThreshold(event.target.value)}
          />
        </Field>
        <Field
          label="Also poured by the shot?"
          help="Only for something measured out. Say how much one holds and you can add shots to it."
        >
          <select value={measure} onChange={(event) => setMeasure(event.target.value as Measure)}>
            <option value="NONE">No — sold whole</option>
            <option value="ML">Yes — millilitres</option>
            <option value="GRAM">Yes — grams</option>
          </select>
        </Field>
        {measure !== "NONE" && (
          <Field
            label={measure === "ML" ? "Millilitres in one" : "Grams in one"}
            help="A 750ml bottle is 750."
          >
            <input
              inputMode="decimal"
              value={perUnit}
              onChange={(event) => setPerUnit(event.target.value)}
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
            api.addProduct({
              salePrice: price.trim() === "" ? null : price,
              contentMeasure: measure,
              contentPerUnitMilli: measure === "NONE" ? 0 : toMilli(perUnit),
              name,
              category,
              baseUnit: unit,
              // Both inert: nothing multiplies by either, and "how much one
              // holds" above is the field that actually converts. Sent as one
              // so the schema CHECKs hold.
              baseUnitsPerPack: MILLI,
              unitsPerPurchasePack: 1,
              lowStockThreshold: toMilli(threshold),
              tracksInventory: true,
              destination: "BAR",
            }),
          );
          if (ok) onDone();
        }}
      >
        Add it
      </button>
    </>
  );
}

/** Rename something, and set what one whole unit sells for. */
function EditItem({
  product,
  sold,
  busy,
  run,
  onDone,
}: {
  product: ProductLine;
  sold: SaleItemLine | undefined;
  busy: boolean;
  run: Run;
  onDone: () => void;
}) {
  const [name, setName] = useState(product.name);
  // The plain figure, never the formatted one: "1,200.00 ETB" would go back
  // as something Rust refuses to read.
  const [price, setPrice] = useState(product.priceValue ?? "");
  // Editable here as well as on the way in, so a bottle already on the shelf
  // can start being poured by the shot without being added again.
  const [measure, setMeasure] = useState<Measure>(product.contentMeasure);
  const [perUnit, setPerUnit] = useState(product.contentPerUnit || "750");

  return (
    <>
      <h3>{product.name}</h3>
      <div className="grid grid--halves">
        <Field label="Name" help="It is renamed on the shelf and on the till together.">
          <input value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Field
          label="Price"
          help={
            sold
              ? "Changing it prices from now on. What was already charged stands."
              : "Give it a price and it goes on the till, one off the shelf per sale."
          }
        >
          <input
            inputMode="decimal"
            value={price}
            onChange={(event) => setPrice(event.target.value)}
          />
        </Field>
        <Field
          label="Also poured by the shot?"
          help="Say how much one holds and shots can be added to it."
        >
          <select value={measure} onChange={(event) => setMeasure(event.target.value as Measure)}>
            <option value="NONE">No — sold whole</option>
            <option value="ML">Yes — millilitres</option>
            <option value="GRAM">Yes — grams</option>
          </select>
        </Field>
        {measure !== "NONE" && (
          <Field
            label={measure === "ML" ? "Millilitres in one" : "Grams in one"}
            help="A 750ml bottle is 750."
          >
            <input
              inputMode="decimal"
              value={perUnit}
              onChange={(event) => setPerUnit(event.target.value)}
            />
          </Field>
        )}
      </div>
      <button
        type="button"
        className="btn btn--primary"
        disabled={busy || name.trim() === ""}
        onClick={async () => {
          const ok = await run(async () => {
            const after = await api.editProduct(
              product.id,
              formOf(product, name, measure, perUnit),
            );
            if (price.trim() === "" || price.trim() === product.priceValue) return after;
            // A first price puts it on the till; a later one reprices what is
            // already there.
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

/**
 * Add a measure poured from this bottle.
 *
 * One command, not three. The menu entry, the amount it draws and its price
 * are written together or not at all — built separately they can leave an item
 * that looks finished here and is refused at the till.
 */
function AddShot({
  product,
  busy,
  run,
  onDone,
}: {
  product: ProductLine;
  busy: boolean;
  run: Run;
  onDone: () => void;
}) {
  const unit = product.contentMeasure === "ML" ? "millilitres" : "grams";
  const [poured, setPoured] = useState("30");
  const [price, setPrice] = useState("");

  return (
    <>
      <h3>A shot of {product.name.toLowerCase()}</h3>
      <p className="field__help">
        One {product.name} holds {product.contentPerUnit} {unit}. What comes off the shelf is
        worked out from that, so the stock stays right without anybody dividing anything.
      </p>
      <div className="grid grid--halves">
        <Field label={`How much, in ${unit}`}>
          <input
            inputMode="decimal"
            value={poured}
            onChange={(event) => setPoured(event.target.value)}
          />
        </Field>
        <Field label="Price" help="What one shot costs the customer.">
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
        disabled={busy || price.trim() === "" || poured.trim() === ""}
        onClick={async () => {
          const ok = await run(() => api.sellByMeasure(product.id, toMilli(poured), price));
          if (ok) onDone();
        }}
      >
        Put it on the till
      </button>
    </>
  );
}
