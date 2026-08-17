#!/usr/bin/env bash
#
# Create a self-signed code-signing certificate so Tether keeps its
# Accessibility permission across updates.
#
# The problem this solves:
#
#   An ad-hoc signature (`codesign -s -`) has no certificate, so macOS builds
#   the app's designated requirement out of a hash of the binary itself:
#
#       designated => cdhash H"7262c7a1..."
#
#   Every rebuild changes that hash. macOS keys Accessibility grants to the
#   designated requirement, so after an update the old grant no longer matches.
#   The row stays visible in System Settings, still switched on, and macOS
#   denies anyway — which looks exactly like the toggle not working.
#
#   Signing with a certificate instead produces a requirement based on the
#   *certificate*:
#
#       designated => identifier "dev.tether.Tether" and certificate leaf = H"..."
#
#   That is stable across rebuilds, so the grant survives updates.
#
# What this does NOT solve: Gatekeeper. A self-signed certificate is not a
# Developer ID, so a downloaded build is still quarantined. That needs a paid
# Apple Developer account and notarisation.
#
# Run once. It will ask for your login password — that is macOS authorising a
# change to your keychain's trust settings, not this script asking for it.
#
#   packaging/macos/make-signing-cert.sh
#   MACOS_SIGN_IDENTITY="Tether Local Signing" packaging/macos/bundle.sh

set -euo pipefail

NAME="${1:-Tether Local Signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if security find-identity -v -p codesigning | grep -qF "$NAME"; then
	echo "A code-signing identity called \"$NAME\" already exists."
	echo "Build with:  MACOS_SIGN_IDENTITY=\"$NAME\" packaging/macos/bundle.sh"
	exit 0
fi

echo "==> Generating a self-signed code-signing certificate"
# codeSigning EKU is what makes the certificate usable by codesign at all;
# without it the identity is created but never offered.
openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
	-keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
	-subj "/CN=$NAME" \
	-addext "basicConstraints=critical,CA:false" \
	-addext "keyUsage=critical,digitalSignature" \
	-addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null

openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
	-out "$WORK/identity.p12" -passout pass: 2>/dev/null

echo "==> Importing it into your login keychain"
# -T /usr/bin/codesign pre-authorises codesign to use the key, so builds do
# not stop on a keychain prompt every time.
security import "$WORK/identity.p12" -k "$KEYCHAIN" -P "" \
	-T /usr/bin/codesign -T /usr/bin/security >/dev/null

echo "==> Marking it trusted for code signing (macOS will ask for your password)"
security add-trusted-cert -p codeSign -k "$KEYCHAIN" "$WORK/cert.pem"

# Stops the keychain prompting on every signature from here on.
security set-key-partition-list -S apple-tool:,apple:,codesign: \
	-k "" "$KEYCHAIN" >/dev/null 2>&1 || true

echo
if security find-identity -v -p codesigning | grep -qF "$NAME"; then
	echo "Done. \"$NAME\" is ready."
	echo
	echo "Build signed with it:"
	echo "    MACOS_SIGN_IDENTITY=\"$NAME\" packaging/macos/bundle.sh"
	echo
	echo "The first build after switching still needs the Accessibility grant"
	echo "re-added once — the requirement changes from a hash to this"
	echo "certificate. After that it survives every update."
else
	echo "The identity was not created. Check the output above." >&2
	exit 1
fi
