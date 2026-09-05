# Integrated dashboard

The React UI and Rust API are part of this repository. `grammatic serve` serves
both from one origin; `grammatic listen` remains the independent Bluetooth capture
process. Both use the same Postgres schema and metric calculation code.

## Run

On Linux, build the frontend with `cd frontend && npm ci && npm run build`,
then build the binary with `cargo build --release`. Run:

```sh
grammatic --config config.toml serve --bind 127.0.0.1:8090 --frontend frontend/dist/client
```

The config uses the existing `[database] url`. No Bluetooth adapter or D-Bus
session is needed for `serve`. The binary links libdbus, as the capture commands
do. Install `deploy/grammatic-web.service` alongside the capture unit for systemd.

For frontend development, `npm run dev` in `frontend` proxies `/api` to port 8090.
The production server supports direct navigation to every frontend route.

Alternatively build the combined image with `docker build -t grammatic .`.
Mount your configuration at `/app/config.toml` and publish port 8090 only to the
loopback interface or an existing trusted reverse-proxy network. The image runs
as an unprivileged user. It serves the dashboard, not Bluetooth capture.

The HTTP API has no independent login. Put **both the UI and `/api` behind the
home server's authenticated reverse proxy**. Do not publish the API directly to
the internet. The default native bind address is loopback; no CORS is enabled.
Responses containing household measurements use `Cache-Control: no-store`.

## Workflows

- Overview: one profile at a time, latest metrics, activity heatmap, daily weight
  averages, and a seven-calendar-day moving average over available daily values.
  Missing days are not invented. Browser-local dates group measurements.
- Heatmap: 3, 6, or 12 months depending on card width. The displayed range and
  count agree. Tap a recorded day for its count; hover also exposes the date.
- History: profile, date and text filters, actual totals, 20 measurements per
  page, and links to details.
- Manual entry and corrections: timestamp, weight, optional impedance, profile.
  Metrics are computed on the server and honor the configured storage policy.
  Original captured frames and receive metadata survive corrections.
- Profiles: create, edit, delete. Editing recomputes existing metrics without
  reassigning measurements. Deletion retains measurements as guests and clears
  their profile-derived metrics. Mutations and recomputation are transactional.
- Settings: actual database health, scale model, metric policy and display
  timezone. It does not pretend to know whether the sleeping scale is connected.
- Light/dark theme follows Northstar's palette and persists in this app's browser
  origin. The sidebar links back to the home dashboard.

The UI refreshes every 30 seconds and after writes. A successful write followed
by a failed refresh closes the editor and shows a saved-but-not-reloaded message;
Retry only reloads the snapshot, so it cannot resubmit the write. Older responses
cannot replace a newer snapshot, and deleted profile selections reset to All.
Empty and failed requests
have explicit states. It does not fall back to mock data. Initial loading fetches
all household measurements; filtering and pagination are client-side. This is
intended for household volumes, not a multi-tenant analytics service.

## API

`GET /api/health`, `GET/POST /api/profiles`, `PUT/DELETE /api/profiles/{id}`,
`GET/POST /api/measurements`, `PUT/DELETE /api/measurements/{id}`.

Writes accept JSON. Profile fields: `name`, `sex` (`male`/`female`), `height_cm`,
`dob` (YYYY-MM-DD), nullable `weight_min`, `weight_max`. Measurement fields:
`measured_at` (RFC3339 with offset), `weight_kg`, nullable `impedance_ohm`,
nullable `profile_id`. PUT supplies the complete editable representation.
Invalid inputs, missing records, and conflicts return non-2xx responses.

## Removed draft behavior

Invented household members, historical totals, weight trends, body composition,
scale connectivity, and the local-only save path were replaced with database
state. Body fat cannot be manually entered: it is derived consistently from
impedance and profile data. Unsupported hardware settings are informational.

## Validation

`npm test` in `frontend` covers dashboard synchronization (including concurrent
writes and out-of-order reads), calendar-day aggregation, and static hosting
packaging. Run `npm run build` first for the packaging checks. Run `cargo test` and `cargo clippy --all-targets` on Linux;
set `TEST_DATABASE_URL` to a disposable Postgres database to exercise database
integration tests. Against a running API connected to a disposable database:

```sh
python3 tests/dashboard_api.py http://127.0.0.1:8090
```

The smoke test creates and cleans its own records and verifies recomputation,
guest assignment, duplicates, invalid inputs and deletion semantics. Browser
verification and its scope are recorded in `frontend/design-qa.md`.
