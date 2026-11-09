#!/usr/bin/env bash
# Build Hub on GitHub Actions and install it on the Kritano droplet.
#
#   deploy/deploy.sh            build the current main, then install it
#   deploy/deploy.sh --latest   install the newest successful build instead
#
# Needs: gh (logged in), the kritano SSH key, and main pushed.
set -euo pipefail
cd "$(dirname "$0")/.."

HOST=root@167.71.134.177
KEY=${KRITANO_KEY:-$HOME/.ssh/keys/kritano}
REPO=Chrisgarlick/hub
ARTIFACT=hub-linux-x86_64

if [ "${1:-}" != "--latest" ]; then
  if [ -n "$(git status --porcelain)" ] || [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
    echo "Commit and push main first: the build uses what's on GitHub." >&2
    exit 1
  fi
  gh workflow run release.yml --repo "$REPO" --ref main
  sleep 5
  run=$(gh run list --repo "$REPO" --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')
  echo "Building (run $run)…"
  gh run watch "$run" --repo "$REPO" --exit-status >/dev/null
else
  run=$(gh run list --repo "$REPO" --workflow release.yml --status success --limit 1 --json databaseId -q '.[0].databaseId')
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
gh run download "$run" --repo "$REPO" -n "$ARTIFACT" -D "$tmp"
cp deploy/hub.service deploy/hub.kritano.com.conf deploy/install.sh "$tmp/"
chmod +x "$tmp/hub"

ssh -i "$KEY" "$HOST" 'rm -rf /tmp/hub-deploy && mkdir -p /tmp/hub-deploy'
scp -q -i "$KEY" "$tmp"/* "$HOST:/tmp/hub-deploy/"
ssh -i "$KEY" "$HOST" 'sh /tmp/hub-deploy/install.sh'
echo "Deployed run $run."
