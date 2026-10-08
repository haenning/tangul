#!/usr/bin/env bash
# The signing certificate (and for a Developer ID the notary key) for a release build
# (GitHub Actions, environment `release`), in a temporary keychain removed again.
#
#   scripts/release-keychain.sh setup
#   scripts/release-keychain.sh cleanup   # always, at the end of the job
#
# Secrets: MACOS_CERTIFICATE_P12 (base64 .p12), MACOS_CERTIFICATE_PASSWORD and
# MACOS_SIGN_IDENTITY (the certificate's name). Either a Developer ID
# ("Developer ID Application: Name (TEAMID)"), which also needs NOTARY_API_KEY_P8 (the
# .p8's contents), NOTARY_KEY_ID and NOTARY_ISSUER_ID and gets notarized; or the
# self-signed one from scripts/release-signing-cert.sh: a stable signature, so the
# Keychain's "Always Allow" survives updates, but Gatekeeper still asks.
# Sets TANGUL_SIGN_IDENTITY, NOTARIZE and NOTARY_KEY_PATH for the later steps
# ($GITHUB_ENV). Never prints a secret.
set -euo pipefail

dir="${RUNNER_TEMP:?runs in GitHub Actions}/release-signing"
keychain="$dir/build.keychain-db"

case "${1:-}" in
setup)
  needed=(MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD MACOS_SIGN_IDENTITY)
  notarize=false
  if [[ "${MACOS_SIGN_IDENTITY:-}" == "Developer ID Application: "* ]]; then
    notarize=true
    needed+=(NOTARY_API_KEY_P8 NOTARY_KEY_ID NOTARY_ISSUER_ID)
  fi
  missing=()
  for name in "${needed[@]}"; do
    [[ -n "${!name:-}" ]] || missing+=("$name")
  done
  if ((${#missing[@]})); then
    echo "::error::release secrets missing in the 'release' environment: ${missing[*]}" \
      "(or set the repository variable UNSIGNED_RELEASE=true for an ad-hoc signed release)"
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
  # Without -v: a self-signed certificate isn't trusted here, and needn't be.
  if ! security find-identity -p codesigning "$keychain" | grep -qF "\"$MACOS_SIGN_IDENTITY\""; then
    echo "::error::the certificate doesn't hold the identity in MACOS_SIGN_IDENTITY"
    exit 1
  fi
  {
    echo "TANGUL_SIGN_IDENTITY=$MACOS_SIGN_IDENTITY"
    echo "NOTARIZE=$notarize"
  } >> "$GITHUB_ENV"
  if $notarize; then
    printf '%s\n' "$NOTARY_API_KEY_P8" > "$dir/AuthKey.p8"
    chmod 600 "$dir/AuthKey.p8"
    echo "NOTARY_KEY_PATH=$dir/AuthKey.p8" >> "$GITHUB_ENV"
    echo "signing identity ready (Developer ID: notarized)"
  else
    echo "signing identity ready (self-signed: not notarized)"
  fi
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
