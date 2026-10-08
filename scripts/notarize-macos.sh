#!/usr/bin/env bash
# Notarizes a signed DMG with Apple (an App Store Connect API key) and staples the
# ticket to it, so it passes Gatekeeper offline too. Fails on anything but
# "Accepted", with Apple's log.
#
#   scripts/notarize-macos.sh target/bundle/tangul-0.2.0-universal.dmg
#
# Needs NOTARY_KEY_PATH (the .p8), NOTARY_KEY_ID and NOTARY_ISSUER_ID
# (scripts/release-keychain.sh sets them up in CI).
set -euo pipefail

dmg="${1:?usage: $0 <dmg>}"
key=(--key "${NOTARY_KEY_PATH:?}" --key-id "${NOTARY_KEY_ID:?}" --issuer "${NOTARY_ISSUER_ID:?}")

result=$(xcrun notarytool submit "$dmg" "${key[@]}" --wait --timeout 45m --output-format json)
field() { python3 -c "import json,sys; print(json.load(sys.stdin).get('$1', ''))" <<<"$result"; }
status=$(field status)
id=$(field id)
echo "notarization $id: $status"
if [[ "$status" != "Accepted" ]]; then
  [[ -n "$id" ]] && xcrun notarytool log "$id" "${key[@]}" || true
  exit 1
fi
xcrun stapler staple "$dmg"
