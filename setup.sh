#!/usr/bin/env bash
# vericounts installer for a Linux server with systemd (x86_64 or aarch64).
#
#   curl -fsSL https://github.com/jgermade/vericounts/releases/latest/download/setup.sh | sudo bash
#
# Without arguments it runs a wizard: install, or, when vericounts is
# already installed, reinstall/update or uninstall. It can also run
# without questions:
#
#   setup.sh install   [--version X.Y.Z] [--yes]
#   setup.sh update    [--version X.Y.Z] [--yes]
#   setup.sh uninstall [--yes] [--purge]
#
# With --yes, install takes its answers from the environment:
# VERICOUNTS_PUBLIC_URL, VERICOUNTS_PORT, VERICOUNTS_ADMIN_EMAIL,
# VERICOUNTS_SMTP_URL, VERICOUNTS_MAIL_FROM and, to use an existing
# PostgreSQL instead of a local one, VERICOUNTS_DATABASE_URL and
# VERICOUNTS_MIGRATION_DATABASE_URL.
#
# VERICOUNTS_ARCHIVE=path/to/vericounts-X.Y.Z-ARCH-linux.tar.gz installs a
# local archive instead of downloading one.
#
# Messages are in Spanish, like the interface; code and comments are not.

set -euo pipefail
# Files and directories are created without write access for others,
# whatever the caller's umask.
umask 022

REPO=${VERICOUNTS_REPO:-jgermade/vericounts}
PREFIX=/opt/vericounts
CONF_DIR=/etc/vericounts
ENV_FILE=$CONF_DIR/vericounts.env
# The owner's connection, for migrations. Readable by root only.
MIGRATE_FILE=$CONF_DIR/migrate.env
UNIT_FILE=/etc/systemd/system/vericounts.service
SERVICE_USER=vericounts
KEEP_RELEASES=3

ACTION=""
VERSION=""
ASSUME_YES=0
PURGE=0

# --- Output and questions ---------------------------------------------------

if [ -t 1 ]; then
    BOLD=$'\e[1m' RED=$'\e[31m' GREEN=$'\e[32m' YELLOW=$'\e[33m' RESET=$'\e[0m'
else
    BOLD="" RED="" GREEN="" YELLOW="" RESET=""
fi
say() { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
ok() { printf '%s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%s!%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() { printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

# Questions are read from the terminal, so the script works when it is
# piped into bash.
TTY=""
if [ -r /dev/tty ] && { true </dev/tty; } 2>/dev/null; then
    TTY=/dev/tty
fi

# ask VAR "question" [default]
ask() {
    local var=$1 question=$2 default=${3:-} answer
    if [ "$ASSUME_YES" = 1 ] || [ -z "$TTY" ]; then
        printf -v "$var" '%s' "$default"
        return
    fi
    if [ -n "$default" ]; then
        read -r -p "$question [$default]: " answer <"$TTY"
    else
        read -r -p "$question: " answer <"$TTY"
    fi
    printf -v "$var" '%s' "${answer:-$default}"
}

# confirm "question" → true for yes; the default is no.
confirm() {
    local answer
    [ "$ASSUME_YES" = 1 ] && return 0
    [ -n "$TTY" ] || return 1
    read -r -p "$1 [s/N]: " answer <"$TTY"
    [[ $answer =~ ^[sSyY] ]]
}

# --- Checks -----------------------------------------------------------------

require_root() {
    [ "$(id -u)" = 0 ] || die "ejecuta el instalador como root (por ejemplo con sudo)."
}

require_systemd() {
    if ! command -v systemctl >/dev/null || [ ! -d /run/systemd/system ]; then
        die "hace falta systemd. Sin él, usa Docker o Helm (mira el README)."
    fi
}

detect_arch() {
    case "$(uname -m)" in
        x86_64 | amd64) echo x86_64 ;;
        aarch64 | arm64) echo aarch64 ;;
        *) die "arquitectura no soportada: $(uname -m)" ;;
    esac
}

installed() { [ -f "$ENV_FILE" ] && [ -e "$PREFIX/current" ]; }

installed_version() { cat "$PREFIX/current/VERSION" 2>/dev/null || echo "?"; }

latest_version() {
    curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p' | head -n1
}

random_password() { od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; }

# Values go into files that systemd and the shell both read, in single
# quotes; a single quote or a line break would end the value.
check_value() {
    case "$2" in
        *"'"* | *$'\n'*) die "$1 no puede contener comillas simples ni saltos de línea." ;;
    esac
}

