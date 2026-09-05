# Grammatic development with Docker Compose

Requires Docker with Compose 2.32 or newer. Rust, Node, Postgres, and the Linux
D-Bus build libraries run in containers; no host toolchains or scale are needed.
Run these commands from this directory.

## Develop

```sh
docker compose up --build --watch
```

Open http://localhost:5173. Vite forwards `/api` to the Rust backend; the backend
is also available at http://localhost:8090/api/health. Start with an empty database
and create profiles and measurements through the dashboard. No household seed
records are loaded. Postgres data persists in the `database` named volume.

Compose Watch synchronizes frontend edits for Vite hot reload, restarts and
recompiles the backend after Rust or migration edits, and rebuilds after package
or dependency changes. The compiled Rust target stays in the development
container across restarts. Rebuilding the image replaces that container.
The watcher uses file transfer, not source bind mounts, so remote Docker engines
also work. See [Docker's Watch documentation](https://docs.docker.com/compose/how-tos/file-watch/).

For a detached stack without watching:

```sh
docker compose up --build --wait
docker compose logs -f backend
```

Use `GRAMMATIC_WEB_PORT=5174 GRAMMATIC_API_PORT=8091 docker compose up --build --watch`
if the default ports are occupied. Only loopback ports are published. With a
remote Docker context these belong to the remote machine; use SSH forwarding
(for example `ssh -L 5173:127.0.0.1:5173 -L 8090:127.0.0.1:8090 your-host`).

Stop with `docker compose down`. To intentionally erase development data, use
`docker compose down --volumes`. Applied SQL migrations are checksummed: add a
new migration for an existing database, or reset this disposable database when
editing an initial migration.

## Test

From the repository root:

```sh
./dev/test.sh
```

Each invocation creates its own Compose project and disposable Postgres database,
then removes its containers, network, and volumes even on failure. It never uses
the development database. The command exits nonzero when building or testing fails.

The frontend image runs all frontend tests and the production build. The test
container runs Rust tests with `TEST_DATABASE_URL` set, Clippy, then starts the
real dashboard with the built frontend and runs `tests/dashboard_api.py` against
it. This exercises database dedup, enrichment, profile corrections, recompute,
validation, deletion, and static route fallback without Bluetooth hardware.

To retain a failed test stack for inspection:

```sh
docker compose -f compose.test.yaml up --build --abort-on-container-exit --exit-code-from tests tests
docker compose -f compose.test.yaml logs tests
docker compose -f compose.test.yaml down --volumes
```

The fixed credentials and configurations here are for isolated development only.
Bluetooth capture remains a Linux host workflow; see the root README for setup.
