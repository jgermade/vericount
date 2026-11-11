#!/bin/sh
# Runs once, when the database volume is created: the server's login
# role. The migrate service, run by the owner, makes it a member of
# vericounts_app, which may only read and insert.
set -eu
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
    -v app_password="$VERICOUNTS_DB_PASSWORD" <<'SQL'
CREATE ROLE vericounts LOGIN PASSWORD :'app_password';
SQL
