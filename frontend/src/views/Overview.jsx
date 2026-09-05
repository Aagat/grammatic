import { useState, lazy, Suspense } from "react";
import { Link } from "react-router-dom";
import { useData } from "../data/AppData.jsx";
import { fmt, date, filtered } from "../data/format.js";
import { Plus, ArrowUpRight } from "lucide-react";
import { Header, ProfileSelect, Empty } from "../components/ui.jsx";
import { MeasurementForm } from "../components/Forms.jsx";
const Heatmap = lazy(() =>
  import("../components/DashboardCharts.jsx").then((m) => ({
    default: m.Heatmap,
  })),
);
const Trend = lazy(() =>
  import("../components/DashboardCharts.jsx").then((m) => ({
    default: m.Trend,
  })),
);

export function Overview() {
  const d = useData();
  const [dialog, setDialog] = useState(false);
  const selected =
    d.profile === "all"
      ? d.profiles[0]
        ? String(d.profiles[0].id)
        : "guest"
      : d.profile;
  const rows = filtered(d.measurements, selected);
  const latest = rows[0];
  return (
    <>
      <Header
        eyebrow="OVERVIEW"
        title="Your household, measured"
        description="A little perspective on your daily measurements."
      >
        <ProfileSelect value={selected} onChange={d.setProfile} />
        <button className="primary" onClick={() => setDialog(true)}>
          <Plus size={16} />
          Log measurement
        </button>
      </Header>
      <div className="metrics-grid">
        {[
          ["Latest weight", fmt(latest?.weight_kg, " kg")],
          ["Body fat", fmt(latest?.body_fat_pct, "%")],
          ["BMI", fmt(latest?.bmi)],
          ["Measurements", rows.length.toLocaleString()],
        ].map(([label, value]) => (
          <article className="metric" key={label}>
            <span>{label}</span>
            <strong>{value}</strong>
            <small>
              {label === "Measurements"
                ? "SELECTED PROFILES"
                : latest
                  ? date(latest.measured_at)
                  : "NO MEASUREMENTS YET"}
            </small>
          </article>
        ))}
      </div>
      <div className="overview-grid">
        <div className="charts">
          <Suspense
            fallback={<div className="card empty">Loading charts…</div>}
          >
            <Heatmap rows={rows} />
            <Trend rows={rows} />
          </Suspense>
        </div>
        <section className="card recent">
          <div className="card-heading">
            <h2>Recent measurements</h2>
            <Link to="/measurements" aria-label="View all measurements">
              <ArrowUpRight size={16} />
            </Link>
          </div>
          {rows.slice(0, 5).map((m) => (
            <Link
              className="recent-row"
              key={m.id}
              to={`/measurements/${m.id}`}
            >
              <span>
                <small>{date(m.measured_at)}</small>
                <strong>{fmt(m.weight_kg, " kg")}</strong>
                <small>
                  {d.profiles.find((p) => p.id === m.profile_id)?.name ||
                    "Guest"}{" "}
                  · {fmt(m.body_fat_pct, "% fat")}
                </small>
              </span>
              <ArrowUpRight size={14} />
            </Link>
          ))}
          {!rows.length && (
            <Empty text="Your first measurement will appear here." />
          )}
          <p className="footnote">
            New scale measurements appear automatically.
          </p>
        </section>
      </div>
      {dialog && (
        <MeasurementForm
          defaultProfile={selected}
          onClose={() => setDialog(false)}
        />
      )}
    </>
  );
}
