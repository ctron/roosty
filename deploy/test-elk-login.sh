#!/usr/bin/env sh
set -eu

response="$(curl -ksS \
  -X POST \
  -H 'content-type: application/json' \
  --data '{"origin":"https://localhost:4001","force_login":false,"lang":"en-US"}' \
  https://localhost:4001/api/roosty.localhost:4000/login)"

case "$response" in
  https://roosty.localhost:4000/oauth/authorize\?*)
    printf '%s\n' "$response"
    ;;
  *)
    printf 'unexpected Elk login response:\n%s\n' "$response" >&2
    exit 1
    ;;
esac
