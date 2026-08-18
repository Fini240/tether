#!/usr/bin/env sh
#
# Install Tether as a proper desktop application.
#
#   sh install.sh              # binaries, icon, launcher entry, udev rule
#   sh install.sh --autostart  # …and start it at login
#   sh install.sh --uninstall  # take it all back out
#
# System paths need root, so this asks for sudo where it needs to and nowhere
# else. The launcher entry and the autostart entry are per-user and do not.

set -eu

PREFIX="${PREFIX:-/usr/local}"
APP_ID=dev.tether.Tether
HERE="$(cd "$(dirname "$0")" && pwd)"

DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"

say() { printf '==> %s\n' "$*"; }

uninstall() {
	say "Removing Tether"
	sudo rm -f "$PREFIX/bin/tether" "$PREFIX/bin/tether-gui"
	rm -f "$DESKTOP_DIR/$APP_ID.desktop" "$AUTOSTART_DIR/$APP_ID.desktop"
	rm -f "$ICON_DIR/$APP_ID.png"
	command -v update-desktop-database >/dev/null 2>&1 &&
		update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
	echo
	echo "Left in place, because removing them may break other things:"
	echo "  /etc/udev/rules.d/99-tether.rules"
	echo "  your membership of the 'input' group"
	echo "  ~/.config/tether  (config, identity and paired machines)"
	exit 0
}

autostart=no
for arg in "$@"; do
	case "$arg" in
	--autostart) autostart=yes ;;
	--uninstall) uninstall ;;
	*)
		echo "unknown option: $arg" >&2
		echo "usage: install.sh [--autostart] [--uninstall]" >&2
		exit 2
		;;
	esac
done

# ---- binaries ---------------------------------------------------------------

[ -f "$HERE/tether" ] || { echo "no 'tether' binary next to this script" >&2; exit 1; }

say "Installing to $PREFIX/bin"
sudo install -d "$PREFIX/bin"
sudo install -m 0755 "$HERE/tether" "$PREFIX/bin/tether"
if [ -f "$HERE/tether-gui" ]; then
	sudo install -m 0755 "$HERE/tether-gui" "$PREFIX/bin/tether-gui"
else
	echo "    (no tether-gui in this archive; installing the CLI only)"
fi

# ---- desktop integration ----------------------------------------------------

if [ -f "$HERE/tether-gui" ]; then
	say "Adding the launcher entry"
	mkdir -p "$DESKTOP_DIR" "$ICON_DIR"
	install -m 0644 "$HERE/$APP_ID.desktop" "$DESKTOP_DIR/$APP_ID.desktop"
	install -m 0644 "$HERE/$APP_ID.png" "$ICON_DIR/$APP_ID.png"

	# Both are best-effort: the entry works without either, they just make it
	# appear without a re-login.
	command -v update-desktop-database >/dev/null 2>&1 &&
		update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
	command -v gtk-update-icon-cache >/dev/null 2>&1 &&
		gtk-update-icon-cache -qtf "${ICON_DIR%/*/*/*}" 2>/dev/null || true

	if [ "$autostart" = yes ]; then
		say "Starting at login"
		mkdir -p "$AUTOSTART_DIR"
		install -m 0644 "$HERE/$APP_ID.desktop" "$AUTOSTART_DIR/$APP_ID.desktop"
	fi
fi

# ---- permissions ------------------------------------------------------------

needs_setup=no

if [ ! -e /dev/uinput ]; then
	say "Loading the uinput module"
	sudo modprobe uinput || true
	echo uinput | sudo tee /etc/modules-load.d/uinput.conf >/dev/null
fi

if [ ! -f /etc/udev/rules.d/99-tether.rules ] && [ -f "$HERE/99-tether.rules" ]; then
	say "Installing the udev rule"
	sudo install -m 0644 "$HERE/99-tether.rules" /etc/udev/rules.d/99-tether.rules
	sudo udevadm control --reload-rules
	sudo udevadm trigger
	needs_setup=yes
fi

if ! id -nG | tr ' ' '\n' | grep -qx input; then
	say "Adding $USER to the 'input' group"
	sudo usermod -aG input "$USER"
	needs_setup=yes
fi

# ---- what to do next --------------------------------------------------------

echo
if [ "$needs_setup" = yes ]; then
	cat <<'NOTE'
  LOG OUT AND BACK IN before using it.

  Group membership only applies to new sessions — a screen lock is not
  enough. This is the step people skip before reporting that it does not
  work. Check it took with:  groups | grep input

NOTE
fi

echo "  Then:"
echo "    tether doctor          check this machine is ready"
echo "    tether run --pair      pair with your other machines, once"
echo
echo "  Or open Tether from your application launcher."
