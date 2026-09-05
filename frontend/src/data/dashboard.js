// Dashboard synchronization owns snapshot ordering, selection, and write/reload
// completion. React and tests cross this interface with different transports.
export async function requestJson(path, options = {}) {
  const response = await fetch(`/api${path}`, {
    ...options,
    headers: { "Content-Type": "application/json", ...options.headers },
  });
  if (!response.ok) {
    let message = "The server could not complete this request.";
    try {
      message = (await response.json()).error || message;
    } catch {}
    throw new Error(message);
  }
  return response.status === 204 ? null : response.json();
}

export function createDashboardData({
  request = requestJson,
  schedule = setInterval,
  cancel = clearInterval,
} = {}) {
  let state = {
    profiles: [], measurements: [], health: null,
    loading: true, error: "", profile: "all", savedButStale: false,
  };
  const listeners = new Set();
  let generation = 0;
  let lifecycle = 0;
  let pendingWrites = 0;
  let timer;
  const publish = (changes) => {
    state = { ...state, ...changes };
    listeners.forEach((listener) => listener());
  };
  const validProfile = (profile, profiles) =>
    ["all", "guest"].includes(profile) || profiles.some((p) => String(p.id) === profile);

  const refresh = async () => {
    // A snapshot read during a write could put the pre-write state back on screen.
    if (pendingWrites) return false;
    const current = ++generation;
    try {
      const [profiles, measurements, health] = await Promise.all([
        request("/profiles"), request("/measurements"), request("/health"),
      ]);
      if (current !== generation) return false;
      publish({
        profiles, measurements, health, loading: false, error: "", savedButStale: false,
        profile: validProfile(state.profile, profiles) ? state.profile : "all",
      });
      return true;
    } catch (error) {
      if (current !== generation) return false;
      publish({
        loading: false,
        error: state.savedButStale
          ? "Changes were saved, but the latest data could not be loaded. Retry refreshing."
          : error.message || "Could not load the latest data.",
      });
      return false;
    }
  };

  return {
    getSnapshot: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    setProfile(profile) {
      profile = String(profile);
      publish({ profile: validProfile(profile, state.profiles) ? profile : "all" });
    },
    refresh,
    async mutate(path, method, body) {
      const currentLifecycle = lifecycle;
      ++generation;
      ++pendingWrites;
      try {
        await request(path, { method, body: body == null ? undefined : JSON.stringify(body) });
      } catch (error) {
        --pendingWrites;
        // Another concurrent write may have succeeded while this one failed.
        if (!pendingWrites && currentLifecycle === lifecycle && state.savedButStale) await refresh();
        throw error;
      }
      --pendingWrites;
      if (currentLifecycle !== lifecycle) return { saved: true, refreshed: false };
      publish({ savedButStale: true });
      return { saved: true, refreshed: await refresh() };
    },
    start() {
      if (timer !== undefined) return;
      void refresh();
      timer = schedule(() => void refresh(), 30000);
    },
    stop() {
      if (timer !== undefined) cancel(timer);
      timer = undefined;
      ++generation;
      ++lifecycle;
    },
  };
}
