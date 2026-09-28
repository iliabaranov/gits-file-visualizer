#!/usr/bin/env bash
# Installs (or removes) gits-file-visualizer for the current user only:
#   ~/.local/bin/gits-collector, ~/.local/bin/gits-render
#   ~/.config/systemd/user/gits-collector.service (enabled and started)
#   ~/.local/share/gnome-shell/extensions/gits-file-visualizer@iliabaranov.github.io
# Nothing is written outside your home directory and no sudo is needed.
#
#   tools/install-user.sh              build (release), install, start the collector
#   tools/install-user.sh uninstall    stop, disable, remove everything above
#
# The wallpaper itself is the GNOME extension; the script enables it. On
# Wayland, GNOME only discovers a newly installed extension when you log in, so
# the first time it appears after the next login.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT=gits-collector.service
UUID=gits-file-visualizer@iliabaranov.github.io
EXT_DIR="$HOME/.local/share/gnome-shell/extensions/$UUID"

shell_major() { gnome-shell --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\1/'; }

if [[ "${1:-}" == "uninstall" ]]; then
  gnome-extensions disable "$UUID" 2>/dev/null || true
  systemctl --user disable --now "$UNIT" 2>/dev/null || true
  rm -rf "$EXT_DIR"
  rm -f "$UNIT_DIR/$UNIT" "$BIN/gits-collector" "$BIN/gits-render"
  systemctl --user daemon-reload
  echo "removed gits-file-visualizer (your config in ~/.config/gits-file-visualizer is kept)"
  exit 0
fi

major=$(shell_major)
if [[ -z "$major" ]]; then
  echo "note: gnome-shell not found; installing the collector and renderer only"
elif (( major >= 45 )); then
  echo "GNOME $major is not supported yet (the extension for GNOME 45+ is planned in milestone M5)" >&2
  exit 1
fi

# Lowest CPU priority, so the desktop stays responsive while it compiles.
nice -n 19 cargo build --release --locked -p gits-collector -p gits-render
mkdir -p "$BIN" "$UNIT_DIR"
install -m 755 target/release/gits-collector target/release/gits-render "$BIN/"
install -m 644 "packaging/systemd/$UNIT" "$UNIT_DIR/$UNIT"
systemctl --user daemon-reload
systemctl --user enable --now "$UNIT"
systemctl --user restart "$UNIT"

enable_extension() {
  # Adds the extension to org.gnome.shell enabled-extensions (keeping the others).
  # GNOME loads it at the next login even if the running shell has not seen it yet.
  local cur
  cur=$(gsettings get org.gnome.shell enabled-extensions)
  [[ "$cur" == *"'$UUID'"* ]] && return 0
  if [[ "$cur" == "@as []" || "$cur" == "[]" ]]; then
    gsettings set org.gnome.shell enabled-extensions "['$UUID']"
  else
    gsettings set org.gnome.shell enabled-extensions "${cur%]}, '$UUID']"
  fi
}

if [[ -n "$major" ]]; then
  mkdir -p "$EXT_DIR"
  install -m 644 extension/gnome-42/metadata.json extension/gnome-42/extension.js "$EXT_DIR/"
  enable_extension
fi

echo
systemctl --user --no-pager status "$UNIT" | head -3
echo
echo "installed. Collector logs: journalctl --user -u $UNIT -f"
if [[ -n "$major" ]]; then
  if gnome-extensions info "$UUID" >/dev/null 2>&1; then
    echo "the wallpaper extension is enabled. To pick up a new version: disable, then enable:"
    echo "  gnome-extensions disable $UUID; gnome-extensions enable $UUID"
  else
    echo "the wallpaper extension is enabled and starts at your next login"
    echo "(GNOME on Wayland only discovers new extensions when you log in)."
  fi
  echo "wallpaper off:  gnome-extensions disable $UUID"
  echo "extension/renderer logs: journalctl --user -f -o cat /usr/bin/gnome-shell | grep -E 'gits'"
fi
