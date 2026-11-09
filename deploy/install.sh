#!/bin/sh
# Installs or updates Hub on the Kritano droplet, as root, from files copied
# to /tmp/hub-deploy (deploy/deploy.sh does both). Safe to run again: the
# database and hub.toml are made once and never overwritten.
set -eu
SRC=/tmp/hub-deploy
DOMAIN=hub.kritano.com
IP=167.71.134.177

# pgvector for the droplet's Postgres 16. --no-upgrade: never upgrade (and
# so restart) the Postgres that Kritano and the others share; if pgvector
# needs a newer Postgres, stop and do that in a quiet moment by hand. (The
# first install, on 2026-11-09, upgraded postgresql-16 and restarted it for
# 14 seconds.)
if [ ! -f /usr/share/postgresql/16/extension/vector.control ]; then
  apt-get install -y -q --no-upgrade postgresql-16-pgvector >/dev/null
  [ -f /usr/share/postgresql/16/extension/vector.control ] \
    || { echo "pgvector didn't install without upgrading Postgres; install it by hand" >&2; exit 1; }
fi

id hub >/dev/null 2>&1 || useradd --system --home /var/lib/hub --shell /usr/sbin/nologin hub
install -d -o hub -g hub -m 0750 /var/lib/hub

# The database, its role and hub.toml, once. The password goes straight
# into the config file (0600), reaches psql on stdin (never in a process
# list) and is never printed.
if [ ! -f /var/lib/hub/hub.toml ]; then
  PASS=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
  if sudo -u postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname = 'hub'" | grep -q 1; then
    printf "ALTER ROLE hub WITH LOGIN PASSWORD '%s';\n" "$PASS" | sudo -u postgres psql -q
  else
    printf "CREATE ROLE hub WITH LOGIN PASSWORD '%s';\n" "$PASS" | sudo -u postgres psql -q
  fi
  sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname = 'hub'" | grep -q 1 \
    || sudo -u postgres createdb -O hub hub
  printf 'bind = "127.0.0.1:8160"\ndatabase_url = "postgres://hub:%s@127.0.0.1:5432/hub"\npublic_origin = "https://%s"\nclients_url = "http://127.0.0.1:8130"\n' "$PASS" "$DOMAIN" \
    | install -o hub -g hub -m 0600 /dev/stdin /var/lib/hub/hub.toml
  unset PASS
fi
# pgvector isn't a trusted extension, so the superuser enables it.
sudo -u postgres psql -q -d hub -c "CREATE EXTENSION IF NOT EXISTS vector"

install -m 0755 "$SRC/hub" /usr/local/bin/hub
install -m 0644 "$SRC/hub.service" /etc/systemd/system/

# nginx once, then certbot once DNS points here. Later template changes are
# flagged, not applied, so certbot's edits survive.
CONF=/etc/nginx/sites-available/$DOMAIN
if [ ! -f "$CONF" ]; then
  install -m 0644 "$SRC/$DOMAIN.conf" "$CONF"
  ln -sf "$CONF" /etc/nginx/sites-enabled/$DOMAIN
  nginx -t && systemctl reload nginx
elif ! cmp -s "$SRC/$DOMAIN.conf" "$CONF.template"; then
  echo "note: deploy/$DOMAIN.conf has changed; merge it into $CONF by hand" >&2
fi
install -m 0644 "$SRC/$DOMAIN.conf" "$CONF.template"
if [ ! -d /etc/letsencrypt/live/$DOMAIN ]; then
  if getent hosts $DOMAIN | grep -q "$IP"; then
    certbot --nginx -d $DOMAIN --redirect -n
  else
    echo "note: $DOMAIN doesn't point at $IP yet; add the DNS record and deploy again for TLS" >&2
  fi
fi

systemctl daemon-reload
systemctl enable hub >/dev/null
systemctl restart hub
for _ in 1 2 3 4 5 6 7 8 9 10; do
  curl -sf 127.0.0.1:8160/healthz >/dev/null && break
  sleep 0.5
done
systemctl is-active hub
curl -s 127.0.0.1:8160/readyz && echo
