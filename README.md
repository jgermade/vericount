<p align="center"><img src="web/public/logo-wordmark.svg" alt="vericounts" width="320"></p>

# vericounts

Bookkeeping for a Spanish SL with Veri*factu compliance. See the latest roadmap in [`ROADMAP/`](ROADMAP/) and the agent conventions in [`AGENTS.md`](AGENTS.md).

## Status

Issued invoices can be created, listed and cancelled by a signed-in user. There are no passwords: invited users sign up and sign in with a code sent to their email, or with a passkey. Each one gets a Veri*factu billing record with a chained hash, stored in insert-only tables, and records are sent to the AEAT in order when sending is enabled. So far they have only been sent to the mock in `crates/aeat-mock`, never to the AEAT itself.

## Install

Three ways, all from the same [releases](https://github.com/jgermade/vericounts/releases): a static binary for Linux (x86_64 and aarch64) with the web client, a Docker image and a Helm chart. Each needs PostgreSQL and, in production, an SMTP server and HTTPS in front (passkeys need a domain name).

### One line, on a Linux server with systemd

```bash
curl -fsSL https://github.com/jgermade/vericounts/releases/latest/download/setup.sh | sudo bash
```

A wizard (in Spanish) offers to **install** or, if vericounts is already there, to **reinstall/update** or **uninstall**:

- **Install** asks for the public URL, the first user's email and SMTP; installs PostgreSQL locally (apt or dnf) or uses an existing one; downloads the release for the architecture and checks it against `SHA256SUMS`; installs it to `/opt/vericounts`, the settings to `/etc/vericounts/vericounts.env` and a hardened systemd service `vericounts` listening on `127.0.0.1`; runs the migrations and invites the first user.
- **Update** installs the latest (or `--version`) release next to the current one, migrates, restarts and checks the service; the two previous versions are kept to go back.
- **Uninstall** removes the program and the service, and keeps the configuration and the database unless `BORRAR` is typed: invoices must be kept for years.

Without questions: `sudo bash setup.sh install --yes` (answers from environment variables, see the top of [`setup.sh`](setup.sh)), `update --yes [--version X.Y.Z]`, `uninstall --yes [--purge]`.

### Docker

[`docker-compose.yml`](docker-compose.yml) runs PostgreSQL, the migrations and the server (`ghcr.io/jgermade/vericounts`, a distroless image of about 25 MB with the static binary):

```bash
cp .env.example .env   # set the two passwords, the public URL and SMTP
```

```bash
docker compose up -d
```

```bash
docker compose run --rm app invite you@example.com
```

The client is then on `127.0.0.1:8080`, for an HTTPS reverse proxy. The AEAT certificate goes in `./secrets` (see `.env.example`). `docker build -t vericounts .` builds the same image from source.

### Kubernetes (Helm)

The chart in [`charts/vericounts`](charts/vericounts) (also attached to each release) runs one replica, with the migrations in an init container. PostgreSQL is external, e.g. an operator such as PGO with pgBackRest:

```bash
helm install vericounts https://github.com/jgermade/vericounts/releases/download/vX.Y.Z/vericounts-X.Y.Z.tgz \
  --set publicUrl=https://books.example.com \
  --set database.url=postgres://vericounts:...@db/vericounts \
  --set database.migrationUrl=postgres://vericounts_owner:...@db/vericounts \
  --set smtp.url=smtps://user:password@smtp.example.com --set smtp.from=no-reply@example.com \
  --set ingress.enabled=true
```

All values are described in [`values.yaml`](charts/vericounts/values.yaml); `existingSecret` takes the connection URLs from a Secret instead.

### Database roles

The server connects with a login role that is a member of `vericounts_app`, which may only read and insert; the migrations run with the role that owns the schema. The first migration creates `vericounts_app`, so on the first run the owner needs `CREATEROLE` (it can be revoked afterwards). `migrate` with `VERICOUNTS_APP_ROLE=<role>` then makes the server's role a member. The installer, the compose file and the chart do all of this.

## Releases

Run **Actions → Release → Run workflow** on `main` and choose `patch`, `minor` or `major`. [`release.yml`](.github/workflows/release.yml) bumps the version (`scripts/version.sh`: Cargo and the chart), commits and tags `vX.Y.Z` on `main`, runs [`build.yml`](.github/workflows/build.yml) on the tag (tests, then the binaries, web client, chart and `setup.sh`), creates the GitHub release with those assets and `SHA256SUMS`, and pushes the image to GHCR as `X.Y.Z`, `X.Y` and `latest`. `build.yml` also runs on every pull request to `main` and every push to it.

## Development

Requires Rust (pinned in `rust-toolchain.toml`), Node 22 and PostgreSQL 16.

```bash
# Web client (output in web/dist)
cd web && npm ci && npm run build

# Database: migrations run with the owner role
DATABASE_URL=postgres://owner:secret@127.0.0.1/vericounts cargo run -- migrate

# First user: invite an email address, then sign up with it in the web client
DATABASE_URL=postgres://app:secret@127.0.0.1/vericounts cargo run -- invite you@example.com

# Server: use a login role that is a member of vericounts_app
DATABASE_URL=postgres://app:secret@127.0.0.1/vericounts cargo run

# Checks (the tests create temporary databases from DATABASE_URL)
DATABASE_URL=postgres://postgres:postgres@127.0.0.1/postgres cargo test --workspace
# The same, with the SMTP test against a local Mailpit (see below)
VERICOUNTS_TEST_SMTP_URL=smtp://127.0.0.1:1025 VERICOUNTS_TEST_MAILPIT_URL=http://127.0.0.1:8025 \
  DATABASE_URL=postgres://postgres:postgres@127.0.0.1/postgres cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The session cookie is `Secure`: browsers accept it over plain HTTP only on `localhost` and `127.0.0.1`, so anything else needs HTTPS. Passkeys are bound to the host name of `VERICOUNTS_PUBLIC_URL` and do not work on an IP address: open the client at `http://localhost:8080`, not `127.0.0.1`.

## Sign-in

- Nobody can sign up uninvited. The first user is invited with `vericounts-server invite <email>`; signed-in users invite others from Settings, which also emails them. All users have full access.
- Signing up and signing in are the same step: enter the email address and the six-digit code sent to it. The first sign-in of an invited address creates the account.
- Codes are valid for 10 minutes and once, only the latest one counts, and each allows 5 guesses. At most one code a minute and five an hour are sent to an address. The answer to a code request is the same whether the address has an account or not.
- After signing in with a code, the client offers to create a passkey on that device. Passkeys are then offered in the email field's autofill and by the "Entrar con passkey" button, with no email or code. They are listed and removed in Settings.

Emails go through SMTP (`VERICOUNTS_SMTP_URL`). Without it, outside production, they are printed to standard error, codes included. To see them as a real mail client would, run [Mailpit](https://mailpit.axllent.org/) and open its inbox at `http://127.0.0.1:8025`:

```bash
mailpit --smtp 127.0.0.1:1025 --listen 127.0.0.1:8025
```

```bash
VERICOUNTS_SMTP_URL=smtp://127.0.0.1:1025 VERICOUNTS_MAIL_FROM='vericounts <no-reply@example.com>' cargo run
```

To try the AEAT flow locally, run the mock with `AEAT_MOCK_SCHEMAS=aeat/schemas cargo run -p vericounts-aeat-mock` and point `VERICOUNTS_AEAT_ENDPOINT` to the URL it prints.

`npm run dev` in `web/` serves the client with hot reload and proxies `/api` to the server.

Configuration (environment variables; an empty value counts as unset):

| Variable | Default | Purpose |
|---|---|---|
| `DATABASE_URL` | required | PostgreSQL connection |
| `VERICOUNTS_AEAT_ENV` | `test` | `test` or `production`; selects the QR URL |
| `VERICOUNTS_AEAT_SUBMIT` | off | Set to `1` to send records to the AEAT |
| `VERICOUNTS_AEAT_ENDPOINT` | official | Service URL, e.g. the mock; refused in production |
| `VERICOUNTS_AEAT_IDENTITY_PEM` | none | PEM file with the client certificate and key |
| `VERICOUNTS_AEAT_REPRESENTATIVE_NAME` | none | Who submits on behalf of the company; set together with the NIF |
| `VERICOUNTS_AEAT_REPRESENTATIVE_NIF` | none | NIF of that representative |
| `VERICOUNTS_SOFTWARE_PRODUCER_NAME` | required to send | Producer of the software, declared in every record |
| `VERICOUNTS_SOFTWARE_PRODUCER_NIF` | required to send | NIF of the producer |
| `VERICOUNTS_INSTALLATION_NUMBER` | `1` | Identifies this installation in every record |
| `VERICOUNTS_PUBLIC_URL` | `http://localhost:8080` | Where users open the client; passkeys are bound to its host, invitations link to it. Must be `https` outside `localhost` |
| `VERICOUNTS_SMTP_URL` | none; required in production | SMTP server, e.g. `smtps://user:password@smtp.example.com` or `smtp://127.0.0.1:1025` |
| `VERICOUNTS_MAIL_FROM` | required with SMTP | Sender, e.g. `vericounts <no-reply@example.com>` |
| `VERICOUNTS_ADDR` | `127.0.0.1:8080` | Listen address |
| `VERICOUNTS_APP_ROLE` | none | Only for `migrate`: the server's login role, made a member of `vericounts_app` |
| `VERICOUNTS_WEB_DIR` | `web/dist` | Directory with the built web client |

After signing in for the first time, enter the company data, a series and a customer in the interface; nothing can be invoiced before that.

## AEAT certificate

The AEAT service requires a client certificate. Two kinds work, and the Settings screen tells which one is in use and when it expires:

- **A certificate of the company** (a "representative of a legal person" certificate). Its NIF is the company's; nothing else is needed.
- **A personal certificate under a power of attorney** ("apoderamiento"). The company must have empowered its holder at the AEAT to submit billing records. Set `VERICOUNTS_AEAT_REPRESENTATIVE_NAME` and `VERICOUNTS_AEAT_REPRESENTATIVE_NIF` to declare the holder as representative in every request.

The server reads one PEM file with the certificate and its private key. Certificates usually come as `.p12` or `.pfx` files; convert them with:

```bash
openssl pkcs12 -in certificate.p12 -out identity.pem -nodes
```

Keep that file outside the repository, readable only by the server, and never commit it.

## Layout

- `crates/core`: domain logic without I/O (record hash, invoice totals, QR URL).
- `crates/server`: Axum HTTP server, API, database access, migrations and security headers.
- `web`: web client built with jq79, TypeScript and Vite; components are precompiled so it runs under a strict CSP.
- `crates/aeat-mock`: mock of the AEAT record submission service, for tests and local development.
- `aeat/schemas`: official AEAT WSDL and XSD files.
- `setup.sh`, `Dockerfile`, `docker/`, `docker-compose.yml`, `charts/vericounts`: installation and deployment.
- `scripts/version.sh`: prints or bumps the version.
