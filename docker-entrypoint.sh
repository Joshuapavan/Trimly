#!/bin/sh
# Starts the Trimly server as an unprivileged user.
#
# Volumes (Docker named volumes, Fly.io volumes) are usually mounted owned by
# root, so this runs as root just long enough to hand the data folder to the
# trimly user, then drops privileges.
set -e

DATA="${TRIMLY_DATA_DIR:-/data}"
mkdir -p "$DATA"
# Big clips are cut in a temporary folder; keep it on the data volume if asked.
if [ -n "$TMPDIR" ]; then mkdir -p "$TMPDIR"; fi

if [ "$(id -u)" = "0" ]; then
  chown -R trimly:trimly "$DATA" ${TMPDIR:+"$TMPDIR"} 2>/dev/null || true
  exec setpriv --reuid=trimly --regid=trimly --init-groups "$@"
fi
exec "$@"
