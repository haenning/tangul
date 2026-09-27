#!/usr/bin/env bash
# Creates a self-signed code-signing certificate in your login keychain, for signing
# local builds (`make install`) with a stable identity. macOS ties a Keychain item's
# "Always Allow" to the app's signature: an ad-hoc signature changes with every
# build, so every install asks for the database passwords again. A certificate stays
# the same, so the answer sticks.
#
#   scripts/local-signing-cert.sh ["certificate name"]
#
# Once per Mac. macOS asks for your login password to trust the certificate for
# code signing. Only for your own builds: others' Macs don't trust it.
set -euo pipefail

name="${1:-tangul local signing}"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -qF "\"$name\""; then
  echo "The certificate \"$name\" already exists."
else
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  cat >"$tmp/cert.cnf" <<EOF
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
EOF
  # macOS's own openssl (LibreSSL) writes a .p12 that `security import` reads.
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -config "$tmp/cert.cnf" -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null
  /usr/bin/openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
    -name "$name" -passout pass:tangul -out "$tmp/cert.p12"
  security import "$tmp/cert.p12" -k "$keychain" -P tangul -T /usr/bin/codesign
  security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$tmp/cert.pem"
  echo "Created the certificate \"$name\"."
fi

echo
echo "Put this line into .env (next to the Makefile), without quotes: make reads"
echo "the file as it is."
echo "TANGUL_SIGN_IDENTITY=$name"
