import { useState, useEffect, useRef } from "react";
import { Link } from "react-router-dom";
import { useData } from "../data/AppData.jsx";
import { fmt, date, filtered } from "../data/format.js";
import { Modal, ProfileSelect } from "./ui.jsx";
import { dayKey } from "../data/trends.js";

export function MeasurementForm({ measurement, defaultProfile, onClose }) {
  const d = useData();
  const initial = measurement ? new Date(measurement.measured_at) : new Date();
  const local = new Date(
    initial.getTime() - initial.getTimezoneOffset() * 60000,
  )
    .toISOString()
    .slice(0, 16);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [profile, setProfile] = useState(
    measurement
      ? measurement.profile_id == null
        ? "guest"
        : String(measurement.profile_id)
      : defaultProfile || (d.profile === "all" ? "guest" : d.profile),
  );
  async function submit(e) {
    e.preventDefault();
    setBusy(true);
    setError("");
    const f = new FormData(e.currentTarget);
    try {
      await d.mutate(
        `/measurements${measurement ? `/${measurement.id}` : ""}`,
        measurement ? "PUT" : "POST",
        {
          measured_at: new Date(f.get("time")).toISOString(),
          weight_kg: Number(f.get("weight")),
          impedance_ohm: f.get("impedance") ? Number(f.get("impedance")) : null,
          profile_id: profile === "guest" ? null : Number(profile),
        },
      );
      onClose();
    } catch (e) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      title={measurement ? "Edit measurement" : "Log measurement"}
      onClose={onClose}
    >
      <form onSubmit={submit}>
        <label>
          Profile
          <ProfileSelect value={profile} onChange={setProfile} />
        </label>
        <label>
          Measured at
          <input
            name="time"
            type="datetime-local"
            required
            defaultValue={local}
          />
        </label>
        <div className="form-grid">
          <label>
            Weight (kg)
            <input
              name="weight"
              type="number"
              min="1"
              max="300"
              step="0.01"
              required
              defaultValue={measurement?.weight_kg}
            />
          </label>
          <label>
            Impedance (Ω, optional)
            <input
              name="impedance"
              type="number"
              min="1"
              max="3000"
              defaultValue={measurement?.impedance_ohm ?? ""}
            />
          </label>
        </div>
        <p className="footnote">
          Body metrics are calculated from the selected profile and measurement.
          Leave impedance blank for weight-only entries.
        </p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <div className="form-actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" disabled={busy}>
            {busy ? "Saving…" : "Save measurement"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function ProfileForm({ profile: p, onClose }) {
  const d = useData();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  return (
    <Modal title={p.id ? "Edit profile" : "Add profile"} onClose={onClose}>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          const f = new FormData(e.currentTarget);
          const body = Object.fromEntries(f);
          for (const key of ["height_cm", "weight_min", "weight_max"])
            body[key] = body[key] === "" ? null : Number(body[key]);
          setBusy(true);
          try {
            await d.mutate(
              `/profiles${p.id ? `/${p.id}` : ""}`,
              p.id ? "PUT" : "POST",
              body,
            );
            onClose();
          } catch (e) {
            setError(e.message);
          } finally {
            setBusy(false);
          }
        }}
      >
        <label>
          Name
          <input name="name" required maxLength={100} defaultValue={p.name} />
        </label>
        <div className="form-grid">
          <label>
            Sex
            <select name="sex" defaultValue={p.sex || "male"}>
              <option value="male">Male</option>
              <option value="female">Female</option>
            </select>
          </label>
          <label>
            Height (cm)
            <input
              name="height_cm"
              type="number"
              min="30"
              max="220"
              step="0.1"
              required
              defaultValue={p.height_cm}
            />
          </label>
        </div>
        <label>
          Date of birth
          <input
            name="dob"
            type="date"
            min="1900-01-01"
            max={dayKey(new Date())}
            required
            defaultValue={p.dob}
          />
        </label>
        <div className="form-grid">
          <label>
            Minimum weight (kg)
            <input
              name="weight_min"
              type="number"
              min="0"
              max="300"
              step="0.1"
              defaultValue={p.weight_min ?? ""}
            />
          </label>
          <label>
            Maximum weight (kg)
            <input
              name="weight_max"
              type="number"
              min="0"
              max="300"
              step="0.1"
              defaultValue={p.weight_max ?? ""}
            />
          </label>
        </div>
        <p className="footnote">
          Optional exclusive weight bounds guide automatic assignment.
          Overlapping windows use measurement history; uncertain matches stay
          guest. Editing a profile recomputes its body metrics.
        </p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <div className="form-actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" disabled={busy}>
            {busy ? "Saving…" : "Save profile"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
