import { test } from "node:test";
import assert from "node:assert/strict";
import { dailyTrend, dayKey } from "../src/data/trends.js";
const row = (day, weight) => ({
  measured_at: `${day}T12:00:00`,
  weight_kg: weight,
});
test("daily average groups multiple measurements without inventing missing days", () => {
  const result = dailyTrend(
    [row("2026-09-01", 70), row("2026-09-01", 74), row("2026-09-03", 73)],
    30,
    new Date("2026-09-05T12:00:00"),
  );
  assert.deepEqual(
    result.map((r) => [r.day, r.weight]),
    [
      ["2026-09-01", 72],
      ["2026-09-03", 73],
    ],
  );
  assert.equal(result[1].average, 72.5);
});
test("moving average uses calendar days, not the previous seven observations", () => {
  const result = dailyTrend(
    [row("2026-08-01", 150), row("2026-09-01", 70), row("2026-09-05", 72)],
    90,
    new Date("2026-09-05T12:00:00"),
  );
  assert.equal(result.at(-1).average, 71);
});
test("first displayed day includes earlier days in its moving average", () => {
  const result = dailyTrend(
    [row("2026-09-01", 70), row("2026-09-05", 72)],
    1,
    new Date("2026-09-05T12:00:00"),
  );
  assert.equal(result.length, 1);
  assert.equal(result[0].average, 71);
});
test("empty and single measurements are supported", () => {
  assert.deepEqual(dailyTrend([], 30), []);
  assert.equal(
    dailyTrend([row("2026-09-05", 55)], 30, new Date("2026-09-05T12:00:00"))[0]
      .average,
    55,
  );
  assert.equal(dayKey(new Date(2026, 0, 2, 0, 5)), "2026-01-02");
});
