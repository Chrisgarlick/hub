# Deploying Hub

Hub runs on the Kritano droplet (`167.71.134.177`) next to Clients, Board and
Motion: a systemd service on `127.0.0.1:8160`, data in Postgres 16 (a `hub`
database with pgvector), nginx in front at `https://hub.kritano.com`
(certbot for TLS).

## Deploy (first time and every update)

From `~/dev/hub`, with main committed and pushed:

```sh
deploy/deploy.sh            # builds on GitHub Actions, then installs
deploy/deploy.sh --latest   # install the newest successful build
```

`install.sh` (run on the droplet by `deploy.sh`) is safe to repeat. The first
time it installs `postgresql-16-pgvector`, creates the `hub` role and
database (a random password, written only to `/var/lib/hub/hub.toml`, mode
0600), enables the extension, and sets up nginx. It gets the certificate once
`hub.kritano.com` points at the droplet. Migrations run at start-up.

## Connecting to Clients

Hub gets its spaces from Clients and signs you in with "Sign in with Clients".
Clients needs a build with the `client.created` event (from 2026-11-09), and
both of these, once.

**On the droplet:** register Hub for sign-in, then give Hub a service key.

```sh
# 1. Sign in with Clients: add hub to CLIENTS_APPS.
grep -q 'hub=https://hub.kritano.com' /etc/clients.env \
  || sed -i 's|^CLIENTS_APPS=.*|&,hub=https://hub.kritano.com|' /etc/clients.env
grep '^CLIENTS_APPS=' /etc/clients.env
systemctl restart clients && systemctl is-active clients

# 2. A service key for Hub. Only the ck_… part is kept.
KEY=$(clients-admin keys create hub --kind service 2>/dev/null | grep -o 'ck_[A-Za-z0-9_-]*' | head -n 1)
echo ${#KEY}        # about 70; 0 means it wasn't captured: stop here
printf 'clients_service_key = "%s"\n' "$KEY" >> /var/lib/hub/hub.toml && unset KEY
grep -c '^clients_service_key = "ck_[^"]*"$' /var/lib/hub/hub.toml   # should print 1
systemctl restart hub && systemctl is-active hub
journalctl -u hub -n 20 | grep -i space   # "space created" for each client
```

Then open `https://hub.kritano.com`: it sends you to Clients to sign in and
back. Every client in Clients has a space within a minute (Hub reconciles
against Clients' list at start-up and every 10 minutes, and follows its
events every 30 seconds). **Check Clients now** on the spaces page does it
at once.

## Day to day

```sh
journalctl -u hub -n 100
curl -s 127.0.0.1:8160/readyz     # database, pgvector, Clients
```
