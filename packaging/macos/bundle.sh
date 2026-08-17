#!/usr/bin/env bash
#
# Build the macOS artifacts:
#
#   Tether.app  — menu bar front end with the daemon bundled inside it
#   .dmg        — drag-to-Applications installer wrapping that app
#   .tar.gz     — the bare signed CLI, for /usr/local/bin and LaunchAgents
#
# Both halves are universal (arm64 + x86_64).
#
# Signing notes, because both of these have bitten this project already:
#
#  * The CLI in the tarball is signed as a *standalone binary*. Signing it as
#    part of the bundle seals Info.plist into the signature, so on its own it
#    then fails `codesign --verify` — and macOS keys the Accessibility grant to
#    the code signature, making that a permissions failure, not a cosmetic one.
#  * The .app is signed after its contents, inside out. A bundle signature
#    covers everything beneath it, so signing the outer bundle first and then
#    touching an embedded binary invalidates it.
#
# Ad-hoc (`codesign -s -`) unless MACOS_SIGN_IDENTITY is set. Ad-hoc gives a
# stable signature, which is enough for the Accessibility grant to stick. It is
# NOT enough for Gatekeeper on a download — that needs a Developer ID plus
# notarisation.
#
# Usage:  packaging/macos/bundle.sh [version]

set -euo pipefail

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/packaging/macos"
DIST="$ROOT/dist"
APP="$DIST/Tether.app"

cd "$ROOT"

sign() {
	if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
		codesign --force --options runtime --timestamp \
			--sign "$MACOS_SIGN_IDENTITY" "$@"
	else
		codesign --force --sign - "$@"
	fi
}

echo "==> Building the daemon for both Apple architectures"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	rustup target add "$target" >/dev/null 2>&1 || true
	cargo build --release --bin tether --target "$target"
done

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

echo "==> Fusing the universal daemon"
lipo -create \
	"target/aarch64-apple-darwin/release/tether" \
	"target/x86_64-apple-darwin/release/tether" \
	-output "$DIST/tether"
lipo -info "$DIST/tether"

echo "==> Building the menu bar launcher"
# -runtime-compatibility-version none: the Command Line Tools ship the Swift
# back-deployment shims for arm64 only, so an x86_64 link fails looking for
# them. The launcher uses no concurrency features that need back-deploying —
# it is Dispatch and Timer — so dropping the shims is what makes a universal
# build possible without a full Xcode install.
for arch in arm64 x86_64; do
	swiftc -O \
		-target "${arch}-apple-macos11.0" \
		-runtime-compatibility-version none \
		-o "$DIST/Tether-$arch" \
		"$HERE/Launcher/main.swift" 2>&1 | grep -vE "^ld: warning|Could not parse" || true
	[[ -f "$DIST/Tether-$arch" ]] || { echo "launcher build failed for $arch" >&2; exit 1; }
done
lipo -create "$DIST/Tether-arm64" "$DIST/Tether-x86_64" -output "$APP/Contents/MacOS/Tether"
rm -f "$DIST/Tether-arm64" "$DIST/Tether-x86_64"

echo "==> Rendering the icon"
python3 "$HERE/make_icon.py" "$APP/Contents/Resources/Tether.icns" >/dev/null

echo "==> Assembling the bundle"
cp "$DIST/tether" "$APP/Contents/Resources/tether"
chmod +x "$APP/Contents/Resources/tether"
sed "s/__VERSION__/$VERSION/g" "$HERE/Info.plist" >"$APP/Contents/Info.plist"

echo "==> Signing"
# Inside out: embedded executables first, then the bundle that contains them.
sign "$APP/Contents/Resources/tether"
sign "$APP/Contents/MacOS/Tether"
sign "$APP"
codesign --verify --deep --verbose=2 "$APP"

# The standalone CLI is signed separately, as a lone executable.
sign "$DIST/tether"
codesign --verify --verbose=2 "$DIST/tether"

echo "==> Packaging"
tar -czf "$DIST/tether-$VERSION-macos-universal.tar.gz" -C "$DIST" tether
rm "$DIST/tether"

STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "Tether $VERSION" -srcfolder "$STAGE" \
	-ov -format UDZO "$DIST/Tether-$VERSION.dmg"
rm -rf "$STAGE"

echo "==> Checksums"
cd "$DIST"
shasum -a 256 ./*.tar.gz ./*.dmg >SHA256SUMS
cat SHA256SUMS

echo
echo "Artifacts in $DIST"
