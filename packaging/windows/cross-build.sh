#!/usr/bin/env bash
#
# Build the Windows artifacts from a Mac.
#
# The release workflow builds `x86_64-pc-windows-msvc` on a Windows runner.
# This does the same job with `x86_64-pc-windows-gnu` and mingw-w64, for when
# the runner is not worth spending — a laptop and a LAN cable will do.
#
#   brew install mingw-w64
#   rustup target add x86_64-pc-windows-gnu
#   packaging/windows/cross-build.sh [version]
#
# Rust links the GNU target self-contained, so the .exe files need no mingw
# DLLs alongside them. They are unsigned, which on Windows means SmartScreen
# will warn the first time — the same as the ones the workflow produces.

set -euo pipefail

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
TARGET=x86_64-pc-windows-gnu
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"
NAME="tether-$VERSION-windows-x86_64"

cd "$ROOT"

command -v x86_64-w64-mingw32-gcc >/dev/null ||
	{ echo "no mingw-w64 — run: brew install mingw-w64" >&2; exit 1; }

echo "==> Building $TARGET"
cargo build --release --target "$TARGET" --bin tether --bin tether-gui

echo "==> Packaging"
rm -rf "${DIST:?}/$NAME" "$DIST/$NAME.zip"
mkdir -p "$DIST/$NAME"

# Distinct base names, not Tether.exe + tether.exe: Windows filenames are
# case-insensitive, so those are the SAME file and the second copy silently
# overwrites the first. That shipped a zip containing only the GUI wearing the
# CLI's name.
cp "target/$TARGET/release/tether-gui.exe" "$DIST/$NAME/Tether.exe"
cp "target/$TARGET/release/tether.exe" "$DIST/$NAME/tether-cli.exe"
cp "$ROOT/packaging/README-WINDOWS.txt" "$DIST/$NAME/README.txt"

(cd "$DIST" && zip -qr "$NAME.zip" "$NAME" && rm -rf "$NAME")

echo "==> Checksum"
(cd "$DIST" && shasum -a 256 "$NAME.zip")
echo
echo "Artifact: $DIST/$NAME.zip"
