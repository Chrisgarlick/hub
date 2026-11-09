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

## Day to day

```sh
journalctl -u hub -n 100
curl -s 127.0.0.1:8160/readyz     # database, pgvector, Clients
```
