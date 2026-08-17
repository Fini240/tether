#!/usr/bin/env bash
#
# Build a signed universal macOS binary and tar it up.
#
# Deliberately NOT an .app bundle or a .dmg. `tether` is a CLI that requires a
# subcommand — double-clicking an app wrapper runs it with no arguments, so it
# prints usage to a stderr nobody is watching and exits. With LSUIElement set
# there is not even a Dock icon to show it happened. A .dmg would look like a
# working installer and deliver nothing. When the tray UI lands it can ship a
# real bundle; until then the terminal is the honest interface.
#
# Signing: the standalone binary is signed *as a standalone binary*. Signing it
# as part of a bundle seals the Info.plist into the signature, so the binary on
# its own then fails `codesign --verify` — and macOS keys the Accessibility
# grant to the code signature, which makes that failure a permissions problem
# rather than a cosmetic one.
#
# Ad-hoc unless MACOS_SIGN_IDENTITY is set. Ad-hoc gives a stable signature, so
# the Accessibility grant sticks across restarts. It does NOT satisfy
# Gatekeeper on a download; that needs a Developer ID and notarisation.
#
# Usage:  packaging/macos/bundle.sh [version]

set -euo pipefail

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"

cd "$ROOT"

echo "==> Building tether $VERSION for both Apple architectures"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	rustup target add "$target" >/dev/null 2>&1 || true
	cargo build --release --bin tether --target "$target"
done

rm -rf "$DIST"
mkdir -p "$DIST"

echo "==> Fusing a universal binary"
lipo -create \
	"target/aarch64-apple-darwin/release/tether" \
	"target/x86_64-apple-darwin/release/tether" \
	-output "$DIST/tether"
lipo -info "$DIST/tether"

echo "==> Signing"
if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
	codesign --force --options runtime --timestamp \
		--sign "$MACOS_SIGN_IDENTITY" "$DIST/tether"
	echo "    signed with $MACOS_SIGN_IDENTITY"
else
	codesign --force --sign - "$DIST/tether"
	echo "    ad-hoc signed (no MACOS_SIGN_IDENTITY set)"
fi

# Verify the way a user's Mac will: as a lone executable, not inside a bundle.
codesign --verify --verbose=2 "$DIST/tether"

echo "==> Packaging"
tar -czf "$DIST/tether-$VERSION-macos-universal.tar.gz" -C "$DIST" tether
rm "$DIST/tether"

echo "==> Checksums"
cd "$DIST"
shasum -a 256 ./*.tar.gz >SHA256SUMS
cat SHA256SUMS

echo
echo "Artifacts in $DIST"
