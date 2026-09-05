import { useState, useEffect, useRef } from "react";
import { Link } from "react-router-dom";
import { useData } from "../data/AppData.jsx";
import { fmt, date, filtered } from "../data/format.js";
import { Plus } from "lucide-react";
import { Header, Empty, Confirm } from "../components/ui.jsx";
import { ProfileForm } from "../components/Forms.jsx";

export function Profiles() {
  const d = useData();
  const [edit, setEdit] = useState(null);
  const [remove, setRemove] = useState(null);
  const guests = d.measurements.filter((m) => m.profile_id == null).length;
  return (
    <>
      <Header
        eyebrow="PEOPLE"
        title="Household profiles"
        description="People, automatic assignment, and body metrics."
      >
        <button className="primary" onClick={() => setEdit({})}>
          <Plus size={16} />
          Add profile
        </button>
      </Header>
      {guests > 0 && (
        <p className="notice">
          {guests} guest measurements.{" "}
          <Link to="/measurements" onClick={() => d.setProfile("guest")}>
            Review and assign →
          </Link>
        </p>
      )}
      <div className="profile-grid">
        {d.profiles.map((p) => (
          <article className="card" key={p.id}>
            <div className="profile-heading">
              <span className="avatar">{p.name.slice(0, 2).toUpperCase()}</span>
              <div>
                <h2>{p.name}</h2>
                <p>
                  {d.measurements.filter((m) => m.profile_id === p.id).length}{" "}
                  measurements
                </p>
              </div>
            </div>
            <dl>
              <div>
                <dt>Sex</dt>
                <dd>{p.sex}</dd>
              </div>
              <div>
                <dt>Height</dt>
                <dd>{fmt(p.height_cm, " cm")}</dd>
              </div>
              <div>
                <dt>Date of birth</dt>
                <dd>{p.dob}</dd>
              </div>
              <div>
                <dt>Weight window</dt>
                <dd>
                  {p.weight_min ?? "No min"} – {p.weight_max ?? "No max"}
                </dd>
              </div>
            </dl>
            <div className="form-actions">
              <button className="danger" onClick={() => setRemove(p)}>
                Delete
              </button>
              <button onClick={() => setEdit(p)}>Edit profile</button>
            </div>
          </article>
        ))}
      </div>
      {!d.profiles.length && (
        <Empty text="Add a household profile to calculate body metrics and assign measurements." />
      )}
      {edit && <ProfileForm profile={edit} onClose={() => setEdit(null)} />}
      {remove && (
        <Confirm
          title={`Delete ${remove.name}?`}
          text="Measurements will be retained as guest entries and their profile-derived metrics cleared."
          onClose={() => setRemove(null)}
          onConfirm={() => d.mutate(`/profiles/${remove.id}`, "DELETE")}
        />
      )}
    </>
  );
}
