/**
 * The shell: which screen is showing, and who is allowed to see it.
 *
 * The two roles do genuinely different jobs and are given genuinely different
 * navigation. The cashier runs the till and settles the night; the owner reads
 * what happened and configures the venue. This is not a permission matrix
 * bolted onto one screen — nothing about a void or a write-off needs a second
 * person to approve it, which was a deliberate decision. It gates reading and
 * configuration, not operations.
 */

import { useCallback, useEffect, useState } from "react";

import { api, inDesktopApp, type BootstrapView, type ServePointError, type Session } from "./api";
import { Banner, Loading } from "./ui";
import { SignIn, Setup } from "./screens/Gate";
import { Catalogue } from "./screens/Catalogue";
import { Settings } from "./screens/Settings";
import { EndOfDay } from "./screens/EndOfDay";
import { Inventory, Overview, Reports, Till } from "./screens/Floor";

type Route =
  | "overview"
  | "till"
  | "inventory"
  | "endofday"
  | "reports"
  | "catalogue"
  | "settings";

interface NavEntry {
  route: Route;
  label: string;
  glyph: string;
}

const CASHIER_NAV: NavEntry[] = [
  { route: "till", label: "Till", glyph: "▤" },
  { route: "endofday", label: "End of day", glyph: "◫" },
  { route: "inventory", label: "Inventory", glyph: "▦" },
];

const OWNER_NAV: NavEntry[] = [
  { route: "overview", label: "Overview", glyph: "◑" },
  { route: "reports", label: "Reports", glyph: "▥" },
  { route: "inventory", label: "Inventory", glyph: "▦" },
  { route: "catalogue", label: "Catalogue", glyph: "◈" },
  { route: "settings", label: "Settings", glyph: "⚙" },
];

type Theme = "night" | "day";

function storedTheme(): Theme {
  return localStorage.getItem("servepoint.theme") === "day" ? "day" : "night";
}

export default function App() {
  const [boot, setBoot] = useState<BootstrapView>();
  const [error, setError] = useState<string>();
  const [route, setRoute] = useState<Route>("overview");
  const [theme, setTheme] = useState<Theme>(storedTheme);

  const refresh = useCallback(async () => {
    try {
      const next = await api.bootstrap();
      setBoot(next);
      setError(undefined);
      // Land wherever this person actually works, rather than on a screen
      // their role cannot open.
      if (next.session) {
        setRoute(next.session.role === "OWNER" ? "overview" : "till");
      }
    } catch (raw) {
      setError((raw as ServePointError).message);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    document.documentElement.dataset["theme"] = theme;
    localStorage.setItem("servepoint.theme", theme);
  }, [theme]);

  if (!inDesktopApp) {
    return (
      <div className="gate">
        <div className="gate__panel">
          <Banner tone="warn" title="This is not the application">
            ServePoint is a desktop program, not a web page. Close this tab and start it with{" "}
            <code>npm run app</code>.
          </Banner>
        </div>
      </div>
    );
  }

  if (error && !boot) {
    return (
      <div className="gate">
        <div className="gate__panel">
          <Banner tone="bad" title="ServePoint could not start">
            {error}
          </Banner>
        </div>
      </div>
    );
  }

  if (!boot) return <Loading what="Opening the venue's records…" />;

  if (!boot.setupCompleted) {
    return <Setup onDone={() => void refresh()} />;
  }

  if (!boot.session) {
    return <SignIn boot={boot} onSignedIn={() => void refresh()} />;
  }

  const session: Session = boot.session;
  const nav = session.role === "OWNER" ? OWNER_NAV : CASHIER_NAV;
  const allowed = nav.some((entry) => entry.route === route);
  const current: Route = allowed ? route : (nav[0]?.route ?? "overview");

  async function signOut() {
    await api.signOut();
    await refresh();
  }

  return (
    <div className="shell">
      <nav className="rail">
        <div className="brand">
          <span className="brand__mark" aria-hidden="true">
            S
          </span>
          <span className="brand__text" style={{ minWidth: 0 }}>
            <div className="brand__name">ServePoint</div>
            <div className="brand__venue">{boot.venue.name || "Unnamed venue"}</div>
          </span>
        </div>

        <div className="nav">
          {nav.map((entry) => (
            <button
              key={entry.route}
              type="button"
              className="navitem"
              aria-current={entry.route === current ? "page" : undefined}
              onClick={() => setRoute(entry.route)}
            >
              <span className="navitem__glyph" aria-hidden="true">
                {entry.glyph}
              </span>
              <span className="navitem__label">{entry.label}</span>
            </button>
          ))}
        </div>

        <div className="rail__foot">
          <div className="whoami">
            <span className="avatar" aria-hidden="true">
              {session.name.slice(0, 1).toUpperCase()}
            </span>
            <span className="whoami__text" style={{ minWidth: 0 }}>
              <div style={{ fontSize: 13.5, fontWeight: 560 }}>{session.name}</div>
              <div className="brand__venue">{session.role === "OWNER" ? "Owner" : "Cashier"}</div>
            </span>
          </div>
          <button
            type="button"
            className="navitem"
            onClick={() => setTheme(theme === "night" ? "day" : "night")}
          >
            <span className="navitem__glyph" aria-hidden="true">
              {theme === "night" ? "☾" : "☀"}
            </span>
            <span className="navitem__label">{theme === "night" ? "Night" : "Daylight"}</span>
          </button>
          <button type="button" className="navitem" onClick={() => void signOut()}>
            <span className="navitem__glyph" aria-hidden="true">
              ⏻
            </span>
            <span className="navitem__label">Sign out</span>
          </button>
        </div>
      </nav>

      <div className="main">
        <header className="topbar">
          <span className="eyebrow">{boot.businessDateLabel}</span>
          <span className="topbar__spacer" />
          {boot.openShift ? (
            <span className={boot.openShift.overdue ? "chip chip--warn" : "chip chip--good"}>
              <span className="dot" />
              {boot.openShift.overdue ? "Shift overdue" : `${boot.openShift.code} open`}
            </span>
          ) : (
            <span className="chip chip--quiet">No shift open</span>
          )}
        </header>

        {current === "overview" && <Overview boot={boot} />}
        {current === "till" && <Till />}
        {current === "inventory" && <Inventory />}
        {current === "endofday" && <EndOfDay />}
        {current === "reports" && <Reports />}
        {current === "catalogue" && <Catalogue />}
        {current === "settings" && <Settings />}
      </div>
    </div>
  );
}