# --- Download and unpack ----------------------------------------------------

# fetch_release VERSION → prints the directory where it was unpacked.
fetch_release() {
    local version=$1 arch tmp archive
    arch=$(detect_arch)
    tmp=$(mktemp -d)
    if [ -n "${VERICOUNTS_ARCHIVE:-}" ]; then
        archive=$VERICOUNTS_ARCHIVE
        [ -f "$archive" ] || die "no existe $archive"
    else
        local name="vericounts-$version-$arch-linux.tar.gz"
        local base="https://github.com/$REPO/releases/download/v$version"
        step "Descargando vericounts $version ($arch)" >&2
        curl -fsSL -o "$tmp/$name" "$base/$name" || die "no se ha podido descargar $base/$name"
        curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "no se ha podido descargar SHA256SUMS"
        (cd "$tmp" && grep " $name\$" SHA256SUMS | sha256sum -c --quiet -) \
            || die "la suma SHA-256 de $name no coincide"
        archive=$tmp/$name
    fi
    tar -C "$tmp" -xzf "$archive"
    [ -x "$tmp/vericounts/bin/vericounts-server" ] || die "el archivo no contiene vericounts"
    echo "$tmp/vericounts"
}

# install_release DIR → installs it under $PREFIX/releases and prints its version.
install_release() {
    local dir=$1 version
    version=$(cat "$dir/VERSION")
    rm -rf "$PREFIX/releases/$version"
    mkdir -p "$PREFIX/releases"
    mv "$dir" "$PREFIX/releases/$version"
    chown -R root:root "$PREFIX/releases/$version"
    chmod -R go-w "$PREFIX/releases/$version"
    echo "$version"
}

activate_release() {
    ln -sfn "$PREFIX/releases/$1" "$PREFIX/current.new"
    mv -T "$PREFIX/current.new" "$PREFIX/current"
}

prune_releases() {
    local keep current
    current=$(readlink -f "$PREFIX/current")
    keep=$(find "$PREFIX/releases" -mindepth 1 -maxdepth 1 -type d -printf '%T@ %p\n' \
        | sort -rn | head -n "$KEEP_RELEASES" | cut -d' ' -f2-)
    find "$PREFIX/releases" -mindepth 1 -maxdepth 1 -type d | while read -r dir; do
        [ "$dir" = "$current" ] && continue
        grep -qxF "$dir" <<<"$keep" || rm -rf "$dir"
    done
}

# --- PostgreSQL -------------------------------------------------------------

psql_admin() { runuser -u postgres -- psql -v ON_ERROR_STOP=1 -qAt "$@"; }

install_postgres() {
    if command -v psql >/dev/null && runuser -u postgres -- psql -qAt -c 'SELECT 1' >/dev/null 2>&1; then
        return
    fi
    step "Instalando PostgreSQL"
    if command -v apt-get >/dev/null; then
        DEBIAN_FRONTEND=noninteractive apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq postgresql >/dev/null
    elif command -v dnf >/dev/null; then
        dnf install -y -q postgresql-server
        [ -f /var/lib/pgsql/data/PG_VERSION ] || postgresql-setup --initdb
    else
        die "no sé instalar PostgreSQL en este sistema; instálalo y vuelve a ejecutar el instalador."
    fi
    systemctl enable --now postgresql >/dev/null 2>&1 || true
    for _ in $(seq 30); do
        runuser -u postgres -- psql -qAt -c 'SELECT 1' >/dev/null 2>&1 && return
        sleep 1
    done
    die "PostgreSQL no arranca"
}

# Creates the database with two roles: the owner, which runs the
# migrations, and the server's, which the first migration makes a member
# of vericounts_app, which may only read and insert. That migration also
# creates vericounts_app, so the owner may create roles until it has run
# (see finish_local_database). Sets DATABASE_URL and MIGRATION_DATABASE_URL.
create_local_database() {
    local owner_password app_password
    owner_password=$(random_password)
    app_password=$(random_password)
    step "Creando la base de datos vericounts"
    if [ -n "$(psql_admin -c "SELECT 1 FROM pg_database WHERE datname = 'vericounts'")" ]; then
        die "ya existe una base de datos vericounts en este PostgreSQL"
    fi
    psql_admin <<SQL
CREATE ROLE vericounts_owner LOGIN CREATEROLE PASSWORD '$owner_password';
CREATE ROLE vericounts LOGIN PASSWORD '$app_password';
CREATE DATABASE vericounts OWNER vericounts_owner;
SQL
    DATABASE_URL="postgres://vericounts:$app_password@127.0.0.1:5432/vericounts"
    MIGRATION_DATABASE_URL="postgres://vericounts_owner:$owner_password@127.0.0.1:5432/vericounts"
}

