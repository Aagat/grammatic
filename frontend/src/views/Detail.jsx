import { useState } from "react";
import { Link, useParams, useNavigate } from "react-router-dom";
import {
  ArrowLeft,
  Activity,
  Scale,
  Droplets,
  Clock,
  UserRoundCheck,
  Trash2,
  Pencil,
  Copy,
  Bone,
  Dumbbell,
  Flame,
  Heart,
} from "lucide-react";
import { useData } from "../data/AppData.jsx";
import { fmt, date } from "../data/format.js";
import {
  Header,
  Empty,
  Confirm,
  Modal,
  ProfileSelect,
} from "../components/ui.jsx";
import { MeasurementForm } from "../components/Forms.jsx";

const composition = [
  ["Water", "water_pct", "%", Droplets],
  ["Muscle mass", "muscle_mass_kg", " kg", Dumbbell],
  ["Bone mass", "bone_mass_kg", " kg", Bone],
  ["Protein", "protein_pct", "%", Activity],
  ["Lean body mass", "lean_body_mass_kg", " kg", Scale],
  ["Visceral fat", "visceral_fat", "", Heart],
  ["BMR", "bmr_kcal", " kcal", Flame],
  ["Metabolic age", "metabolic_age", " years", Clock],
  ["Body type", "body_type", "", Activity],
];

