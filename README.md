# Hub

What you know about each client, and where every job starts: per-client
markdown (briefs, voice, constraints, rules, decisions), searched by meaning
and by words, a library of reusable files for the other apps, and one MCP for
Claude to start from.

- Spec: [`spec.md`](spec.md)
- Build order: [`plan.md`](plan.md)
- Deploying: [`deploy/README.md`](deploy/README.md)

## Brand downloads (`/v1`)

Site builds get a client's published brand from Hub, which passes it through
from Clients (spec §8.3). They need a Hub API token with the `brand:read`
scope, made on the server and printed once:

```sh
hub admin token create --name wp-base --scope brand:read   # prints hub_…
hub admin token list
hub admin token revoke wp-base                              # stops at once
```

```sh
H="Authorization: Bearer $HUB_TOKEN"
curl -sfH "$H" https://hub.kritano.com/v1/clients/acme/brand            # brand JSON
curl -sfH "$H" https://hub.kritano.com/v1/clients/acme/brand?version=3  # a given version
curl -sfH "$H" -o logo.svg https://hub.kritano.com/v1/clients/acme/brand/assets/logo
curl -sfH "$H" -o brand.zip https://hub.kritano.com/v1/clients/acme/brand/bundle
```

- `/brand`: the brand JSON exactly as Clients publishes it, with `version`;
  each `assets.<name>` also has `use` (font, logo, image), `file` and `url`.
- `/brand/assets/{name}`: that file, with its content type, and its SHA-256
  as the ETag and `X-Content-SHA256`.
- `/brand/bundle`: a zip of `brand.json` and `assets/<file>` (up to 64 MB).
- 404 with a message when the client, a published brand or the version
  doesn't exist. The token works nowhere else (the API and MCP refuse it);
  calls are logged, 120 a minute per token.

## Layout

```
crates/hub         the app (server, CLI, embedded frontend)
crates/hub-model   Hub's published types
web/               frontend source; web/dist is served by the app
```

## Development

`bin/setup` gets a fresh clone ready (packages, web/dist, crates, hub.toml, the pre-push hook) and is safe to run again. By hand:

```sh
cp hub.example.toml hub.toml          # point database_url at a Postgres with pgvector
(cd web && npm install && npm run watch)
cargo run -- serve                    # http://localhost:8160
./scripts/ci.sh                       # every check (runs before each push)
git config core.hooksPath .githooks   # once per clone
```

A local Postgres with pgvector, if you have Docker:

```sh
docker run -d --name hub-pg -p 54329:5432 -e POSTGRES_USER=hub -e POSTGRES_PASSWORD=hub pgvector/pgvector:pg16
export HUB_TEST_DATABASE_URL=postgres://hub:hub@127.0.0.1:54329/hub   # runs the database tests too
```