# After the first migration the owner no longer needs to create roles.
finish_local_database() {
    psql_admin -c "ALTER ROLE vericounts_owner NOCREATEROLE"
}

drop_local_database() {
    psql_admin -c "DROP DATABASE IF EXISTS vericounts"
    psql_admin -c "DROP ROLE IF EXISTS vericounts"
    psql_admin -c "DROP ROLE IF EXISTS vericounts_app"
    psql_admin -c "DROP ROLE IF EXISTS vericounts_owner"
}

# --- Service ----------------------------------------------------------------

# Runs the server binary as the service user with the given env file.
run_with() {
    local file=$1
    shift
    (
        set -a
        # shellcheck disable=SC1090
        . "$file"
        set +a
        runuser -u "$SERVICE_USER" -- "$PREFIX/current/bin/vericounts-server" "$@"
    )
}

migrate() {
    step "Aplicando las migraciones"
    run_with "$MIGRATE_FILE" migrate
}

write_unit() {
    cat >"$UNIT_FILE" <<EOF
# Installed by setup.sh. Settings are in $ENV_FILE.
[Unit]
Description=vericounts
Documentation=https://github.com/$REPO
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=$SERVICE_USER
Group=$SERVICE_USER
EnvironmentFile=$ENV_FILE
ExecStart=$PREFIX/current/bin/vericounts-server serve
Restart=on-failure
RestartSec=5
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
CapabilityBoundingSet=
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
}

