# Grammatic

Captures weight and body-composition measurements from a **Xiaomi Mi Body Composition Scale 2 (XMTZC05HM)** over Bluetooth LE and records them directly into a **Postgres** database — no Xiaomi cloud, no phone app, no manual steps.

Workflow: step on the scale → the measurement is auto-recorded with its timestamp, impedance, assigned profile, computed body metrics (body fat, muscle, water, BMR, ...) and the raw frame. Deduplication is a database constraint, so re-broadcasts collapse and restarts are safe. The integrated web dashboard browses, corrects and deletes measurements, manages household profiles, and displays activity and weight trends. See [dashboard setup](docs/dashboard.md).

Design and data model: [docs/spec.md](docs/spec.md).

## How it works

The scale passively broadcasts a 13-byte body-composition frame (service data `0x181B`) while you weigh yourself and re-broadcasts the final result afterwards. The agent (intended to run on a machine with Bluetooth in range of the scale) watches advertisements with BlueZ, classifies the frame's embedded clock, assigns a profile by weight window, computes the Mi Fit / Holtek body metrics, and inserts the row. A database unique index on `(measured_at, weight, impedance)` makes duplicates impossible. Frame layout and formulas are documented in [docs/spec.md](docs/spec.md), following the reverse-engineering in [lolouk44/xiaomi_mi_scale](https://github.com/lolouk44/xiaomi_mi_scale).

The agent also takes over the scale's clock maintenance (previously done by the OpenScale app): when drift is detected it connects via GATT and writes the current time to the Current Time Service (`0x2A9C`-adjacent characteristic `0x2A2B`), only while the scale is idle.

## Binary releases

