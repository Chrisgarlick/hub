#!/usr/bin/env bash
# The checks, run before every push (.githooks/pre-push) and on GitHub.
# Database tests run when HUB_TEST_DATABASE_URL points at a Postgres with
# pgvector (CI provides one); without it they say so and skip.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== frontend"
(cd web && { [ -d node_modules ] || npm ci --silent; } && npm run -s typecheck && npm run -s build)

echo "== rust"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

printf '\033[1;32mAll checks passed.\033[0m\n'
