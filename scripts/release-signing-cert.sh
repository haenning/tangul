#!/usr/bin/env bash
# Creates the self-signed certificate releases are signed with until there is an
# Apple Developer ID (#192), as a .p12 file for the `release` environment's secrets.
# Not added to any keychain.
#
#   scripts/release-signing-cert.sh tangul-release.p12
#
# Valid for 20 years. Every release must be signed with this same certificate: macOS
# ties the Keychain's "Always Allow" to it, so a new one asks every user again. Keep
# the .p12 and its password in a password manager, never in the repository.
set -euo pipefail

out="${1:?usage: $0 <file.p12>}"
name="tangul release signing"
[[ -e "$out" ]] && { echo "$out exists already" >&2; exit 1; }

read -rsp "Password for the .p12: " password && echo
[[ -n "$password" ]] || { echo "a password is needed" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cat >"$tmp/cert.cnf" <<CNF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CNF
# macOS's own openssl (LibreSSL) writes a .p12 that `security import` reads.
openssl=/usr/bin/openssl
[[ -x "$openssl" ]] || openssl=openssl
"$openssl" req -x509 -newkey rsa:3072 -nodes -days 7300 \
  -config "$tmp/cert.cnf" -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null
"$openssl" pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
  -name "$name" -passout "pass:$password" -out "$out"
chmod 600 "$out"

echo "Created $out. Secrets for the 'release' environment:"
echo "  MACOS_CERTIFICATE_P12       base64 -i $out | pbcopy"
echo "  MACOS_CERTIFICATE_PASSWORD  the password you just typed"
echo "  MACOS_SIGN_IDENTITY         $name"
