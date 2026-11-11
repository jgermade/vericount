# vericounts

Bookkeeping for a Spanish SL with Veri*factu compliance. See the latest roadmap in [`ROADMAP/`](ROADMAP/) and the agent conventions in [`AGENTS.md`](AGENTS.md).

## Status

Issued invoices can be created, listed and cancelled. Each one gets a Veri*factu billing record with a chained hash, stored in insert-only tables. **Records are not sent to the AEAT yet, and the API has no authentication: do not expose the server.**

## Development

Requires Rust (pinned in `rust-toolchain.toml`), Node 22 and PostgreSQL 16.

```bash
# Web client (output in web/dist)
cd web && npm ci && npm run build

# Database: migrations run with the owner role
DATABASE_URL=postgres://owner:secret@127.0.0.1/vericounts cargo run -- migrate

# Server: use a login role that is a member of vericounts_app
DATABASE_URL=postgres://app:secret@127.0.0.1/vericounts \
VERICOUNTS_ISSUER_NIF=B12345674 VERICOUNTS_ISSUER_NAME="Example SL" \
cargo run

# Checks (the tests create temporary databases from DATABASE_URL)
DATABASE_URL=postgres://postgres:postgres@127.0.0.1/postgres cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`npm run dev` in `web/` serves the client with hot reload and proxies `/api` to the server.

Configuration (environment variables):

| Variable | Default | Purpose |
|---|---|---|
| `DATABASE_URL` | required | PostgreSQL connection |
| `VERICOUNTS_ISSUER_NIF` | required | NIF of the company that issues the invoices |
| `VERICOUNTS_ISSUER_NAME` | required | Name of that company |
| `VERICOUNTS_AEAT_ENV` | `test` | `test` or `production`; selects the QR URL |
| `VERICOUNTS_ADDR` | `127.0.0.1:8080` | Listen address |
| `VERICOUNTS_WEB_DIR` | `web/dist` | Directory with the built web client |

## Layout

- `crates/core`: domain logic without I/O (record hash, invoice totals, QR URL).
- `crates/server`: Axum HTTP server, API, database access, migrations and security headers.
- `web`: web client built with jq79 and Vite; components are precompiled so it runs under a strict CSP.
