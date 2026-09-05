import { useState, useEffect, useRef } from "react";
import { Link } from "react-router-dom";
import { useData } from "../data/AppData.jsx";
import { fmt, date, filtered } from "../data/format.js";
import { Header } from "../components/ui.jsx";

export function Preferences() {
  const d = useData();
  return (
    <>
      <Header
        eyebrow="SETTINGS"
        title="Your workspace"
        description="Capture and storage information."
      />
      <section className="card">
        <h2>Scale & storage</h2>
        <dl className="settings">
          <div>
            <dt>Scale model</dt>
            <dd>{d.health?.scale || "Unavailable"}</dd>
          </div>
          <div>
            <dt>Database</dt>
            <dd>
              {d.error ? "Unavailable" : d.health?.database || "Unavailable"}
            </dd>
          </div>
          <div>
            <dt>Units</dt>
            <dd>Kilograms · centimetres</dd>
          </div>
          <div>
            <dt>Metric storage</dt>
            <dd>{d.health?.metrics_policy || "Unavailable"}</dd>
          </div>
          <div>
            <dt>Display timezone</dt>
            <dd>{Intl.DateTimeFormat().resolvedOptions().timeZone}</dd>
          </div>
        </dl>
        <p className="footnote">
          The capture service manages Bluetooth independently. Database
          connectivity does not indicate whether the scale is awake. Capture
          settings are managed in the server configuration.
        </p>
      </section>
    </>
  );
}
