#!/usr/bin/env bash
# Simulate a signed WhatsApp webhook POST against a local server, so the
# whole pipeline is testable before Meta's app/number/template review.
#
#   ./scripts/simulate.sh text "check this out https://youtu.be/dQw4w9WgXcQ"
#   ./scripts/simulate.sh text "where was that pasta place?"
#   ./scripts/simulate.sh text "great carbonara at Roscioli in Rome"
#
# Requires WA_APP_SECRET in the environment (or .env next to this script).

set -euo pipefail

if [[ -f "$(dirname "$0")/../.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$(dirname "$0")/../.env"
  set +a
fi

HOST="${HOST:-http://localhost:8080}"
SECRET="${WA_APP_SECRET:?WA_APP_SECRET must be set}"
FROM="${FROM:-919999999999}"
TYPE="${1:-text}"
BODY_TEXT="${2:-hello from simulate.sh}"
MSG_ID="wamid.test.$(date +%s%N)"

case "$TYPE" in
  text)
    MESSAGE=$(cat <<EOF
{"from":"$FROM","id":"$MSG_ID","timestamp":"$(date +%s)","type":"text","text":{"body":$(printf '%s' "$BODY_TEXT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}}
EOF
)
    ;;
  forwarded)
    MESSAGE=$(cat <<EOF
{"from":"$FROM","id":"$MSG_ID","timestamp":"$(date +%s)","type":"text","context":{"forwarded":true},"text":{"body":$(printf '%s' "$BODY_TEXT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}}
EOF
)
    ;;
  *)
    echo "usage: $0 {text|forwarded} \"message body\"" >&2
    exit 1
    ;;
esac

PAYLOAD=$(cat <<EOF
{"object":"whatsapp_business_account","entry":[{"id":"0","changes":[{"field":"messages","value":{"messaging_product":"whatsapp","metadata":{"display_phone_number":"0","phone_number_id":"0"},"contacts":[{"profile":{"name":"Test"},"wa_id":"$FROM"}],"messages":[$MESSAGE]}}]}]}
EOF
)

SIG=$(printf '%s' "$PAYLOAD" | openssl dgst -sha256 -hmac "$SECRET" -hex | awk '{print $NF}')

curl -sS -X POST "$HOST/webhook/whatsapp" \
  -H "Content-Type: application/json" \
  -H "X-Hub-Signature-256: sha256=$SIG" \
  -d "$PAYLOAD"
echo
