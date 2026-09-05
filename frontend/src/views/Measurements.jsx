import { useState, useEffect, useRef } from "react";
import { Link } from "react-router-dom";
import { useData } from "../data/AppData.jsx";
import { fmt, date, filtered } from "../data/format.js";
import { Plus, ArrowUpRight } from "lucide-react";
import { Header, ProfileSelect, Empty } from "../components/ui.jsx";
import { MeasurementForm } from "../components/Forms.jsx";

export function Measurements() {
  const d = useData();
  const [query, setQuery] = useState("");
  const [days, setDays] = useState("all");
  const [page, setPage] = useState(1);
  const [dialog, setDialog] = useState(false);
  const rows = filtered(d.measurements, d.profile).filter(
    (m) =>
      (days === "all" ||
        new Date(m.measured_at) >=
          new Date(Date.now() - Number(days) * 86400000)) &&
      `${date(m.measured_at)} ${m.weight_kg} ${d.profiles.find((p) => p.id === m.profile_id)?.name || "Guest"}`
        .toLowerCase()
        .includes(query.toLowerCase()),
  );
  useEffect(() => setPage(1), [query, days, d.profile]);
  const pages = Math.max(1, Math.ceil(rows.length / 20));
  const current = Math.min(page, pages);
  return (
    <>
      <Header
        eyebrow="MEASUREMENTS"
        title="Measurement history"
        description={`${d.measurements.length} measurements across your household`}
      >
        <button className="primary" onClick={() => setDialog(true)}>
          <Plus size={16} />
          Log measurement
        </button>
      </Header>
      <section className="card">
        <div className="filters">
          <input
            aria-label="Search measurements"
            placeholder="Search measurements…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <ProfileSelect all value={d.profile} onChange={d.setProfile} />
          <select
            aria-label="Date range"
            value={days}
            onChange={(e) => setDays(e.target.value)}
          >
            <option value="all">All time</option>
            <option value="30">Last 30 days</option>
            <option value="90">Last 90 days</option>
            <option value="365">Last year</option>
          </select>
        </div>
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                {[
                  "Measured at",
                  "Profile",
                  "Weight",
                  "Body fat",
                  "Source",
                  "",
                ].map((s, i) => (
                  <th key={i}>{s}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.slice((current - 1) * 20, current * 20).map((m) => (
                <tr key={m.id}>
                  <td>
                    <Link to={`/measurements/${m.id}`}>
                      {date(m.measured_at)}
                    </Link>
                  </td>
                  <td>
                    {d.profiles.find((p) => p.id === m.profile_id)?.name ||
                      "Guest"}
                  </td>
                  <td>
                    <strong>{fmt(m.weight_kg, " kg")}</strong>
                  </td>
                  <td>{fmt(m.body_fat_pct, "%")}</td>
                  <td>
                    <span className="chip">
                      {m.raw_frame ? "Scale" : "Manual"}
                    </span>
                  </td>
                  <td>
                    <Link
                      aria-label={`Open measurement ${m.id}`}
                      to={`/measurements/${m.id}`}
                    >
                      <ArrowUpRight size={16} />
                    </Link>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {!rows.length && <Empty text="No measurements match these filters." />}
        <div className="pagination">
          <span>
            {rows.length
              ? `${(current - 1) * 20 + 1}–${Math.min(current * 20, rows.length)}`
              : "0"}{" "}
            of {rows.length}
          </span>
          <div>
            <button
              disabled={current === 1}
              onClick={() => setPage(current - 1)}
            >
              Previous
            </button>
            <span>
              {current} / {pages}
            </span>
            <button
              disabled={current === pages}
              onClick={() => setPage(current + 1)}
            >
              Next
            </button>
          </div>
        </div>
      </section>
      {dialog && <MeasurementForm onClose={() => setDialog(false)} />}
    </>
  );
}
