# Grammatic — Spec & Design

Status: draft for sign-off

## 1. Goals

- **Core workflow**: step on the scale → the measurement is automatically recorded in the
  homeserver's Postgres database. No phone app, no cloud, no manual steps.
- **Single place for the data**: one Postgres instance, directly queryable and joinable with
  data from other sources.
- **Integrated management**: capture, storage, and a simple household dashboard live in this project.
- Runs as a native systemd service on the homeserver (has Bluetooth, within BLE range of the
  scale). Rust, robustness-first; single static binary, no runtime VM.

## 2. Scope

Browsing, profile management, and measurement corrections are supported by the
integrated dashboard; see [dashboard.md](dashboard.md).

Non-goals:
- Receiving measurements over GATT — passive advertisements only, except the narrow
  history-recovery exception (§6, §8b). Advertisements remain the primary path; GATT pulls
  are read-only recovery, and GATT writes stay clock-sync only (§8).
- Non-Linux platforms.
- Coexisting with the OpenScale phone app — the agent becomes the scale's sole consumer and
  takes over clock maintenance (§8).

## 3. Architecture

One binary, `grammatic`, on the homeserver:

```
BLE advertisements (BlueZ/D-Bus via bluer, tokio)
        │  filter: scale MAC, service data 0x181B
        ▼
   frame parser (pure)
        ▼
   profile assignment (weight-range lookup, DB)
        ▼
   metrics (pure, Holtek formulas)
        ▼
   INSERT … ON CONFLICT DO NOTHING  ── unreachable ─▶  spool file (hex lines, capped)
        ▲                                              │
        └────────────── replay on reconnect ───────────┘
```

- systemd system service, `Restart=always`, logs to journald via `tracing`.
- BlueZ access: service user in the `bluetooth` group; binary gets
  `setcap 'cap_net_raw,cap_net_admin+eip'`.
- Migrations run on service start (sqlx).

## 4. Data model (Postgres)

```sql
CREATE TABLE profiles (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    sex         TEXT NOT NULL CHECK (sex IN ('male', 'female')),
    height_cm   DOUBLE PRECISION NOT NULL CHECK (height_cm BETWEEN 30 AND 220),
    dob         DATE NOT NULL,
    weight_min  DOUBLE PRECISION,   -- assignment window, exclusive bounds; NULL = unbounded
    weight_max  DOUBLE PRECISION,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE measurements (
    id                 BIGSERIAL PRIMARY KEY,
    measured_at        TIMESTAMPTZ NOT NULL,      -- scale clock (validated) or received_at
    clock_source       TEXT NOT NULL,             -- 'scale' | 'receiver'
    received_at        TIMESTAMPTZ NOT NULL,
    weight_kg          DOUBLE PRECISION NOT NULL,
    impedance_ohm      INTEGER CHECK (impedance_ohm BETWEEN 1 AND 3000),
    profile_id         BIGINT REFERENCES profiles(id),   -- NULL = guest / ambiguous
    unit               TEXT NOT NULL,             -- 'kg' | 'lbs' | 'jin'
    raw_frame          TEXT NOT NULL,             -- hex of the 13/10-byte frame
    rssi               SMALLINT,
    -- computed at capture (nullable; present when impedance is present, except the
    -- weight-only four, which are always computed):
    bmi                DOUBLE PRECISION,
    bmr_kcal           DOUBLE PRECISION,
    visceral_fat       DOUBLE PRECISION,
    ideal_weight_kg    DOUBLE PRECISION,
    body_fat_pct       DOUBLE PRECISION,
    water_pct          DOUBLE PRECISION,
    bone_mass_kg       DOUBLE PRECISION,
    muscle_mass_kg     DOUBLE PRECISION,
    protein_pct        DOUBLE PRECISION,
    lean_body_mass_kg  DOUBLE PRECISION,
    metabolic_age      INTEGER,
    body_type          TEXT
);

-- Dedup. impedance is nullable, so use a COALESCE expression index
-- (plain UNIQUE treats NULLs as distinct and would admit duplicates).
CREATE UNIQUE INDEX measurements_dedup
    ON measurements (measured_at, weight_kg, COALESCE(impedance_ohm, -1));
```

### Dedup semantics

The scale re-broadcasts the identical final frame (same embedded timestamp) after a weigh-in.
Dedup is a DB unique constraint on `(measured_at, weight, impedance)`: re-broadcasts collapse,
distinct weigh-ins survive — and dedup survives restarts and is correct for `--once` and service
mode alike. Stabilized frames are inserted with `ON CONFLICT DO NOTHING`; unstable (live) frames
are never written.

Weight-scale `0x181D` frames carry no timestamp: `measured_at = received_at`,
`clock_source = 'receiver'`. The Body Composition Scale 2 normally only sends `0x181B`.
For dedup stability,
receiver-clocked `measured_at` is truncated to the minute (scale-clocked values are already
minute-exact), so re-broadcasts of one physical measurement always share the key.

## 5. Crates / dependencies

