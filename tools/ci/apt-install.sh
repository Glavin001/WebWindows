#!/bin/sh
# Installs Ubuntu packages on a CI runner:
#
#   tools/ci/apt-install.sh package ...
#
# A download from the runners' archive mirror sometimes stops partway and
# never fails (apt's own timeouts don't fire), holding the job until its
# time limit. So the packages are downloaded first, each attempt with a
# limit on its whole duration and retried (apt resumes what it got), and
# installed from the downloads once all are in. A mirror that stalls tends
# to stall for every attempt, so the retries switch to other mirrors.
set -eu
apt="apt-get -qq -o Acquire::Retries=3 -o Acquire::http::Timeout=20"
from='https\?://azure\.archive\.ubuntu\.com/ubuntu'
for next in http://archive.ubuntu.com/ubuntu http://mirrors.edge.kernel.org/ubuntu ''; do
  if sudo timeout 90 $apt update && sudo timeout 120 $apt install -y --download-only "$@"; then
    sudo $apt install -y --no-download "$@"
    exit 0
  fi
  echo "apt: download failed or stalled" >&2
  [ -n "$next" ] || break
  echo "apt: switching to $next" >&2
  for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
    if [ -f "$f" ]; then sudo sed -i "s#$from#$next#g" "$f"; fi
  done
  from=$(printf '%s' "$next" | sed 's/[.]/\\./g')
done
exit 1
