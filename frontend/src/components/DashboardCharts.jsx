import { useState, useEffect, useRef } from "react";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import CalendarHeatmap from "react-calendar-heatmap";
import "react-calendar-heatmap/dist/styles.css";
import { dayKey, dailyTrend } from "../data/trends.js";
import { fmt } from "../data/format.js";
import { Empty } from "./ui.jsx";

export function Trend({ rows }) {
  const [days, setDays] = useState(90);
  const points = dailyTrend(rows, days);
  return (
    <section className="card">
      <div className="card-heading">
        <div>
          <h2>Weight trend</h2>
          <p>Daily average and 7-day moving average</p>
        </div>
        <select
          aria-label="Trend period"
          value={days}
          onChange={(e) => setDays(Number(e.target.value))}
        >
          {[30, 90, 365].map((n) => (
            <option key={n} value={n}>
              Last {n} days
            </option>
          ))}
        </select>
      </div>
      <div className="legend">
        <span>
          <i />
          Daily weight
        </span>
        <span>
          <i className="average" />
          7-day average
        </span>
      </div>
      {points.length ? (
        <div className="chart">
          <ResponsiveContainer width="100%" height="100%" minWidth={0}>
            <LineChart
              data={points}
              margin={{ top: 12, right: 15, bottom: 6, left: 0 }}
              accessibilityLayer
            >
              <CartesianGrid vertical={false} stroke="var(--border)" />
              <XAxis
                dataKey="day"
                tickFormatter={(d) =>
                  new Date(`${d}T12:00:00`).toLocaleDateString(undefined, {
                    month: "short",
                    day: "numeric",
                  })
                }
                minTickGap={35}
                tick={{ fontSize: 10, fill: "var(--muted)" }}
                tickLine={false}
                axisLine={false}
              />
              <YAxis
                width={45}
                domain={["auto", "auto"]}
                tick={{ fontSize: 10, fill: "var(--muted)" }}
                tickLine={false}
                axisLine={false}
                tickFormatter={(v) => fmt(v)}
              />
              <Tooltip
                contentStyle={{
                  background: "var(--surface)",
                  border: "1px solid var(--border)",
                  borderRadius: 8,
                }}
                formatter={(v, n) => [fmt(v, " kg"), n]}
                labelFormatter={(d) =>
                  new Date(`${d}T12:00:00`).toLocaleDateString(undefined, {
                    dateStyle: "medium",
                  })
                }
              />
              <Line
                name="Daily weight"
                dataKey="weight"
                stroke="var(--accent)"
                strokeWidth={2}
                dot={points.length < 35 ? { r: 3 } : false}
                isAnimationActive={false}
              />
              <Line
                name="7-day average"
                dataKey="average"
                stroke="var(--foreground)"
                strokeWidth={1.5}
                strokeDasharray="4 4"
                dot={false}
                isAnimationActive={false}
              />
            </LineChart>
          </ResponsiveContainer>
        </div>
      ) : (
        <Empty text="No measurements in this period." />
      )}
    </section>
  );
}

export function Heatmap({ rows }) {
  const ref = useRef(null);
  const [width, setWidth] = useState(600);
  const [selected, setSelected] = useState("");
  useEffect(() => {
    const observer = new ResizeObserver(([e]) => setWidth(e.contentRect.width));
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const months = width < 400 ? 3 : width < 650 ? 6 : 12;
  const end = new Date();
  end.setHours(12, 0, 0, 0);
  const start = new Date(end);
  start.setMonth(start.getMonth() - months);
  const counts = new Map();
  for (const m of rows) {
    const k = dayKey(m.measured_at);
    counts.set(k, (counts.get(k) || 0) + 1);
  }
  const values = [...counts]
    .filter(([k]) => k > dayKey(start) && k <= dayKey(end))
    .map(([date, count]) => ({ date, count }));
  return (
    <section className="card" ref={ref}>
      <div className="card-heading">
        <div>
          <h2>Measurement activity</h2>
          <p>Daily measurements · last {months} months</p>
        </div>
      </div>
      <div className="heatmap">
        <CalendarHeatmap
          startDate={start}
          endDate={end}
          values={values}
          gutterSize={3}
          showWeekdayLabels={width > 400}
          classForValue={(v) =>
            !v?.count ? "color-empty" : `color-scale-${Math.min(4, v.count)}`
          }
          titleForValue={(v) =>
            v?.date
              ? `${v.date}: ${v.count || 0} measurements`
              : "No measurement"
          }
          onClick={(v) =>
            setSelected(
              v?.date
                ? `${v.date} · ${v.count || 0} measurements`
                : "No measurement on this day",
            )
          }
        />
      </div>
      <div className="heatmap-footer">
        <span role="status">
          {selected ||
            `${values.reduce((n, v) => n + v.count, 0)} measurements in this period`}
        </span>
        <span className="legend">
          Less{" "}
          {[0, 1, 2, 3, 4].map((n) => (
            <i key={n} className={`heat-${n}`} />
          ))}{" "}
          More
        </span>
      </div>
    </section>
  );
}
