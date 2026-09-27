#!/usr/bin/env bash
# Installs (or removes) gits-file-visualizer for the current user only:
#   ~/.local/bin/gits-collector, ~/.local/bin/gits-render
#   ~/.config/systemd/user/gits-collector.service (enabled and started)
# Nothing is written outside your home directory and no sudo is needed.
#
#   tools/install-user.sh            build (release), install, enable, start
#   tools/install-user.sh uninstall  stop, disable, remove the files above
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT=gits-collector.service

if [[ "${1:-}" == "uninstall" ]]; then
  systemctl --user disable --now "$UNIT" 2>/dev/null || true
  rm -f "$UNIT_DIR/$UNIT" "$BIN/gits-collector" "$BIN/gits-render"
  systemctl --user daemon-reload
  echo "removed gits-file-visualizer (your config in ~/.config/gits-file-visualizer is kept)"
  exit 0
fi

cargo build --release --locked -p gits-collector -p gits-render
mkdir -p "$BIN" "$UNIT_DIR"
install -m 755 target/release/gits-collector target/release/gits-render "$BIN/"
install -m 644 "packaging/systemd/$UNIT" "$UNIT_DIR/$UNIT"
systemctl --user daemon-reload
systemctl --user enable --now "$UNIT"
systemctl --user restart "$UNIT"
echo
systemctl --user --no-pager status "$UNIT" | head -5
echo
echo "installed. Logs: journalctl --user -u $UNIT -f"
echo "run the renderer: gits-render window   (uses the collector when it is running)"
