# vericounts

Bookkeeping for a Spanish SL with Veri*factu compliance. See the latest roadmap in [`ROADMAP/`](ROADMAP/) and the agent conventions in [`AGENTS.md`](AGENTS.md).

## Status

Issued invoices can be created, listed and cancelled by a signed-in user. Each one gets a Veri*factu billing record with a chained hash, stored in insert-only tables, and records are sent to the AEAT in order when sending is enabled. So far they have only been sent to the mock in `crates/aeat-mock`, never to the AEAT itself.

## Development

Requires Rust (pinned in `rust-toolchain.toml`), Node 22 and PostgreSQL 16.

```bash
# Web client (output in web/dist)
cd web && npm ci && npm run build

# Database: migrations run with the owner role
DATABASE_URL=postgres://owner:secret@127.0.0.1/vericounts cargo run -- migrate

# First user; the password is read from standard input
DATABASE_URL=postgres://app:secret@127.0.0.1/vericounts cargo run -- create-user admin

# Server: use a login role that is a member of vericounts_app
DATABASE_URL=postgres://app:secret@127.0.0.1/vericounts cargo run

# Checks (the tests create temporary databases from DATABASE_URL)
DATABASE_URL=postgres://postgres:postgres@127.0.0.1/postgres cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The session cookie is `Secure`: browsers accept it over plain HTTP only on `localhost` and `127.0.0.1`, so anything else needs HTTPS.

To try the AEAT flow locally, run the mock with `AEAT_MOCK_SCHEMAS=aeat/schemas cargo run -p vericounts-aeat-mock` and point `VERICOUNTS_AEAT_ENDPOINT` to the URL it prints.

`npm run dev` in `web/` serves the client with hot reload and proxies `/api` to the server.

Configuration (environment variables):

| Variable | Default | Purpose |
|---|---|---|
| `DATABASE_URL` | required | PostgreSQL connection |
| `VERICOUNTS_AEAT_ENV` | `test` | `test` or `production`; selects the QR URL |
| `VERICOUNTS_AEAT_SUBMIT` | off | Set to `1` to send records to the AEAT |
| `VERICOUNTS_AEAT_ENDPOINT` | official | Service URL, e.g. the mock; refused in production |
| `VERICOUNTS_AEAT_IDENTITY_PEM` | none | PEM file with the client certificate and key |
| `VERICOUNTS_SOFTWARE_PRODUCER_NAME` | required to send | Producer of the software, declared in every record |
| `VERICOUNTS_SOFTWARE_PRODUCER_NIF` | required to send | NIF of the producer |
| `VERICOUNTS_INSTALLATION_NUMBER` | `1` | Identifies this installation in every record |
| `VERICOUNTS_ADDR` | `127.0.0.1:8080` | Listen address |
| `VERICOUNTS_WEB_DIR` | `web/dist` | Directory with the built web client |

After signing in for the first time, enter the company data, a series and a customer in the interface; nothing can be invoiced before that.

## Layout

- `crates/core`: domain logic without I/O (record hash, invoice totals, QR URL).
- `crates/server`: Axum HTTP server, API, database access, migrations and security headers.
- `web`: web client built with jq79, TypeScript and Vite; components are precompiled so it runs under a strict CSP.
- `crates/aeat-mock`: mock of the AEAT record submission service, for tests and local development.
- `aeat/schemas`: official AEAT WSDL and XSD files.