| Purpose      | Crate                                      |
| ------------ | ------------------------------------------ |
| async runtime| `tokio`                                    |
| BLE          | `bluer` (native BlueZ D-Bus; Linux-only)   |
| DB           | `sqlx` (postgres, runtime-tokio, migrations)|
| CLI          | `clap` (derive)                            |
| config       | `serde` + `toml`                           |
| time         | `chrono`                                   |
| logging      | `tracing` + `tracing-subscriber` (journald-friendly) |
| errors       | `thiserror` (lib) / `anyhow` (bin)         |

Single binary crate, `lib.rs` + `main.rs` split so integration tests exercise the library:
modules `parser`, `metrics`, `profile`, `store`, `ble`, `clocksync`, `spool`, `config`, `cli`.

## 6. CLI surface

```
grammatic listen [--once]        # service mode (default of the systemd unit); --once exits
                               # after the first stabilized measurement (debugging)
grammatic find [--scan-seconds]  # scan and print candidate scales (setup helper)
grammatic profile add|list|remove # profile management (profiles live in the DB, §7)
grammatic recompute [--all]      # recompute metric columns from current profiles (§9)
grammatic sync-clock             # manual clock sync (normally automatic, §8)
grammatic fetch-history          # pull stored weigh-ins over GATT (§8b)
grammatic replay <file>          # dev tool: feed recorded frame hexes through the pipeline
```

Dashboard queries and edits use the integrated HTTP API. Global flags: `--config`, `--debug`.

## 7. Profiles & assignment

- Profiles live **in the DB** (they are data, not config), managed via the `profile` subcommand.
- On each stabilized measurement: weight windows (`weight_min`, `weight_max`,
  exclusive) are the guardrails. Zero matches → `profile_id = NULL` (guest),
  a single match wins with no further I/O. Overlapping windows are the only
  case that consults history: each candidate's most recent measurement
  strictly before the current one scores the fit (weight first, impedance
  second); a tie, a thin margin (< 0.3 kg-equivalent), or missing history
  stays guest, never a guess. Logged at info/debug. Never guess.
- History is bounded (newest 5 per candidate) and strictly `< measured_at`,
  so replay sees the identical prefix regardless of replay wall-clock time;
  history failure degrades to guest and never blocks the measurement.
- New installations start without profile history. Configure exclusive weight windows
  or assign initial measurements through the dashboard before relying on overlap resolution.
- Dashboard profile fixes recompute metrics transactionally; direct SQL edits require `grammatic recompute --all` (§9).
  Recompute never re-assigns: the stored `profile_id` is pinned, only the
  metric columns are re-derived.

## 8. Clock integrity & sync

- Frame bytes 2–7 carry the scale's own clock. Validation: plausible year (2000–2099) **and**
  within ±24 h of receive time. On implausible timestamps: `measured_at = received_at`,
  `clock_source = 'receiver'`.
- When drift exceeds `drift_threshold_sec` (default 120 s), the agent queues a clock sync:
  connect via GATT, write the current time to the **Current Time Service (0x2A2B)** — the same
  mechanism the OpenScale app uses — then disconnect. Sync is only attempted when the scale is
  idle (no live frames for ≥ 30 s), with exponential backoff on failure. `measured_at` keeps
  using the fallback until a post-sync frame confirms the fix.
- If OpenScale never set the clock, the first sync may need the manual `sync-clock` once.

## 8b. History recovery (GATT pull)

- When live frames were seen but the stabilized frame never arrived (agent restarted
  mid-weigh-in, advertisement missed), the agent asks the scale for its stored history over
  GATT: Body Composition History characteristic `00002a2f-0000-3512-2118-0009af100700`
  (WRITE + NOTIFY, under service `0x181B`) — `0x01 + u32 device-id` size probe, `0x02`
  fetch, `0x03` stop, `0x04 + u32 device-id` ack. Protocol validated live on
  the XMTZC05HM 2026-09-03 (see `docs/adr/0002-gatt-history-pull.md` for protocol details); `auto_fetch` defaults on since the 2026-09-03/04 soak
  validated the fallback end to end (recovered the 11:31 weigh-in's
  impedance 13 min after its weight-only live row).
- Each 13-byte entry (byte-identical frame; the seconds byte in byte 8 is
  dropped from the timestamp) is a stabilized record by construction, so it
  feeds through the normal capture path: clock decision, profile assignment, metrics,
  dedup, spool. Seconds never enter the dedup key — history and live share one key, and
  re-delivery collapses.
- History-only invariant: a pull that yields nothing (connect fails, scale asleep, history
  empty) records nothing. Live frames are never synthesized into measurements, not even
  behind a flag. Malformed entries are skipped + counted, as in spool replay.
- `grammatic fetch-history` pulls on demand (works regardless of `auto_fetch`) and prints
  `fetched N / recorded M` (M ≤ N via dedup collapse). The automatic fallback in `listen`
  (gated by `[history] enabled` + `auto_fetch`, default on) arms on live frames and fires
  after `quiet_timeout_secs` (default 75 s, strictly longer than the 30 s clock-sync idle
  window) with no advertisement at all — measured on raw sightings, not filtered frames
  (the filter suppresses identical re-broadcasts, so filtered silence while the user stands
  still is not scale silence) — and disarms on any stabilized outcome or empty pull.
  Every pull outcome is journaled (recovered N, no new entries, all already
  recorded, or failure); failures back off exponentially; GATT pulls and clock
  writes serialize (one GATT session at a time, never during a weigh-in).
