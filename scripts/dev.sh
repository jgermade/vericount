#!/usr/bin/env bash
# Runs vericount in development mode, for `make up`: the AEAT mock, the
# server and the Vite dev server with hot reload, until Ctrl+C. The
# database and Mailpit must be running (docker-compose.dev.yml) and
# migrated; the Makefile does both, and passes the settings as
# environment variables.
set -euo pipefail

cd "$(dirname "$0")/.."

: "${DATABASE_URL:?}" "${VERICOUNT_PUBLIC_URL:?}" "${DEV_WEB_PORT:?}"

# Builds both binaries before starting them, so their output does not
# interleave with the compiler's.
cargo build -p vericount-server -p vericount-aeat-mock

pids=()
stop() {
    trap - INT TERM EXIT
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
}
trap stop INT TERM EXIT

AEAT_MOCK_SCHEMAS=aeat/schemas target/debug/vericount-aeat-mock &
pids+=($!)
target/debug/vericount-server &
pids+=($!)
(cd web && exec node_modules/.bin/vite --host localhost --port "$DEV_WEB_PORT" --strictPort) &
pids+=($!)

cat <<EOF

  Client:  $VERICOUNT_PUBLIC_URL
  Mail:    http://127.0.0.1:${DEV_MAILPIT_PORT:-8025}
  First user: make invite EMAIL=you@example.com

EOF

# Stops everything when any of them exits.
wait -n
