#!/usr/bin/env bash
# Notarizes and staples FILE (a .dmg). Needs NOTARY_KEY (path to the .p8),
# KEY_ID and ISSUER_ID.
set -euo pipefail

file=$1
auth=(--key "$NOTARY_KEY" --key-id "$KEY_ID" --issuer "$ISSUER_ID")
ids=()
submitted=0

# Apple sits on some submissions for days: submits again every 2 hours, and
# staples from any accepted one, earlier runs' too. Network errors only skip
# a poll.
while :; do
  if xcrun stapler staple "$file" >/dev/null 2>&1; then
    echo "Notarized and stapled $file"
    exit 0
  fi
  if [ ${#ids[@]} -eq 0 ] || ((SECONDS - submitted >= 7200)); then
    if id=$(xcrun notarytool submit "$file" "${auth[@]}" --output-format json | jq -er .id); then
      ids+=("$id")
      submitted=$SECONDS
      echo "Submitted $file: $id"
    fi
  fi
  for id in "${ids[@]}"; do
    status=$(xcrun notarytool info "$id" "${auth[@]}" --output-format json | jq -r .status) || status=
    case $status in
      Invalid | Rejected)
        echo "::error::Notarization $id: $status"
        xcrun notarytool log "$id" "${auth[@]}" || true
        exit 1
        ;;
    esac
    echo "$(date -u +%H:%M) $id: ${status:-no answer}"
  done
  sleep 120
done
