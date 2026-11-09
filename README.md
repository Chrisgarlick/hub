# Hub

What you know about each client, and where every job starts: per-client
markdown (briefs, voice, constraints, rules, decisions), searched by meaning
and by words, a library of reusable files for the other apps, and one MCP for
Claude to start from.

- Spec: [`spec.md`](spec.md)
- Build order: [`plan.md`](plan.md)
- Deploying: [`deploy/README.md`](deploy/README.md)

## Layout

```
crates/hub         the app (server, CLI, embedded frontend)
crates/hub-model   Hub's published types
web/               frontend source; web/dist is served by the app
```

## Development

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
