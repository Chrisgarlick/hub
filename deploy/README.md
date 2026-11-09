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

## Search by meaning (Voyage)

Without a key, Hub searches by words only. To add one, append to
`/var/lib/hub/hub.toml` and restart:

```toml
embedding_provider = "voyage"
embedding_key = "pa-…"
# The free tier's limits (the defaults); raise them once the Voyage
# account has a payment method.
# embedding_requests_per_minute = 3
# embedding_tokens_per_minute = 10000
```

Hub then embeds everything in the background (on the free tier, a few
minutes per hundred sections) and `curl -s 127.0.0.1:8160/readyz` stays
ready throughout. Progress: `/api/index` (`embedded` of `chunks`). A
document marked `visibility: secret` is never sent to Voyage.

After changing the model: `sudo -u hub hub --config /var/lib/hub/hub.toml reindex`.

## Letting Claude manage clients

Hub's MCP can add, rename and delete clients (deleting waits for your
approval). Clients has to allow Hub's key; on the droplet, after deploying a
Clients build with that feature:

```sh
grep -q '^CLIENTS_MANAGERS=' /etc/clients.env || echo 'CLIENTS_MANAGERS=hub' >> /etc/clients.env
systemctl restart clients && systemctl is-active clients
```

## Claude Code (the MCP)

Hub's MCP is at `https://hub.kritano.com/mcp`, with a Clients caller key.
From your Mac, this makes the key on the droplet and adds Hub to Claude Code
in one go, without the key appearing on screen:

```sh
KEY=$(ssh -i ~/.ssh/keys/kritano root@167.71.134.177 \
  'clients-admin keys create claude-code --kind caller 2>/dev/null | grep -o "ck_[A-Za-z0-9_-]*" | head -n 1')
echo ${#KEY}   # about 70; 0 means it wasn't captured: stop here
claude mcp add --scope user --transport http hub https://hub.kritano.com/mcp \
  --header "Authorization: Bearer $KEY" && unset KEY
```

What Claude changes through it waits in Hub's Approvals (`/approvals`) unless
the hands-off switch (`/settings`) lets that kind through. If the key leaks:
`clients-admin keys revoke <id>` on the droplet (Hub forgets it within a
minute), then run the above again.

## The library and Design's styles

Design keeps its style references in Hub's library. Once, after deploying
Hub, copy Design's existing styles across (safe to run again):

```sh
sqlite3 -json /var/lib/design/design.db "SELECT name, markdown, source FROM styles" \
  | sudo -u hub hub --config /var/lib/hub/hub.toml import-styles
```

Then point Design at Hub (it uses its Clients key) and restart it:

```sh
grep -q '^DESIGN_HUB_URL=' /etc/design.env || echo 'DESIGN_HUB_URL=http://127.0.0.1:8160' >> /etc/design.env
systemctl restart design && journalctl -u design -n 20 | grep -i hub   # "styles from Hub"
```

## Day to day

```sh
journalctl -u hub -n 100
curl -s 127.0.0.1:8160/readyz     # database, pgvector, Clients
```
