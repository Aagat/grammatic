#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
# A unique project isolates concurrent runs, including their disposable databases.
project="grammatic-test-$(date +%s)-$$"
compose=(docker compose -p "$project" -f compose.test.yaml)
trap '"${compose[@]}" down --volumes --remove-orphans' EXIT
"${compose[@]}" up --build --abort-on-container-exit --exit-code-from tests tests