function AssignmentDialog({ measurement, onClose }) {
  const { mutate } = useData();
  const [profile, setProfile] = useState(
    measurement.profile_id == null ? "guest" : String(measurement.profile_id),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  return (
    <Modal title="Change profile" onClose={onClose}>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError("");
          try {
            await mutate(`/measurements/${measurement.id}`, "PUT", {
              measured_at: measurement.measured_at,
              weight_kg: measurement.weight_kg,
              impedance_ohm: measurement.impedance_ohm,
              profile_id: profile === "guest" ? null : Number(profile),
            });
            onClose();
          } catch (e) {
            setError(e.message);
          } finally {
            setBusy(false);
          }
        }}
      >
        <label>
          Assigned profile
          <ProfileSelect value={profile} onChange={setProfile} />
        </label>
        <p className="footnote">
          Body metrics will be recalculated for the selected profile.
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
            {busy ? "Saving…" : "Save assignment"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function Detail() {
  const { id } = useParams();
  const d = useData();
  const navigate = useNavigate();
  const m = d.measurements.find((m) => String(m.id) === id);
  const [edit, setEdit] = useState(false);
  const [remove, setRemove] = useState(false);
  const [assign, setAssign] = useState(false);
  const [copyStatus, setCopyStatus] = useState("");
  if (!m)
    return (
      <Empty text="Measurement not found">
        <Link to="/measurements">Back to measurements</Link>
      </Empty>
    );
  const profile = d.profiles.find((p) => p.id === m.profile_id);
  const complete = composition.every(([, key]) => m[key] != null);
  const raw = m.raw_frame
    ?.match(/.{1,2}/g)
    ?.join(" ")
    .toUpperCase();
  const summaries = [
    [
      "Weight",
      fmt(m.weight_kg, " kg"),
      `Ideal weight ${fmt(m.ideal_weight_kg, " kg")}`,
      Scale,
    ],
    [
      "BMI",
      fmt(m.bmi),
      profile ? "Calculated from profile" : "Assign a profile to calculate",
      Activity,
    ],
    [
      "Body fat",
      fmt(m.body_fat_pct, "%"),
      m.body_fat_pct == null
        ? "Not available for this measurement"
        : "Estimated from impedance",
      Droplets,
    ],
    [
      "Measured",
      new Date(m.measured_at).toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
      }),
      new Date(m.measured_at).toLocaleDateString(undefined, {
        dateStyle: "medium",
      }),
      Clock,
    ],
  ];
  return (
    <div className="measurement-detail">
      <Header
        eyebrow="MEASUREMENTS / RECORD"
        title="Measurement details"
        description={`${date(m.measured_at)} · ${m.raw_frame ? "Mi Body Composition Scale 2" : "Manual entry"} · Record #${m.id}`}
      >
        <Link className="detail-button" to="/measurements">
          <ArrowLeft size={14} />
          All measurements
        </Link>
        <button onClick={() => setEdit(true)}>
          <Pencil size={13} />
          Edit
        </button>
        <button
          className="danger detail-delete"
          onClick={() => setRemove(true)}
        >
          <Trash2 size={14} />
          Delete measurement
        </button>
      </Header>
      <section className="record-summaries" aria-label="Measurement summary">
        {summaries.map(([label, value, meta, Icon]) => (
          <article className="record-summary" key={label}>
            <div>
              <span>{label}</span>
              <Icon size={14} aria-hidden="true" />
            </div>
            <strong>{value}</strong>
            <small>{meta}</small>
          </article>
        ))}
      </section>
      <section className="assignment-panel" aria-label="Profile assignment">
        <UserRoundCheck size={22} aria-hidden="true" />
        <div>
          <span>Assigned profile</span>
          <strong>{profile?.name || "Guest"}</strong>
        </div>
        <button onClick={() => setAssign(true)}>Change profile</button>
      </section>
      <div className="record-content">
        <section className="card composition-panel">
          <div className="card-heading">
            <div>
              <h2>Body composition</h2>
              <p>Calculated from weight and impedance</p>
            </div>
            <span
              className={`composition-status ${complete ? "is-complete" : ""}`}
            >
              {complete ? "Complete" : "Partial"}
            </span>
          </div>
          <div className="composition-grid">
            {composition.map(([label, key, unit, Icon]) => (
              <article key={key}>
                <div>
                  <span>{label}</span>
                  <Icon size={14} aria-hidden="true" />
                </div>
                <strong>
                  {key === "body_type"
                    ? m[key]?.replaceAll("_", " ") || "—"
                    : fmt(m[key], unit)}
                </strong>
              </article>
            ))}
          </div>
          <p className="footnote">
            Body composition values are estimates. Unavailable metrics are shown
            as —.
          </p>
        </section>
        <div className="record-column">
          <section className="card record-metadata">
            <div className="card-heading">
              <h2>Record details</h2>
              <span>#{m.id}</span>
            </div>
            <dl>
              {[
                ["Measured at", date(m.measured_at)],
                ["Received at", date(m.received_at)],
                [
                  "Clock source",
                  m.clock_source === "scale" ? "Scale" : "Receiver",
                ],
                ["Impedance", fmt(m.impedance_ohm, " Ω")],
                ["Signal", fmt(m.rssi, " dBm")],
                ["Unit", "Kilograms"],
              ].map(([label, value]) => (
                <div key={label}>
                  <dt>{label}</dt>
                  <dd>{value}</dd>
                </div>
              ))}
            </dl>
          </section>
          <section className="card raw-frame-panel">
            <div className="card-heading">
              <div>
                <h2>Raw frame</h2>
                <p>
                  {raw
                    ? `HEX · ${Math.ceil(m.raw_frame.length / 2)} BYTES`
                    : "MANUAL ENTRY"}
                </p>
              </div>
              {raw && (
                <button
                  aria-label="Copy raw frame"
                  onClick={async () => {
                    try {
                      await navigator.clipboard.writeText(m.raw_frame);
                      setCopyStatus("Copied");
                    } catch {
                      setCopyStatus(
                        "Could not copy. Select the frame text to copy it.",
                      );
                    }
                  }}
                >
                  <Copy size={14} />
                </button>
              )}
            </div>
            {raw ? (
              <code>{raw}</code>
            ) : (
              <p className="footnote">
                No Bluetooth frame is recorded for a manual measurement.
              </p>
            )}
            {copyStatus && (
              <p className="footnote" role="status">
                {copyStatus}
              </p>
            )}
          </section>
        </div>
      </div>
      {edit && (
        <MeasurementForm measurement={m} onClose={() => setEdit(false)} />
      )}
      {assign && (
        <AssignmentDialog measurement={m} onClose={() => setAssign(false)} />
      )}
      {remove && (
        <Confirm
          title="Delete measurement?"
          text="This permanently removes this measurement from your history."
          onClose={() => setRemove(false)}
          onConfirm={async () => {
            await d.mutate(`/measurements/${m.id}`, "DELETE");
            navigate("/measurements");
          }}
        />
      )}
    </div>
  );
}
