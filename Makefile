# Development tasks. Requires Rust (rust-toolchain.toml), Node 22 and
# Docker with Compose, which runs PostgreSQL and Mailpit
# (docker-compose.dev.yml).
#
#   make up       run the application locally in development mode
#   make test     run the project's tests
#   make build    build the application: web client and release server
#
# Also: make invite EMAIL=..., make lint, make check, make down,
# make reset (deletes the development database), make clean.

DEV_DB_PORT      ?= 5433
DEV_SMTP_PORT    ?= 1025
DEV_MAILPIT_PORT ?= 8025
DEV_WEB_PORT     ?= 5173
DEV_SERVER_PORT  ?= 8080
DEV_AEAT_PORT    ?= 8090

export DEV_DB_PORT DEV_SMTP_PORT DEV_MAILPIT_PORT DEV_WEB_PORT

COMPOSE := docker compose -f docker-compose.dev.yml

OWNER_URL := postgres://vericount_owner:vericount_owner@127.0.0.1:$(DEV_DB_PORT)
APP_URL   := postgres://vericount:vericount@127.0.0.1:$(DEV_DB_PORT)/vericount

# The server in development: the client is opened through Vite, which
# proxies /api to it, so passkeys are bound to Vite's address. Mail goes
# to Mailpit. Practice records go to the mock inside the server; live
# ones, once live mode is on, to the AEAT mock that `make up` runs, never
# to the AEAT. The secret key, which encrypts the uploaded certificate,
# is fixed: development only.
DEV_ENV := \
	DATABASE_URL=$(APP_URL) \
	VERICOUNT_ADDR=127.0.0.1:$(DEV_SERVER_PORT) \
	VERICOUNT_PUBLIC_URL=http://localhost:$(DEV_WEB_PORT) \
	VERICOUNT_SMTP_URL=smtp://127.0.0.1:$(DEV_SMTP_PORT) \
	VERICOUNT_MAIL_FROM='vericount <no-reply@localhost>' \
	VERICOUNT_SECRET_KEY=0000000000000000000000000000000000000000000000000000000000000000 \
	VERICOUNT_AEAT_ENV=test \
	VERICOUNT_AEAT_ENDPOINT=http://127.0.0.1:$(DEV_AEAT_PORT)/wlpl/TIKE-CONT/ws/SistemaFacturacion/VerifactuSOAP \
	VERICOUNT_SOFTWARE_PRODUCER_NAME='vericount desarrollo' \
	VERICOUNT_SOFTWARE_PRODUCER_NIF=B00000000 \
	AEAT_MOCK_ADDR=127.0.0.1:$(DEV_AEAT_PORT)

# The tests create temporary databases from DATABASE_URL, and send the
# sign-in code email to Mailpit.
TEST_ENV := \
	DATABASE_URL=$(OWNER_URL)/postgres \
	VERICOUNT_TEST_SMTP_URL=smtp://127.0.0.1:$(DEV_SMTP_PORT) \
	VERICOUNT_TEST_MAILPIT_URL=http://127.0.0.1:$(DEV_MAILPIT_PORT)

.PHONY: up test build invite lint check services migrate down reset clean

up: web/node_modules services migrate
	$(DEV_ENV) DEV_MAILPIT_PORT=$(DEV_MAILPIT_PORT) scripts/dev.sh

test: web/node_modules services
	cd web && npm run build
	$(TEST_ENV) cargo test --workspace --locked

build: web/node_modules
	cd web && npm run build
	cargo build --release --locked -p vericount-server
	@echo "Built target/release/vericount-server and web/dist"

# Invites the first user, who then signs up in the client with the code
# sent to Mailpit.
invite: services migrate
	@test -n "$(EMAIL)" || { echo "usage: make invite EMAIL=you@example.com"; exit 1; }
	$(DEV_ENV) cargo run -q -p vericount-server -- invite $(EMAIL)

# The other checks CI runs; shellcheck and helm only if installed.
lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	@if command -v shellcheck >/dev/null; then shellcheck setup.sh scripts/*.sh docker/initdb/*.sh; \
		else echo "shellcheck not installed, skipped"; fi
	@if command -v helm >/dev/null; then helm lint charts/vericount; \
		else echo "helm not installed, skipped"; fi

check: lint test

web/node_modules: web/package-lock.json
	cd web && npm ci
	@touch $@

services:
	$(COMPOSE) up -d --wait

migrate:
	DATABASE_URL=$(OWNER_URL)/vericount VERICOUNT_APP_ROLE=vericount \
		cargo run -q -p vericount-server -- migrate

down:
	$(COMPOSE) down

reset:
	$(COMPOSE) down --volumes

clean:
	cargo clean
	rm -rf web/dist web/node_modules
