#!/usr/bin/env bash
# Publish www/ to https://ondewebdemo.5mb.app with the smbCloud CLI (smb >= 1.1.0).
#
#   ./deploy.sh --dry-run   stage and list the files; uploads nothing
#   ./deploy.sh             stage, deploy, then check /version.txt
#
# Run ./build.sh first: this ships whatever is in www/pkg. Auth is the token
# from `smb login`, or SMB_TOKEN in CI.
set -euo pipefail
cd "$(dirname "$0")"

# The App .smb/config.toml must name. A stale config once sent a deploy into
# another app's directory, so a mismatch stops here before anything is written.
EXPECTED_APP_ID="c0277812-c891-4f4b-a3ee-c8cb371bb629"
URL="https://ondewebdemo.5mb.app"

app_id="$(sed -n 's/^frontend_app_id *= *"\([^"]*\)".*/\1/p' .smb/config.toml | head -n 1)"
if [ "$app_id" != "$EXPECTED_APP_ID" ]; then
    echo "refusing to deploy: .smb/config.toml says frontend_app_id ${app_id:-?}, expected $EXPECTED_APP_ID" >&2
    exit 1
fi

for f in www/pkg/web_chat.js www/pkg/web_chat_bg.wasm; do
    [ -f "$f" ] || { echo "missing $f; run ./build.sh first" >&2; exit 1; }
done

version="$(git rev-parse --short HEAD)"
git diff --quiet HEAD -- . || version="$version-dirty"

# Only the servable files. www/pkg also holds a README, package.json and
# .gitignore from wasm-pack that have no place on the site.
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/pkg" "$stage/badges"
cp www/index.html www/main.js www/worker.js www/style.css www/favicon.svg www/apple-touch-icon.png "$stage/"
cp www/badges/app-store.svg www/badges/google-play.png "$stage/badges/"
cp www/pkg/web_chat.js www/pkg/web_chat_bg.wasm "$stage/pkg/"
printf '%s\n' "$version" > "$stage/version.txt"
# Config only: the CLI reads it and leaves it out of the upload.
cp -R .smb "$stage/.smb"

echo "version: $version"
echo "app:     $app_id"
echo "cli:     $(smb --version 2>&1)"

if [ "${1:-}" = "--dry-run" ]; then
    (cd "$stage" && smb --ci deploy --prebuilt --dry-run)
    exit 0
fi

(cd "$stage" && smb --ci deploy --prebuilt)

echo "waiting for $URL/version.txt to report $version"
for _ in $(seq 15); do
    live="$(curl -fsS --max-time 5 -H 'Cache-Control: no-cache' "$URL/version.txt" 2>/dev/null | tr -d '[:space:]' || true)"
    if [ "$live" = "$version" ]; then
        echo "live: $URL serves $version"
        exit 0
    fi
    sleep 2
done
echo "$URL did not report $version within 30s (last saw: ${live:-nothing})" >&2
exit 1
