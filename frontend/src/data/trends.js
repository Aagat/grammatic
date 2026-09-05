export const dayKey = (value) => {
  const d = new Date(value);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};
export function dailyTrend(rows, days, now = new Date()) {
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  start.setDate(start.getDate() - days + 1);
  const averageStart = new Date(start);
  averageStart.setDate(averageStart.getDate() - 6);
  const grouped = new Map();
  for (const m of rows) {
    if (new Date(m.measured_at) < averageStart) continue;
    const key = dayKey(m.measured_at);
    const list = grouped.get(key) || [];
    list.push(m.weight_kg);
    grouped.set(key, list);
  }
  const points = [...grouped]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([day, weights]) => ({
      day,
      weight: weights.reduce((a, b) => a + b, 0) / weights.length,
    }));
  return points
    .map((p) => {
      const end = new Date(`${p.day}T12:00:00`);
      const begin = new Date(end);
      begin.setDate(begin.getDate() - 6);
      const window = points.filter(
        (q) => q.day >= dayKey(begin) && q.day <= p.day,
      );
      return {
        ...p,
        average: window.reduce((a, b) => a + b.weight, 0) / window.length,
      };
    })
    .filter((p) => p.day >= dayKey(start));
}
