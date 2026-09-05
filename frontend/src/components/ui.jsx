import { useState, useEffect, useRef } from "react";
import { Scale, X } from "lucide-react";
import { useData } from "../data/AppData.jsx";

export function Header({ eyebrow, title, description, children }) {
  return (
    <header className="page-header">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h1>{title}</h1>
        <p className="description">{description}</p>
      </div>
      <div className="actions">{children}</div>
    </header>
  );
}

export function Empty({ text, children }) {
  return (
    <div className="empty">
      <Scale size={24} />
      <p>{text}</p>
      {children}
    </div>
  );
}

export function ProfileSelect({ value, onChange, all = false }) {
  const { profiles } = useData();
  return (
    <select
      aria-label="Profile"
      value={value}
      onChange={(e) => onChange(e.target.value)}
    >
      {all && <option value="all">All profiles</option>}
      <option value="guest">Guest</option>
      {profiles.map((p) => (
        <option key={p.id} value={String(p.id)}>
          {p.name}
        </option>
      ))}
    </select>
  );
}

export function Modal({ title, onClose, children }) {
  const ref = useRef(null);
  useEffect(() => {
    const previous = document.activeElement;
    ref.current.showModal();
    return () => previous?.focus();
  }, []);
  return (
    <dialog
      aria-label={title}
      ref={ref}
      onCancel={onClose}
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
    >
      <div className="modal-heading">
        <h2>{title}</h2>
        <button onClick={onClose} aria-label="Close dialog">
          <X size={18} />
        </button>
      </div>
      {children}
    </dialog>
  );
}

export function Confirm({ title, text, onConfirm, onClose }) {
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  return (
    <Modal title={title} onClose={onClose}>
      <p>{text}</p>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="form-actions">
        <button onClick={onClose}>Cancel</button>
        <button
          className="danger"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await onConfirm();
              onClose();
            } catch (e) {
              setError(e.message);
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Deleting…" : "Delete permanently"}
        </button>
      </div>
    </Modal>
  );
}