wait_healthy() {
    for _ in $(seq 20); do
        run_with "$ENV_FILE" healthcheck >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

restart_service() {
    systemctl restart vericounts
    if wait_healthy; then
        ok "vericounts $(installed_version) está en marcha"
    else
        warn "vericounts no responde. Mira el registro con: journalctl -u vericounts -e"
        return 1
    fi
}

# --- Actions ----------------------------------------------------------------

do_install() {
    require_systemd
    local version=$VERSION public_url port admin_email smtp_url mail_from db_mode release
    [ -n "$version" ] || [ -n "${VERICOUNTS_ARCHIVE:-}" ] || version=$(latest_version)
    [ -n "$version" ] || [ -n "${VERICOUNTS_ARCHIVE:-}" ] || die "no se encuentra ninguna versión publicada de $REPO"

    # An earlier uninstall that kept the data: install the program again
    # with the same settings and database.
    if [ -f "$ENV_FILE" ] && [ -f "$MIGRATE_FILE" ]; then
        say "Hay una configuración anterior en $CONF_DIR: se reutiliza con sus datos."
        id "$SERVICE_USER" >/dev/null 2>&1 \
            || useradd --system --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
        release=$(install_release "$(fetch_release "$version")")
        activate_release "$release"
        migrate
        write_unit
        systemctl enable vericounts >/dev/null 2>&1
        restart_service
        return
    fi

    say ""
    say "${BOLD}Instalación de vericounts${RESET}"
    say "Las respuestas se guardan en $ENV_FILE y se pueden cambiar después."
    say ""
    ask public_url "Dirección web con la que se abrirá (https://…; las passkeys necesitan un dominio)" \
        "${VERICOUNTS_PUBLIC_URL:-http://localhost:8080}"
    ask port "Puerto local del servidor (pon delante un proxy HTTPS)" "${VERICOUNTS_PORT:-8080}"
    [[ $port =~ ^[0-9]+$ ]] || die "el puerto debe ser un número"
    ask admin_email "Correo del primer usuario (recibirá un código para crear su cuenta)" \
        "${VERICOUNTS_ADMIN_EMAIL:-}"
    say ""
    say "Correo saliente (SMTP) para los códigos de acceso. Sin él, los códigos"
    say "se escriben en el registro del servicio, lo que solo vale para probar."
    ask smtp_url "URL SMTP, p. ej. smtps://usuario:clave@smtp.ejemplo.es (vacío: sin SMTP)" \
        "${VERICOUNTS_SMTP_URL:-}"
    mail_from=""
    if [ -n "$smtp_url" ]; then
        ask mail_from "Remitente, p. ej. vericounts <no-reply@ejemplo.es>" "${VERICOUNTS_MAIL_FROM:-}"
        [ -n "$mail_from" ] || die "con SMTP hace falta un remitente"
    fi
    for value in "$public_url" "$admin_email" "$smtp_url" "$mail_from"; do
        check_value "Las respuestas" "$value"
    done

    say ""
    DATABASE_URL=${VERICOUNTS_DATABASE_URL:-}
    MIGRATION_DATABASE_URL=${VERICOUNTS_MIGRATION_DATABASE_URL:-}
    if [ -n "$DATABASE_URL" ] && [ -n "$MIGRATION_DATABASE_URL" ]; then
        db_mode=existing
    elif [ "$ASSUME_YES" != 1 ] && confirm "¿Usar un PostgreSQL que ya tienes en lugar de uno local?"; then
        db_mode=existing
        say "Hacen falta dos conexiones: la del dueño del esquema, que aplica las"
        say "migraciones (la primera vez necesita CREATEROLE), y la del servidor, un rol"
        say "con LOGIN al que la primera migración da los permisos de vericounts_app."
        ask MIGRATION_DATABASE_URL "URL del dueño" ""
        ask DATABASE_URL "URL del servidor" ""
    else
        db_mode=local
    fi
    check_value "Las URL de la base de datos" "$DATABASE_URL$MIGRATION_DATABASE_URL"
    if [ "$db_mode" = existing ] && { [ -z "$DATABASE_URL" ] || [ -z "$MIGRATION_DATABASE_URL" ]; }; then
        die "faltan las URL de la base de datos"
    fi

    if ! id "$SERVICE_USER" >/dev/null 2>&1; then
        useradd --system --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
    fi
    release=$(install_release "$(fetch_release "$version")")
    activate_release "$release"
    ok "vericounts $release instalado en $PREFIX"

    if [ "$db_mode" = local ]; then
        install_postgres
        create_local_database
    fi

    # The server's role, from its connection URL.
    local app_role
    app_role=$(sed -E 's#^[a-z]+://([^:@/]+).*#\1#' <<<"$DATABASE_URL")
    [[ $app_role =~ ^[a-z_][a-z0-9_]*$ ]] || die "no se reconoce el rol en la URL del servidor"

    mkdir -p "$CONF_DIR"
    umask 077
    cat >"$MIGRATE_FILE" <<EOF
# Owner of the database schema, used by setup.sh to run migrations, and
# the server's role, which they make a member of vericounts_app.
DATABASE_URL='$MIGRATION_DATABASE_URL'
VERICOUNTS_APP_ROLE='$app_role'
VERICOUNTS_SETUP_DATABASE='$db_mode'
EOF
    cat >"$ENV_FILE" <<EOF
# vericounts settings, read by the systemd service. After a change:
#   systemctl restart vericounts
# All settings are described in the README.
DATABASE_URL='$DATABASE_URL'
VERICOUNTS_ADDR='127.0.0.1:$port'
VERICOUNTS_PUBLIC_URL='$public_url'
VERICOUNTS_WEB_DIR='$PREFIX/current/web'
VERICOUNTS_SMTP_URL='$smtp_url'
VERICOUNTS_MAIL_FROM='$mail_from'
VERICOUNTS_AEAT_ENV='test'
EOF
    umask 022
    chown root:root "$MIGRATE_FILE"
    chmod 0600 "$MIGRATE_FILE"
    chown "root:$SERVICE_USER" "$CONF_DIR" "$ENV_FILE"
    chmod 0750 "$CONF_DIR"
    chmod 0640 "$ENV_FILE"

    migrate
    [ "$db_mode" = local ] && finish_local_database
    write_unit
    systemctl enable vericounts >/dev/null 2>&1
    restart_service

    if [ -n "$admin_email" ]; then
        run_with "$ENV_FILE" invite "$admin_email" >/dev/null && ok "Invitado $admin_email"
    fi

    say ""
    say "${BOLD}Siguientes pasos${RESET}"
    say "- Publica http://127.0.0.1:$port con HTTPS en $public_url (Caddy, nginx…)."
    [ -n "$admin_email" ] && say "- Entra en $public_url con $admin_email: recibirás un código para crear tu cuenta."
    [ -z "$smtp_url" ] && say "- Sin SMTP, los códigos están en: journalctl -u vericounts -e"
    say "- La configuración de la AEAT y el certificado se ponen en $ENV_FILE (mira el README)."
    say "- Para actualizar o desinstalar, vuelve a ejecutar este instalador."
}

do_update() {
    require_systemd
    installed || die "vericounts no está instalado"
    local current version release
    current=$(installed_version)
    version=$VERSION
    if [ -z "$version" ] && [ -z "${VERICOUNTS_ARCHIVE:-}" ]; then
        version=$(latest_version)
        [ -n "$version" ] || die "no se encuentra ninguna versión publicada de $REPO"
    fi
    if [ "$version" = "$current" ]; then
        confirm "La versión $current ya está instalada. ¿Reinstalarla?" || return 0
    else
        confirm "¿Actualizar vericounts de $current a ${version:-la del archivo}?" || return 0
    fi
    release=$(install_release "$(fetch_release "$version")")
    activate_release "$release"
    migrate
    write_unit
    if restart_service; then
        prune_releases
    else
        warn "La versión anterior sigue en $PREFIX/releases/$current; para volver a ella:"
        warn "  ln -sfn $PREFIX/releases/$current $PREFIX/current && systemctl restart vericounts"
        warn "Las migraciones nuevas ya están aplicadas y no se deshacen."
        exit 1
    fi
}

do_uninstall() {
    installed || [ -f "$UNIT_FILE" ] || die "vericounts no está instalado"
    confirm "¿Desinstalar vericounts?" || return 0
    systemctl disable --now vericounts >/dev/null 2>&1 || true
    rm -f "$UNIT_FILE"
    systemctl daemon-reload || true
    rm -rf "$PREFIX"
    ok "Programa desinstalado"

    local purge=$PURGE answer=""
    if [ "$purge" != 1 ] && [ -n "$TTY" ] && [ "$ASSUME_YES" != 1 ]; then
        say ""
        warn "La base de datos guarda las facturas y los registros Veri*factu, que la ley"
        warn "obliga a conservar durante años (al menos 4 por la normativa tributaria y"
        warn "6 por la mercantil). Haz una copia antes de borrarla."
        read -r -p "Para borrar también la configuración y la base de datos, escribe BORRAR: " answer <"$TTY"
        [ "$answer" = BORRAR ] && purge=1
    fi
    if [ "$purge" = 1 ]; then
        if grep -q "^VERICOUNTS_SETUP_DATABASE='local'" "$MIGRATE_FILE" 2>/dev/null; then
            drop_local_database
            ok "Base de datos borrada"
        else
            warn "La base de datos no la creó este instalador; no se ha tocado."
        fi
        rm -rf "$CONF_DIR"
        userdel "$SERVICE_USER" >/dev/null 2>&1 || true
        ok "Configuración borrada"
    else
        say "Se conservan la configuración ($CONF_DIR) y la base de datos."
    fi
}

# --- Wizard -----------------------------------------------------------------

menu() {
    local choice
    [ -n "$TTY" ] || die "sin terminal, indica la acción: install, update o uninstall"
    say "${BOLD}vericounts${RESET}"
    if installed; then
        say "Instalado: versión $(installed_version) en $PREFIX"
        say "  1) Reinstalar o actualizar"
        say "  2) Desinstalar"
        say "  3) Salir"
        ask choice "Elige" 1
        case "$choice" in
            1) do_update ;;
            2) do_uninstall ;;
            *) return 0 ;;
        esac
    else
        say "No está instalado en este equipo."
        say "  1) Instalar"
        say "  2) Salir"
        ask choice "Elige" 1
        case "$choice" in
            1) do_install ;;
            *) return 0 ;;
        esac
    fi
}

usage() {
    sed -n '2,25p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'
}

main() {
    while [ $# -gt 0 ]; do
        case "$1" in
            install | update | uninstall) ACTION=$1 ;;
            --version) VERSION=${2:?--version necesita un valor}; VERSION=${VERSION#v}; shift ;;
            --yes | -y) ASSUME_YES=1 ;;
            --purge) PURGE=1 ;;
            -h | --help) usage; exit 0 ;;
            *) die "opción desconocida: $1" ;;
        esac
        shift
    done
    require_root
    command -v curl >/dev/null || die "hace falta curl"
    case "$ACTION" in
        install)
            installed && die "vericounts ya está instalado; usa update o uninstall"
            do_install
            ;;
        update) do_update ;;
        uninstall) do_uninstall ;;
        "") menu ;;
    esac
}

main "$@"
