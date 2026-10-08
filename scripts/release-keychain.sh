#!/usr/bin/env bash
# The Developer ID certificate and the notary key for a release build (GitHub
# Actions, environment `release`), in a temporary keychain that is removed again.
#
#   scripts/release-keychain.sh setup     # needs the six secrets below
#   scripts/release-keychain.sh cleanup   # always, at the end of the job
#
# Secrets: MACOS_CERTIFICATE_P12 (base64 .p12), MACOS_CERTIFICATE_PASSWORD,
# MACOS_SIGN_IDENTITY ("Developer ID Application: Name (TEAMID)"),
# NOTARY_API_KEY_P8 (the .p8's contents), NOTARY_KEY_ID, NOTARY_ISSUER_ID.
# Sets TANGUL_SIGN_IDENTITY and NOTARY_KEY_PATH for the later steps ($GITHUB_ENV).
# Never prints a secret.
set -euo pipefail

dir="${RUNNER_TEMP:?runs in GitHub Actions}/release-signing"
keychain="$dir/build.keychain-db"

case "${1:-}" in
setup)
  missing=()
  for name in MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD MACOS_SIGN_IDENTITY \
      NOTARY_API_KEY_P8 NOTARY_KEY_ID NOTARY_ISSUER_ID; do
    [[ -n "${!name:-}" ]] || missing+=("$name")
  done
  if ((${#missing[@]})); then
    echo "::error::release secrets missing in the 'release' environment: ${missing[*]}" \
      "(or set the repository variable UNSIGNED_RELEASE=true for an ad-hoc signed release)"
    exit 1
  fi
  if [[ "$MACOS_SIGN_IDENTITY" != "Developer ID Application: "* ]]; then
    echo "::error::MACOS_SIGN_IDENTITY must be a 'Developer ID Application: …' identity"
    exit 1
  fi
  mkdir -p "$dir" && chmod 700 "$dir"
  password=$(openssl rand -hex 24)
  echo "::add-mask::$password"
  printf '%s' "$MACOS_CERTIFICATE_P12" | base64 --decode > "$dir/cert.p12"
  security create-keychain -p "$password" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$password" "$keychain"
  security import "$dir/cert.p12" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" \
    -t cert -f pkcs12 -T /usr/bin/codesign >/dev/null
  security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
  # Search this keychain first, then the existing ones.
  security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
  rm -f "$dir/cert.p12"
  if ! security find-identity -v -p codesigning "$keychain" | grep -qF "\"$MACOS_SIGN_IDENTITY\""; then
    echo "::error::the certificate doesn't hold the identity in MACOS_SIGN_IDENTITY"
    exit 1
  fi
  printf '%s\n' "$NOTARY_API_KEY_P8" > "$dir/AuthKey.p8"
  chmod 600 "$dir/AuthKey.p8"
  {
    echo "TANGUL_SIGN_IDENTITY=$MACOS_SIGN_IDENTITY"
    echo "NOTARY_KEY_PATH=$dir/AuthKey.p8"
  } >> "$GITHUB_ENV"
  echo "signing identity ready"
  ;;
cleanup)
  security delete-keychain "$keychain" 2>/dev/null || true
  rm -rf "$dir"
  ;;
*)
  echo "usage: $0 setup|cleanup" >&2
  exit 2
  ;;
esac
