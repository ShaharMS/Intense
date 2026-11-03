#!/bin/sh
set -eu
cd "$(dirname "$0")"
for archive in river-backend-linux-amd64.tar.gz river-ui-linux-amd64.tar.gz; do
  expected=$(awk -v name="$archive" '$2 == name { print $1 }' SHA256SUMS)
  actual=$(sha256sum "$archive" | awk '{print $1}')
  [ "$actual" = "$expected" ] || { echo "Checksum failed: $archive" >&2; exit 1; }
  docker load --input "$archive"
done
if [ ! -f .env ]; then
  cp .env.example .env
fi
if [ "${1:-}" = --gpu ]; then
  docker compose -p river-portable -f compose.yaml -f compose.gpu.yaml up -d --wait --wait-timeout 240
else
  docker compose -p river-portable -f compose.yaml up -d --wait --wait-timeout 240
fi
echo 'River started. Default UI: http://127.0.0.1:18765/; Swagger: http://127.0.0.1:18080/docs/'
