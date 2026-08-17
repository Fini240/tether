#!/usr/bin/env sh
#
# Download and install the tether binary from the latest GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/Fini240/tether/main/install.sh | sh
#
# While the repository is PRIVATE that one-liner cannot work — GitHub requires
# authentication to read a private repo's release assets, and an unauthenticated
# curl gets a 404 rather than a useful error. Until it goes public, install with
# the GitHub CLI, which this script uses automatically when it is available:
#
#   gh auth login
#   sh install.sh
#
# Environment:
#   TETHER_VERSION   tag to install (default: the latest release)
#   TETHER_BIN_DIR   install directory (default: /usr/local/bin)

set -eu

REPO="Fini240/tether"
BIN_DIR="${TETHER_BIN_DIR:-/usr/local/bin}"
VERSION="${TETHER_VERSION:-}"

die() {
	echo "install.sh: $*" >&2
	exit 1
}

need() {
	command -v "$1" >/dev/null 2>&1
}

# ---- work out which build we need -------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
Darwin) platform="macos-universal" ;;
Linux)
	case "$arch" in
	x86_64 | amd64) platform="linux-x86_64" ;;
	*) die "no prebuilt Linux binary for $arch — build from source:
      cargo build --release --bin tether" ;;
	esac
	echo "note: the Linux input backend is not implemented yet. This binary"
	echo "      runs, but only with --backend headless. See the README."
	;;
*)
	die "unsupported system: $os. On Windows, download the .zip from
      https://github.com/$REPO/releases and put tether.exe on your PATH."
	;;
esac

# ---- fetch ------------------------------------------------------------------

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if need gh; then
	# Works for private repositories, using the caller's existing gh auth.
	gh auth status >/dev/null 2>&1 || die "run 'gh auth login' first"

	if [ -z "$VERSION" ]; then
		VERSION="$(gh release view --repo "$REPO" --json tagName -q .tagName)" ||
			die "no releases found in $REPO"
	fi
	echo "==> Downloading $VERSION ($platform) with gh"
	gh release download "$VERSION" --repo "$REPO" \
		--pattern "*$platform*.tar.gz" --pattern "SHA256SUMS" \
		--dir "$tmp" --clobber ||
		die "download failed — is there an asset matching *$platform*.tar.gz?"
else
	need curl || die "need either the 'gh' CLI or curl"

	if [ -z "$VERSION" ]; then
		VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
			sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
		[ -n "$VERSION" ] || die "could not find the latest release.
      If the repository is still private, install the GitHub CLI and run
      'gh auth login' — see the comment at the top of this script."
	fi
	base="https://github.com/$REPO/releases/download/$VERSION"
	echo "==> Downloading $VERSION ($platform)"
	curl -fsSL "$base/tether-${VERSION#v}-$platform.tar.gz" \
		-o "$tmp/tether.tar.gz" || die "download failed"
	curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || true
fi

# ---- verify -----------------------------------------------------------------

archive="$(find "$tmp" -name '*.tar.gz' | head -1)"
[ -n "$archive" ] || die "no archive was downloaded"

if [ -f "$tmp/SHA256SUMS" ]; then
	if need shasum; then
		actual="$(shasum -a 256 "$archive" | cut -d' ' -f1)"
	elif need sha256sum; then
		actual="$(sha256sum "$archive" | cut -d' ' -f1)"
	else
		actual=""
	fi

	if [ -n "$actual" ]; then
		# Only the hash column is compared: the paths in SHA256SUMS are
		# whatever the build machine used and will not match a temp dir.
		if grep -q "$actual" "$tmp/SHA256SUMS"; then
			echo "==> Checksum verified"
		else
			die "CHECKSUM MISMATCH — refusing to install.
      Got $actual, which is not listed in SHA256SUMS."
		fi
	fi
else
	echo "warning: no SHA256SUMS published; skipping verification" >&2
fi

# ---- install ----------------------------------------------------------------

tar -xzf "$archive" -C "$tmp"
binary="$(find "$tmp" -type f -name tether -perm -u+x | head -1)"
[ -n "$binary" ] || die "the archive contained no 'tether' binary"

if [ -w "$BIN_DIR" ]; then
	install -m 0755 "$binary" "$BIN_DIR/tether"
else
	echo "==> $BIN_DIR needs elevation"
	sudo install -m 0755 "$binary" "$BIN_DIR/tether"
fi

# A downloaded file carries com.apple.quarantine. Left in place, Gatekeeper
# refuses to run it and the message blames the developer rather than the
# quarantine flag, which sends people hunting in the wrong place.
if [ "$os" = "Darwin" ]; then
	xattr -d com.apple.quarantine "$BIN_DIR/tether" 2>/dev/null || true
fi

echo
echo "Installed $("$BIN_DIR/tether" --version 2>/dev/null || echo tether) to $BIN_DIR/tether"

if [ "$os" = "Darwin" ]; then
	cat <<'EOF'

Before it can capture or inject input, grant it Accessibility:
  System Settings -> Privacy & Security -> Accessibility -> +
  and choose /usr/local/bin/tether

Then, on the machine with the keyboard and mouse:   tether host --pair
and on every other machine:                          tether client --pair
Compare the fingerprints, then restart both without --pair.
EOF
fi