- One weigh-in, one row: the live stabilized frame carries no impedance of
  its own, so when the pull delivers the same minute+weight *with* impedance,
  the pull enriches the weight-only sibling row (impedance, profile, metrics,
  raw frame become the entry's; the original `received_at`/`rssi` win) instead
  of inserting a second row. A weight-only frame arriving after its impedance
  sibling collapses the same way. A conflicting non-NULL impedance stays a
  distinct row — genuinely odd evidence that must not merge silently.
- The `u32` device-id is a per-agent random id persisted at `[history] device_id_file`
  (default `/var/lib/grammatic/device_id`); the scale tracks the history position per id.
  Re-delivery is harmless via dedup.

## 9. Metrics

- Holtek/Mi Fit formulas implemented in `metrics.rs` as pure functions.
  Store doubles rounded to 2 dp (`metabolic_age` as integer, `body_type` as label text).
- Weight-only metrics (BMI, BMR, visceral fat, ideal weight) always computed;
  impedance-derived metrics only when impedance present.
- Storage is optional via `[metrics] store = "all" | "weight-only" | "none"`
  (default `"all"`). Trimmed storage only affects persistence: capture still
  computes transient metrics for the history tie-break (§7), and `recompute`
  backfills under the current setting. No migration — the columns are
  already nullable.
- Profile age is evaluated as of the **measurement date** (the validated scale
  clock's date, or the receive date when the scale clock isn't trusted — see
  `CONTEXT.md` and `docs/adr/0001`). Compute-time aging would make
  `recompute --all` non-idempotent; the measurement date keeps it
  deterministic.
- `recompute` reruns the library over stored rows using current profile data — used after the
  dashboard edits profiles or reassigns measurements. Raw frames are retained precisely so
  nothing is ever lost to formula/profile changes.

## 10. Spool (DB outage)

When Postgres is unreachable: stabilized frames append as hex lines to a capped spool file
(default 5 MB, oldest dropped first). On reconnect, spool is replayed before live inserts, then
truncated. ~30 lines of code, not a subsystem.

## 11. Config (TOML)

```toml
scale_mac = "00:00:00:00:00:00"

[database]
url = "postgres://grammatic@localhost/grammatic"   # password via PGPASSWORD or ~/.pgpass

[listen]
spool_path  = "/var/lib/grammatic/spool.hex"
spool_max_bytes = 5_242_880

[clock_sync]
enabled            = true
drift_threshold_sec = 120

[metrics]
store = "all"   # or "weight-only" (4 weight-only columns) / "none" (no metrics)

[history]
enabled            = true
auto_fetch         = true    # validated by the 2026-09-03/04 soak (§8b)
quiet_timeout_secs = 75
device_id_file     = "/var/lib/grammatic/device_id"
```

Note: no `dedup_window_sec` — dedup is enforced by the DB constraint alone.

## 12. Testing strategy

- **Parser**: unit tests plus synthetic protocol frames as fixtures.
- **Metrics**: golden tests — a matrix of (weight, impedance, sex, height, age) inputs with
  committed expected outputs, asserted with float tolerance 1e-9 (document any rounding
  divergence).
- **Profile assignment & dedup**: pure-logic unit tests; DB behavior exercised in an
  integration test against a disposable Postgres (docker) where available.
- **Replay**: `grammatic replay` drives the whole pipeline without hardware — this dev machine
  has no scale in range; end-to-end weigh-in tests happen on the homeserver.

## 13. Operational notes

- Units: parser converts lbs/jin to kg before storing; `unit` column preserves the original.
- `measured_at` interpreted in the homeserver's local timezone (scale clock carries no tz info).
- One scale, one agent instance assumed; `ON CONFLICT` makes an accidental second instance
  (e.g., `--once` while the service runs) harmless.

## 14. Implementation status

1. ~~Scaffold the cargo project; `parser` + `metrics` with golden tests~~ — done.
2. ~~BLE listener against the real scale~~ — done; validated live on the dev machine with real
   weigh-ins (two bugs found and fixed: BLE event delivery, dedup-key stability).
3. ~~DB layer, profile assignment, spool, service wiring~~ — done; integration-tested against a
   disposable local Postgres.
4. ~~Clock sync~~ — implemented; drift detection validated with real frames (the scale clock was
   correct within minute resolution).
5. ~~systemd unit + README~~ — done.
   **Remaining:** deploy the unit + Postgres setup on the homeserver.

## 15. Open items (for sign-off)

- Service user & hardening details (dedicated user vs. login user) — decided at deployment.
- Whether external consumers need the metric formulas as a library crate dependency instead of
  invoking `grammatic recompute` — the lib.rs split supports either; decide when it exists.
