#!/usr/bin/env bash
set -euo pipefail
: "${TEST_DATABASE_URL:?The test database URL must be set}"
test_port=${TEST_HTTP_PORT:-8090}
test_config=$(mktemp)
python3 - "$test_config" <<'PYCONFIG'
import json, os, sys
with open(sys.argv[1], "w") as config:
    config.write("[database]\nurl = " + json.dumps(os.environ["TEST_DATABASE_URL"]) + "\n")
PYCONFIG
trap 'rm "$test_config"' EXIT
cargo test --locked
cargo clippy --locked --all-targets
cargo build --locked
./target/debug/grammatic --config "$test_config" serve --bind "127.0.0.1:$test_port" &
backend_pid=$!
trap 'kill "$backend_pid" 2>/dev/null || true; wait "$backend_pid" 2>/dev/null || true; rm "$test_config"' EXIT
for attempt in {1..30}; do
  if curl --fail --silent "http://127.0.0.1:$test_port/api/health" >/dev/null; then
    python3 tests/dashboard_api.py "http://127.0.0.1:$test_port"
    exit 0
  fi
  kill -0 "$backend_pid"
  sleep 1
done
echo 'Dashboard did not become ready within 30 seconds' >&2
exit 1
