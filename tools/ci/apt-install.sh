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
#
# With APT_ARCHIVES set (.github/actions/apt, which caches it between runs)
# the .deb files go to that directory, and when it already holds all of
# them the packages install without the network.
set -eu
# IPv4 only: IPv6 is a known cause of stalled apt downloads on hosted runners.
apt="apt-get -qq -o Acquire::Retries=3 -o Acquire::http::Timeout=20 -o Acquire::ForceIPv4=true"
archives=${APT_ARCHIVES:-}
if [ -n "$archives" ]; then
  mkdir -p "$archives/partial"
  apt="$apt -o Dir::Cache::Archives=$archives"
  if sudo $apt install -y --no-download "$@" > /dev/null 2>&1; then
    echo "apt: installed from $archives" >&2
    exit 0
  fi
fi
# The cache keeps whole files, readable by the runner's user.
done_downloads() {
  if [ -n "$archives" ]; then
    sudo rm -rf "$archives/partial" "$archives/lock"
    sudo chmod -R a+rX "$archives"
  fi
}
from='https\?://azure\.archive\.ubuntu\.com/ubuntu'
for next in http://archive.ubuntu.com/ubuntu http://mirrors.edge.kernel.org/ubuntu ''; do
  if ! sudo timeout 90 $apt update; then
    echo "apt: update failed or stalled" >&2
  elif ! sudo timeout 120 $apt install -y --download-only "$@"; then
    echo "apt: package download failed or stalled" >&2
  else
    sudo $apt install -y --no-download "$@"
    done_downloads
    exit 0
  fi
  [ -n "$next" ] || break
  echo "apt: switching to $next" >&2
  for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
    if [ -f "$f" ]; then sudo sed -i "s#$from#$next#g" "$f"; fi
  done
  from=$(printf '%s' "$next" | sed 's/[.]/\\./g')
done
exit 1
