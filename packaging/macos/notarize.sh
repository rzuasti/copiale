#!/usr/bin/env bash
# Notarizes FILE (a .zip of the app, or a .dmg) and fails unless Apple
# accepts it. Needs NOTARY_KEY (path to the .p8), KEY_ID and ISSUER_ID.
set -euo pipefail

file=$1
auth=(--key "$NOTARY_KEY" --key-id "$KEY_ID" --issuer "$ISSUER_ID")

id=$(xcrun notarytool submit "$file" "${auth[@]}" --output-format json | jq -r .id)
echo "Submitted $file: $id"

# A team's first submissions can take hours; waiting survives network drops.
for _ in $(seq 1 12); do
  if xcrun notarytool wait "$id" "${auth[@]}" --timeout 30m; then
    break
  fi
  sleep 30
done

status=$(xcrun notarytool info "$id" "${auth[@]}" --output-format json | jq -r .status)
if [ "$status" != Accepted ]; then
  echo "::error::Notarization of $file: $status"
  xcrun notarytool log "$id" "${auth[@]}" || true
  exit 1
fi
echo "Notarized $file"
