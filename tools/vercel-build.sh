#!/bin/sh
# Vercel build: the page with windowed Wine programs (runtime/web).
#
# The site needs Wine's i386 DLLs (MinGW), Wine's Unix side (Emscripten) and
# their translations, which CI builds; CI's wine job publishes the site
# tools/site/build.sh assembles as assets of the "site-preview" release:
# site-<commit>.tar.gz, and site-latest-<branch>.tar.gz. CI runs for pull
# requests and main, so for those this deploys the commit's site, waiting up
# to SITE_WAIT_MINUTES (default 25) for CI to publish it. Otherwise (or when
# it does not come) it deploys the branch's latest site, then main's, so a
# preview always shows the newest working state.
set -eu
cd "$(dirname "$0")/.."
repo=${SITE_REPO:-Glavin001/WebWindows}
base="https://github.com/$repo/releases/download/site-preview"
sha=${VERCEL_GIT_COMMIT_SHA:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}
branch=$(echo "${VERCEL_GIT_COMMIT_REF:-main}" | tr '/' '-')
wait_s=$(( ${SITE_WAIT_MINUTES:-25} * 60 ))
# A branch push without a pull request gets no CI run: nothing to wait for.
if [ -z "${VERCEL_GIT_PULL_REQUEST_ID:-}" ] && [ "$branch" != main ]; then wait_s=0; fi

fetch() { curl -sfL --retry 3 -o site.tar.gz "$base/$1"; }

start=$(date +%s)
found=""
while :; do
  if fetch "site-$sha.tar.gz"; then found="commit $sha"; break; fi
  if [ $(( $(date +%s) - start )) -ge "$wait_s" ]; then break; fi
  echo "waiting for CI to publish site-$sha.tar.gz ($(( $(date +%s) - start ))s)"
  sleep 30
done
if [ -z "$found" ]; then
  for name in "site-latest-$branch.tar.gz" "site-latest-main.tar.gz"; do
    if fetch "$name"; then found="$name (not yet built for $sha)"; break; fi
  done
fi

rm -rf dist
mkdir -p dist
if [ -n "$found" ]; then
  tar xzf site.tar.gz -C dist
  rm -f site.tar.gz
  echo "deploying the site of $found: $(cat dist/version.txt 2>/dev/null)"
else
  cat > dist/index.html <<'HTML'
<!doctype html>
<meta charset="utf-8">
<title>WebWindows</title>
<p>No site has been published yet: CI's wine job builds it (see the
"site-preview" release). Redeploy once it has run.</p>
HTML
fi
