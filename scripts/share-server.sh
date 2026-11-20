#!/usr/bin/env bash
# Runs the Trimly server on this computer and shares it over the internet with a
# free Cloudflare quick tunnel (no account needed).
#
#   scripts/share-server.sh
#
# It prints a https://....trycloudflare.com address. Put that in Vercel as
# TRIMLY_API_URL (and redeploy), then enter the access key it prints when you
# open your site. Press Ctrl+C to stop sharing.
#
# Good to know:
# - The address changes every time you start the tunnel, so Vercel's
#   TRIMLY_API_URL has to be updated each time.
# - The site only works while this computer is on and this script is running.
# - Optional settings (environment variables):
#     TRIMLY_ALLOW_ORIGIN   your site's address, e.g. https://trimly-clipy.vercel.app
#     TRIMLY_KEY_FILE       where the access key is kept (default ~/.trimly-access-key)
#     PORT                  local port (default 8787)

set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

PORT="${PORT:-8787}"
BIN="target/release/trimly-server"
KEY_FILE="${TRIMLY_KEY_FILE:-$HOME/.trimly-access-key}"
DATA="${TRIMLY_DATA_DIR:-$PWD/data}"
LOGS="$DATA/logs"

command -v cloudflared >/dev/null || {
  echo "cloudflared isn't installed. Get it from https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/" >&2
  exit 1
}
command -v yt-dlp >/dev/null || echo "Warning: yt-dlp wasn't found on your PATH. The server needs it." >&2
command -v ffmpeg >/dev/null || echo "Warning: ffmpeg wasn't found on your PATH. The server needs it." >&2

[ -x "$BIN" ] || cargo build --release -p trimly-server

# The access key: made once, then reused.
if [ ! -s "$KEY_FILE" ]; then
  ( umask 077; head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$KEY_FILE" )
fi
KEY="$(cat "$KEY_FILE")"

mkdir -p "$LOGS"
if lsof -ti ":$PORT" >/dev/null 2>&1; then
  echo "Port $PORT is already in use. Stop whatever is using it, or set PORT=another." >&2
  exit 1
fi

TRIMLY_ACCESS_KEY="$KEY" \
TRIMLY_DATA_DIR="$DATA" \
TRIMLY_ALLOW_ORIGIN="${TRIMLY_ALLOW_ORIGIN:-*}" \
PORT="$PORT" \
  "$BIN" > "$LOGS/server.log" 2>&1 &
SERVER_PID=$!

cloudflared tunnel --no-autoupdate --url "http://localhost:$PORT" > "$LOGS/tunnel.log" 2>&1 &
TUNNEL_PID=$!

cleanup() { kill "$TUNNEL_PID" "$SERVER_PID" 2>/dev/null || true; }
trap cleanup INT TERM EXIT

URL=""
for _ in $(seq 1 40); do
  URL="$(grep -Eo 'https://[a-z0-9-]+\.trycloudflare\.com' "$LOGS/tunnel.log" | head -1 || true)"
  [ -n "$URL" ] && break
  kill -0 "$TUNNEL_PID" 2>/dev/null || { echo "The tunnel stopped. See $LOGS/tunnel.log" >&2; exit 1; }
  sleep 1
done
[ -n "$URL" ] || { echo "No tunnel address appeared. See $LOGS/tunnel.log" >&2; exit 1; }

echo
echo "Trimly is shared at:   $URL"
echo "Access key:            $KEY"
echo "Vercel setting:        TRIMLY_API_URL = $URL"
echo
echo "Logs are in $LOGS. Press Ctrl+C to stop."
wait "$SERVER_PID"
