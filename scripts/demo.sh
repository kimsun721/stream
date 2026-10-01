#!/usr/bin/env bash
# Plays the backend's part in a broadcast: creates a room, waits for OBS to
# start publishing into it, takes it live, and deletes it on the way out.
#
#   scripts/demo.sh
#
# The API key is read from the environment or from .env, and never leaves this
# terminal. CONTROL_URL and PUBLIC_URL override where the server is reached.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ -z "${API_KEY:-}" && -f .env ]]; then
  API_KEY=$(sed -n 's/^API_KEY=["'\'']\{0,1\}\([^"'\'']*\)["'\'']\{0,1\}$/\1/p' .env)
fi

if [[ -z "${API_KEY:-}" ]]; then
  echo "API_KEY is not set, in the environment or in .env" >&2
  exit 1
fi

CONTROL_URL=${CONTROL_URL:-http://localhost:8080}
PUBLIC_URL=${PUBLIC_URL:-http://localhost:8443}

control() {
  local method=$1 path=$2
  shift 2
  curl -fsS -X "$method" "$CONTROL_URL$path" -H "Authorization: Bearer $API_KEY" "$@"
}

field() {
  sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"
}

room=$(control POST /rooms)
room_id=$(field room_id <<<"$room")
stream_key=$(field stream_key <<<"$room")

cleanup() {
  control DELETE "/rooms/$room_id" >/dev/null 2>&1 || true
  echo
  echo "Room $room_id deleted."
}
trap cleanup EXIT
trap 'exit 130' INT TERM

cat <<EOF

Room $room_id created.

In OBS 30 or later, Settings, Stream:
  Service       WHIP
  Server        $PUBLIC_URL/whip
  Bearer Token  $stream_key

Simulcast needs OBS 32.1 or later, with more than one layer set.

Waiting for OBS to start publishing...
EOF

until control GET "/rooms/$room_id" | grep -q '"state":"Preview"'; do
  sleep 1
done

echo
read -rp "OBS is publishing. Press Enter to go live. "
control PATCH "/rooms/$room_id" -H "Content-Type: application/json" -d '{"state":"Live"}'

cat <<EOF

Live. Watch at:
  $PUBLIC_URL/watch?room=$room_id

EOF

read -rp "Press Enter to end the broadcast. "
control PATCH "/rooms/$room_id" -H "Content-Type: application/json" -d '{"state":"Idle"}'
