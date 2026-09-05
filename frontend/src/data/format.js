export const fmt = (n, unit = "") =>
  n == null
    ? "—"
    : `${Number(n).toLocaleString(undefined, { maximumFractionDigits: 1 })}${unit}`;
export const date = (value) =>
  new Date(value).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });

export function filtered(rows, profile) {
  return rows.filter(
    (m) =>
      profile === "all" ||
      (profile === "guest"
        ? m.profile_id == null
        : String(m.profile_id) === profile),
  );
}
