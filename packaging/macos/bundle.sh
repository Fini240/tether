#!/usr/bin/env bash
#
# Build a universal macOS binary, wrap it in Tether.app, and produce a .dmg
# plus a plain .tar.gz.
#
# Two artifacts because they serve different users: the .app is what you drag
# to /Applications and grant Accessibility to, the tarball is what you drop in
# /usr/local/bin and run from a terminal or a LaunchAgent.
#
# Signing: ad-hoc (`codesign -s -`) unless MACOS_SIGN_IDENTITY is set. That is
# enough to give the binary a *stable* code signature, which matters because
# macOS keys the Accessibility grant to it — an unsigned binary has to be
# re-approved constantly. It is NOT enough to satisfy Gatekeeper on a
# downloaded file; see the quarantine note in the README.
#
# Usage:  packaging/macos/bundle.sh [version]

set -euo pipefail

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/Tether.app"

cd "$ROOT"

echo "==> Building tether $VERSION for both Apple architectures"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	rustup target add "$target" >/dev/null 2>&1 || true
	cargo build --release --bin tether --target "$target"
done

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

echo "==> Fusing a universal binary"
lipo -create \
	"target/aarch64-apple-darwin/release/tether" \
	"target/x86_64-apple-darwin/release/tether" \
	-output "$APP/Contents/MacOS/tether"
lipo -info "$APP/Contents/MacOS/tether"

sed "s/__VERSION__/$VERSION/g" packaging/macos/Info.plist >"$APP/Contents/Info.plist"

echo "==> Signing"
if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
	# A real Developer ID. Hardened runtime is required for notarisation, and
	# notarisation is what stops Gatekeeper quarantining the download.
	codesign --force --options runtime --timestamp \
		--sign "$MACOS_SIGN_IDENTITY" "$APP"
	echo "    signed with $MACOS_SIGN_IDENTITY"
else
	codesign --force --sign - "$APP"
	echo "    ad-hoc signed (no MACOS_SIGN_IDENTITY set)"
fi
codesign --verify --verbose=2 "$APP"

echo "==> Packaging"
tar -czf "$DIST/tether-$VERSION-macos-universal.tar.gz" \
	-C "$APP/Contents/MacOS" tether

STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "Tether $VERSION" -srcfolder "$STAGE" \
	-ov -format UDZO "$DIST/tether-$VERSION-macos-universal.dmg"
rm -rf "$STAGE"

echo "==> Checksums"
cd "$DIST"
shasum -a 256 ./*.tar.gz ./*.dmg >SHA256SUMS
cat SHA256SUMS

echo
echo "Artifacts in $DIST"
