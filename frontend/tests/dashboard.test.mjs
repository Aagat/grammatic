import test from "node:test";
import assert from "node:assert/strict";
import { createDashboardData } from "../src/data/dashboard.js";

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function harness() {
  const calls = [];
  let tick;
  let cancelled = false;
  const dashboard = createDashboardData({
    request(path, options) {
      const call = { path, options, ...deferred() };
      calls.push(call);
      return call.promise;
    },
    schedule(callback, delay) { assert.equal(delay, 30000); tick = callback; return 1; },
    cancel(id) { assert.equal(id, 1); cancelled = true; },
  });
  const complete = (offset, profiles = [{ id: 1 }], measurements = [{ id: 10 }]) => {
    calls[offset].resolve(profiles);
    calls[offset + 1].resolve(measurements);
    calls[offset + 2].resolve({ database: "connected" });
  };
  return { dashboard, calls, complete, tick: () => tick(), cancelled: () => cancelled };
}
const flush = () => new Promise((resolve) => setImmediate(resolve));

test("loads one coherent snapshot and repairs selection after profile deletion", async () => {
  const h = harness();
  const first = h.dashboard.refresh();
  h.complete(0);
  assert.equal(await first, true);
  h.dashboard.setProfile("1");
  const next = h.dashboard.refresh();
  h.complete(3, []);
  await next;
  assert.equal(h.dashboard.getSnapshot().profile, "all");
  h.dashboard.setProfile("guest");
  assert.equal(h.dashboard.getSnapshot().profile, "guest");
});

test("older completion and older failure cannot replace a newer snapshot", async () => {
  const h = harness();
  const old = h.dashboard.refresh();
  const next = h.dashboard.refresh();
  h.complete(3, [{ id: 2 }]);
  await next;
  h.calls[0].reject(new Error("old failure"));
  await old;
  assert.equal(h.dashboard.getSnapshot().profiles[0].id, 2);
  assert.equal(h.dashboard.getSnapshot().error, "");
  const olderSuccess = h.dashboard.refresh();
  const newest = h.dashboard.refresh();
  h.complete(9, [{ id: 3 }]);
  await newest;
  h.complete(6, [{ id: 4 }]);
  assert.equal(await olderSuccess, false);
  assert.equal(h.dashboard.getSnapshot().profiles[0].id, 3);
});

test("failed refresh preserves data and a retry clears the error", async () => {
  const h = harness();
  const first = h.dashboard.refresh(); h.complete(0); await first;
  const failed = h.dashboard.refresh(); h.calls[3].reject(new Error("offline")); await failed;
  assert.equal(h.dashboard.getSnapshot().measurements[0].id, 10);
  assert.equal(h.dashboard.getSnapshot().error, "offline");
  const retry = h.dashboard.refresh(); h.complete(6); await retry;
  assert.equal(h.dashboard.getSnapshot().error, "");
});

test("a saved write with failed reload is never reported as a failed write", async () => {
  const h = harness();
  const save = h.dashboard.mutate("/measurements", "POST", { weight_kg: 70 });
  h.calls[0].resolve({ id: 1 }); await flush();
  h.calls[1].reject(new Error("offline"));
  assert.deepEqual(await save, { saved: true, refreshed: false });
  assert.match(h.dashboard.getSnapshot().error, /Changes were saved/);
  const retry = h.dashboard.refresh(); h.complete(4); await retry;
  assert.equal(h.calls.filter((c) => c.options?.method === "POST").length, 1);
  assert.equal(h.dashboard.getSnapshot().savedButStale, false);
});

test("a rejected write throws without changing the snapshot", async () => {
  const h = harness();
  const snapshot = h.dashboard.getSnapshot();
  const write = h.dashboard.mutate("/measurements", "POST", {});
  h.calls[0].reject(new Error("invalid weight"));
  await assert.rejects(write, /invalid weight/);
  assert.equal(h.dashboard.getSnapshot(), snapshot);
});

test("pending writes invalidate earlier reads and defer polling until writes settle", async () => {
  const h = harness();
  const old = h.dashboard.refresh();
  const one = h.dashboard.mutate("/measurements/1", "PUT", {});
  const two = h.dashboard.mutate("/measurements/2", "DELETE");
  assert.equal(await h.dashboard.refresh(), false);
  h.complete(0); assert.equal(await old, false);
  h.calls[3].resolve({}); await one;
  assert.equal(h.calls.length, 5);
  h.calls[4].resolve(null); await flush();
  h.complete(5, [], []);
  assert.deepEqual(await two, { saved: true, refreshed: true });
  assert.deepEqual(h.dashboard.getSnapshot().measurements, []);
});

test("polling cleans up and late responses cannot publish after stop", async () => {
  const h = harness();
  let updates = 0;
  h.dashboard.subscribe(() => updates++);
  h.dashboard.start(); h.complete(0); await flush();
  h.tick();
  h.dashboard.stop();
  h.complete(3); await flush();
  assert.equal(updates, 1);
  assert.equal(h.cancelled(), true);
  h.dashboard.start(); h.complete(6, [{ id: 2 }]); await flush();
  assert.equal(h.dashboard.getSnapshot().profiles[0].id, 2);
  h.dashboard.stop();
});

test("late write completion after stop does not refresh or publish", async () => {
  const h = harness();
  const write = h.dashboard.mutate("/profiles/1", "DELETE");
  h.dashboard.stop(); h.calls[0].resolve(null);
  assert.deepEqual(await write, { saved: true, refreshed: false });
  assert.equal(h.calls.length, 1);
  assert.equal(h.dashboard.getSnapshot().savedButStale, false);
});

test("initial load failure leaves loading state and can recover", async () => {
  const h = harness();
  const first = h.dashboard.refresh(); h.calls[0].reject(new Error("offline"));
  await first;
  assert.equal(h.dashboard.getSnapshot().loading, false);
  assert.equal(h.dashboard.getSnapshot().error, "offline");
  const retry = h.dashboard.refresh(); h.complete(3); await retry;
  assert.equal(h.dashboard.getSnapshot().error, "");
});

test("a concurrent rejected write still reloads a preceding successful write", async () => {
  const h = harness();
  const saved = h.dashboard.mutate("/profiles/1", "DELETE");
  const rejected = h.dashboard.mutate("/profiles/2", "PUT", {});
  const rejection = assert.rejects(rejected, /invalid profile/);
  h.calls[0].resolve(null); await saved;
  h.calls[1].reject(new Error("invalid profile")); await flush();
  h.complete(2, [], []); await rejection;
  assert.equal(h.dashboard.getSnapshot().savedButStale, false);
  assert.deepEqual(h.dashboard.getSnapshot().profiles, []);
});
