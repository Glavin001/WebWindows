#!/bin/sh
# Installs Ubuntu packages on a CI runner:
#
#   tools/ci/apt-install.sh package ...
#
# A download from the runners' archive mirror sometimes stops partway and
# never fails (apt's own timeouts don't fire), holding the job until its
# time limit. So the packages are downloaded first, each attempt with a
# limit on its whole duration and retried (apt resumes what it got), and
# installed from the downloads once all are in.
set -eu
apt="apt-get -qq -o Acquire::Retries=3 -o Acquire::http::Timeout=20"
for attempt in 1 2 3; do
  if sudo timeout 90 $apt update && sudo timeout 120 $apt install -y --download-only "$@"; then
    sudo $apt install -y --no-download "$@"
    exit 0
  fi
  echo "apt: download attempt $attempt failed or stalled" >&2
done
exit 1
