import { useState, useEffect } from "react";
import { BrowserRouter, NavLink, Routes, Route, Link } from "react-router-dom";
import {
  Activity,
  LayoutDashboard,
  Moon,
  RefreshCw,
  Scale,
  Settings,
  Sun,
  Users,
} from "lucide-react";
import { Provider, useData } from "./data/AppData.jsx";
import { Empty } from "./components/ui.jsx";
import { Overview } from "./views/Overview.jsx";
import { Measurements } from "./views/Measurements.jsx";
import { Detail } from "./views/Detail.jsx";
import { Profiles } from "./views/Profiles.jsx";
import { Preferences } from "./views/Preferences.jsx";

function Shell() {
  const d = useData();
  const [theme, setTheme] = useState(
    () => localStorage.getItem("grammatic-theme") || "light",
  );
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("grammatic-theme", theme);
  }, [theme]);
  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <Scale size={16} strokeWidth={2} />
          </span>
          <span>
            <strong>Grammatic</strong>
            <small>SMART SCALE</small>
          </span>
        </div>
        <p className="nav-caption">EXPLORE</p>
        <nav>
          {[
            ["/", "Overview", LayoutDashboard],
            ["/measurements", "Measurements", Activity],
            ["/profiles", "Profiles", Users],
            ["/settings", "Settings", Settings],
          ].map(([to, label, Icon]) => (
            <NavLink key={to} to={to} end={to === "/"}>
              <Icon size={17} />
              <span>{label}</span>
            </NavLink>
          ))}
        </nav>
      </aside>
      <main>
        <div className="utility">
          <button onClick={d.refresh} aria-label="Refresh data">
            <RefreshCw size={14} />
            <span>{d.error ? "Retry connection" : "Refresh"}</span>
          </button>
          <button
            aria-label={`Use ${theme === "light" ? "dark" : "light"} theme`}
            onClick={() => setTheme(theme === "light" ? "dark" : "light")}
          >
            {theme === "light" ? <Moon size={16} /> : <Sun size={16} />}
          </button>
        </div>
        {d.error && (
          <div className="error" role="alert">
            {d.error} <button onClick={d.refresh}>Retry</button>
          </div>
        )}
        {d.loading ? (
          <div className="empty" role="status">
            Loading your measurements…
          </div>
        ) : (
          <Routes>
            <Route path="/" element={<Overview />} />
            <Route path="/measurements" element={<Measurements />} />
            <Route path="/measurements/:id" element={<Detail />} />
            <Route path="/profiles" element={<Profiles />} />
            <Route path="/settings" element={<Preferences />} />
            <Route
              path="*"
              element={
                <Empty text="Page not found">
                  <Link to="/">Return to overview</Link>
                </Empty>
              }
            />
          </Routes>
        )}
      </main>
    </div>
  );
}

export function App() {
  return (
    <BrowserRouter>
      <Provider>
        <Shell />
      </Provider>
    </BrowserRouter>
  );
}
