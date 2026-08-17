#!/usr/bin/env bash
#
# Build the macOS artifacts:
#
#   Tether.app  — the window app, with the session running inside it
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

echo "==> Building for both Apple architectures"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	rustup target add "$target" >/dev/null 2>&1 || true
	cargo build --release --bin tether --bin tether-gui --target "$target"
done

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

echo "==> Fusing the universal daemon"
lipo -create \
	"target/aarch64-apple-darwin/release/tether" \
	"target/x86_64-apple-darwin/release/tether" \
	-output "$DIST/tether"
lipo -info "$DIST/tether"

echo "==> Fusing the universal app"
lipo -create \
	"target/aarch64-apple-darwin/release/tether-gui" \
	"target/x86_64-apple-darwin/release/tether-gui" \
	-output "$APP/Contents/MacOS/Tether"

echo "==> Icon"
# The .icns is committed, not generated at build time. It is a design asset —
# it should change when someone decides it should, not silently whenever a
# build machine has a different Pillow. Regenerate it deliberately with
# make_icon.py, which needs Pillow that CI does not have.
if [[ -f "$HERE/Tether.icns" ]]; then
	cp "$HERE/Tether.icns" "$APP/Contents/Resources/Tether.icns"
	echo "    using the committed packaging/macos/Tether.icns"
elif python3 -c "import PIL" 2>/dev/null; then
	python3 "$HERE/make_icon.py" "$APP/Contents/Resources/Tether.icns" >/dev/null
	echo "    rendered from make_icon.py"
else
	echo "    no icon available (no Tether.icns, no Pillow) — bundling without one" >&2
fi

echo "==> Assembling the bundle"
# No CLI inside the bundle: the window runs the session itself, in-process.
# One binary also means one Accessibility grant rather than two.
sed "s/__VERSION__/$VERSION/g" "$HERE/Info.plist" >"$APP/Contents/Info.plist"

echo "==> Signing"
# Inside out: the executable first, then the bundle containing it.
sign "$APP/Contents/MacOS/Tether"
sign "$APP"
codesign --verify --deep --verbose=2 "$APP"

# The standalone CLI is signed separately, as a lone executable.
sign "$DIST/tether"
codesign --verify --verbose=2 "$DIST/tether"

echo "==> Packaging"
# The archive extracts to a *folder*, not a lone executable. A bare `tether`
# sitting in Downloads looks like something you double-click; it is a CLI that
# needs a subcommand, so clicking it prints usage and quits — and on macOS the
# quarantine flag turns that into a malware warning instead. A folder with a
# README next to the binary makes what it is obvious before that happens.
CLI_DIR="$DIST/tether-$VERSION-macos-universal"
mkdir -p "$CLI_DIR"
mv "$DIST/tether" "$CLI_DIR/tether"
cp "$ROOT/packaging/README-CLI.txt" "$CLI_DIR/README.txt"
tar -czf "$DIST/tether-$VERSION-macos-universal.tar.gz" -C "$DIST" \
	"tether-$VERSION-macos-universal"
rm -rf "$CLI_DIR"

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