Download the Linux archive and `SHA256SUMS` from [GitHub Releases](https://github.com/Aagat/grammatic/releases).
The archive includes `grammatic`, the compiled dashboard, an example `config.toml`,
and deployment documentation. The architecture is part of the archive filename.

On Debian 12 or newer, install runtime dependencies with
`sudo apt-get install libdbus-1-3 libssl3 ca-certificates bluez` (newer distributions
may name the OpenSSL package `libssl3t64`). Binaries require glibc 2.36 or newer.
Postgres must be available; Bluetooth capture also needs a local Bluetooth adapter
and a running BlueZ service.

```sh
sha256sum -c SHA256SUMS
tar -xzf grammatic-v0.1.0-linux-x86_64.tar.gz
cd grammatic-v0.1.0-linux-x86_64
./grammatic --help
# Edit config.toml, then follow the database/profile setup below.
./grammatic serve --bind 127.0.0.1:8090
```

Run from the extracted directory so the default dashboard path resolves, or pass
`--frontend /absolute/path/to/frontend/dist/client` to `serve`.

### Publishing a release

GitHub Actions runs frontend and backend checks for pushes and pull requests.
To prepare an archive locally on Linux, build the dashboard and release binary,
then run `RELEASE_TAG=v0.1.0 bash scripts/package-release.sh`.
Upload the archive and `SHA256SUMS` to a GitHub release after CI succeeds.

## Setup

```sh
cargo build --release        # binary at target/release/grammatic
```

1. Create the database (or reuse an existing Postgres):
   ```sql
   CREATE USER grammatic WITH PASSWORD '...';
   CREATE DATABASE grammatic OWNER grammatic;
   ```
   Schema migrations run automatically on first start.
2. Edit [`config.toml`](config.toml): set `scale_mac` and `[database] url`.
3. Add profiles — they live **in the database**:
   ```sh
   grammatic profile add alice --sex female --height-cm 168 --dob 1994-05-01 --weight-min 40 --weight-max 80
   grammatic profile add bob   --sex male   --height-cm 173 --dob 1996-01-01 --weight-min 60 --weight-max 100
   ```
   A measurement matching exactly one window is assigned to that profile; zero matches are recorded with `profile_id = NULL` (guest). Overlapping windows break the tie against each candidate's most recent measurement (weight first, impedance second); ties and thin margins stay guest — never guessed.
4. Deploy as a service (homeserver): see [`deploy/grammatic.service`](deploy/grammatic.service).

## Usage

### 1. Find the scale (discover your scale’s MAC address)

```sh
grammatic find
```

Step on the scale briefly to wake it up first. The configured MAC is marked in the output.

### 2. Listen (the service mode)

```sh
grammatic listen          # runs until Ctrl+C; what the systemd unit runs
grammatic listen --once   # records one measurement, then exits (debugging)
```

Passive only — never connects for measurement, so nothing to pair. Stay on the scale until it shows the final weight; the agent records it.

### 3. Dashboard

```sh
grammatic serve --bind 127.0.0.1:8090  # UI + API; build frontend first
```

The dashboard uses the same Postgres database as capture. See [deployment and API](docs/dashboard.md).

### 4. Everything else

```sh
grammatic profile list
grammatic recompute --all                    # refresh metric columns after profile edits
grammatic sync-clock                         # set the scale's clock manually
grammatic fetch-history                      # recover weigh-ins whose final frame never arrived
grammatic replay frames.hex                  # dev: feed recorded frames through the pipeline
```

## Output schema

`measurements` table (see `migrations/0001_init.sql`): `measured_at` (validated scale clock, falls back to receive time), `clock_source`, `received_at`, `weight_kg`, `impedance_ohm`, `profile_id` (nullable), `unit`, `raw_frame` (hex), `rssi`, plus metric columns (`bmi`, `body_fat_pct`, `water_pct`, `muscle_mass_kg`, `bone_mass_kg`, `protein_pct`, `visceral_fat`, `bmr_kcal`, `metabolic_age`, `lean_body_mass_kg`, `ideal_weight_kg`, `body_type`). Impedance-derived columns are `NULL` when measured with socks; weight-only metrics (BMI, BMR, visceral fat, ideal weight) are always computed. `[metrics] store` in `config.toml` optionally trims stored columns (`all` / `weight-only` / `none`, default `all`).

## Development

For a containerized dashboard, source watching, and isolated database-backed tests,
see [the Docker Compose development setup](dev/README.md).

```sh
cargo test         # parser unit tests, 2,938-case golden metrics, clock/profile/spool logic,
                   # and the capture pipeline against an in-memory sink
cargo clippy --all-targets
```

`tests/dedup_integration.rs` verifies the DB dedup constraint end-to-end and
self-skips unless `TEST_DATABASE_URL` points at a disposable Postgres:

```sh
docker run --rm -d -p 5432:5432 -e POSTGRES_PASSWORD=mi postgres:16
TEST_DATABASE_URL=postgres://postgres:mi@localhost/postgres cargo test --test dedup_integration
```

The metrics golden fixtures (`tests/fixtures/metrics_golden.json`) are committed as a regression baseline; they use generated parameter combinations, and `examples/probe.rs` diagnoses BLE event delivery without the capture pipeline.

## Troubleshooting

- **Scale not found** — step on it to wake it from sleep, keep it within a few meters, check batteries.
- **BlueZ permission errors** — put the service user in the `bluetooth` group or `sudo setcap 'cap_net_raw,cap_net_admin+eip' /usr/local/bin/grammatic`.
- **No impedance recorded** — measure barefoot; impedance needs skin contact.
- **Wrong person recorded** — widen/narrow profile weight windows (`profile` + `recompute`), fix rows via SQL; guests have `profile_id = NULL`.
- **Clock drift warnings** — the agent fixes the clock automatically when the scale is idle; `grammatic sync-clock` forces it.
- **Missed weigh-in** (stepped off before the final weight, agent restarted mid-weigh-in) — live frames in the journal but no measurement: `grammatic fetch-history` pulls the scale's stored history over GATT (history-only: empty history records nothing, live frames are never synthesized). See `docs/adr/0002-gatt-history-pull.md`; the automatic fallback (`[history] auto_fetch`, default on) covers this in service mode. When the live row lands weight-only, the pull enriches it with impedance instead of inserting a second row.
- **`-d` flag** enables debug logging of every received frame.

Public repository maintenance: [release privacy guidance](docs/public-release.md).
