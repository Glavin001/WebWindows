#!/bin/sh
# Deploys the static site to Vercel as a prebuilt output, so Vercel only
# uploads and serves it (no build on Vercel's side):
#
#   tools/site/deploy.sh [preview|production]      (default: preview)
#
# Needs what tools/site/build.sh needs, and VERCEL_TOKEN, VERCEL_ORG_ID and
# VERCEL_PROJECT_ID in the environment (CI sets them). The deployment's URL
# is the last line of the output.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
target=${1:-preview}
out=$root/.vercel/output
rm -rf "$out"
sh "$root/tools/site/build.sh" "$out/static" >&2
# Build Output API v3. SharedArrayBuffer needs cross-origin isolation.
cat > "$out/config.json" <<'JSON'
{
  "version": 3,
  "routes": [
    {
      "src": "/(.*)",
      "headers": {
        "Cross-Origin-Opener-Policy": "same-origin",
        "Cross-Origin-Embedder-Policy": "require-corp"
      },
      "continue": true
    }
  ]
}
JSON
prod=
[ "$target" = production ] && prod=--prod
cd "$root"
log=$(mktemp)
if npx --yes vercel@62 deploy --prebuilt $prod --token "$VERCEL_TOKEN" --yes 2> "$log"; then
  cat "$log" >&2
  exit 0
fi
cat "$log" >&2
# Vercel's free plan takes 5000 file uploads a day and refuses the rest with
# this code until the next day. Still a failure (nothing was deployed), but
# one that says so: the commit is not at fault.
if grep -q 'api-upload-free' "$log"; then
  echo "::error title=Vercel's daily upload limit::Nothing deployed: the plan's 5000 uploads a day are used up (it resets within 24 hours). Not a fault of this commit." >&2
fi
exit 1
