#!/usr/bin/env bash
# The dev databases without Docker, for Linux machines that have none (cloud
# sessions): Postgres on :55432, MySQL on :53306 and an SSH bastion on :52222,
# installed with apt and started directly, the same as docker-compose.yml offers
# them (passwordless, localhost only, seeded from dev/seed). Throwaway: only for a
# disposable machine. Safe to run again; it only starts what isn't running.
# Used by `make test-live-native`.
set -euo pipefail

PG_PORT=55432
MYSQL_PORT=53306
SSH_PORT=52222
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ "$(uname -s)" != Linux ] || ! command -v apt-get >/dev/null; then
    echo "dev-db-native: Debian or Ubuntu only; elsewhere use Docker (make db-up)" >&2
    exit 1
fi
SUDO=""
[ "$(id -u)" -eq 0 ] || SUDO=sudo

listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

wait_for() {
    for _ in $(seq 60); do
        listening "$1" && return 0
        sleep 1
    done
    echo "dev-db-native: nothing listens on :$1" >&2
    exit 1
}

missing=()
command -v pg_createcluster >/dev/null || missing+=(postgresql)
command -v mysqld >/dev/null || missing+=(mysql-server)
command -v sshd >/dev/null || [ -x /usr/sbin/sshd ] || missing+=(openssh-server)
if [ ${#missing[@]} -gt 0 ]; then
    echo "dev-db-native: installing ${missing[*]}"
    $SUDO apt-get update -qq
    $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${missing[@]}" >/dev/null
fi

# Postgres: a cluster of its own on PG_PORT that trusts local logins.
if ! listening $PG_PORT; then
    version="$(ls /usr/lib/postgresql | sort -V | tail -1)"
    if ! pg_lsclusters -h | awk '{print $2}' | grep -qx tangul; then
        # Statement statistics for the Queries performance board.
        $SUDO pg_createcluster "$version" tangul -p $PG_PORT \
            -o shared_preload_libraries=pg_stat_statements -- --auth=trust >/dev/null
    fi
    $SUDO pg_ctlcluster "$version" tangul start
    wait_for $PG_PORT
fi
psql_dev() { psql -h 127.0.0.1 -p $PG_PORT -U postgres -v ON_ERROR_STOP=1 -q "$@"; }
if [ "$(psql_dev -tAc "SELECT count(*) FROM pg_namespace WHERE nspname = 'app'")" = 0 ]; then
    psql_dev -f "$ROOT/dev/seed/postgres.sql" >/dev/null
fi

# MySQL: on MYSQL_PORT, root without a password over TCP, database `app`.
if ! listening $MYSQL_PORT; then
    printf '[mysqld]\nport = %s\nbind-address = 127.0.0.1\n' $MYSQL_PORT |
        $SUDO tee /etc/mysql/mysql.conf.d/zz-tangul.cnf >/dev/null
    $SUDO service mysql start >/dev/null
    wait_for $MYSQL_PORT
    # Ubuntu's root logs in through the socket only; allow TCP without a password.
    $SUDO mysql -uroot <<'SQL'
CREATE USER IF NOT EXISTS 'root'@'127.0.0.1' IDENTIFIED BY '';
GRANT ALL ON *.* TO 'root'@'127.0.0.1' WITH GRANT OPTION;
SQL
fi
mysql_dev() { mysql -h 127.0.0.1 -P $MYSQL_PORT -uroot "$@"; }
if ! mysql_dev -e "USE app; SELECT 1 FROM users LIMIT 1" >/dev/null 2>&1; then
    mysql_dev -e "CREATE DATABASE IF NOT EXISTS app"
    mysql_dev app <"$ROOT/dev/seed/mysql.sql" >/dev/null
fi

# SSH bastion: user/password tangul/tangul, TCP forwarding on (dev/ssh/Dockerfile).
if ! listening $SSH_PORT; then
    id tangul >/dev/null 2>&1 || $SUDO useradd -m -s /bin/sh tangul
    echo 'tangul:tangul' | $SUDO chpasswd
    $SUDO ssh-keygen -A >/dev/null
    $SUDO mkdir -p /run/sshd
    $SUDO /usr/sbin/sshd -p $SSH_PORT -o ListenAddress=127.0.0.1 \
        -o PasswordAuthentication=yes -o KbdInteractiveAuthentication=yes \
        -o AllowTcpForwarding=yes -o PermitRootLogin=no
    wait_for $SSH_PORT
fi

echo "dev-db-native: Postgres :$PG_PORT, MySQL :$MYSQL_PORT, SSH :$SSH_PORT"
