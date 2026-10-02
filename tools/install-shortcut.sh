#!/usr/bin/env bash
# Installs the restart tool as `gits-restart`, with an app-launcher entry and a
# keyboard shortcut (default Super+Shift+W).
#
#   tools/install-shortcut.sh                    install
#   tools/install-shortcut.sh '<Super><Alt>r'    install with another key combination
#   tools/install-shortcut.sh uninstall          remove all three
set -euo pipefail
cd "$(dirname "$0")"
[[ -f restart.sh ]] || cd tools   # in a bundle this script sits next to bin/

BIN="$HOME/.local/bin/gits-restart"
DESKTOP="$HOME/.local/share/applications/gits-restart.desktop"
KEY=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/gits-restart/
SCHEMA=org.gnome.settings-daemon.plugins.media-keys
LIST_KEY=custom-keybindings

list() { gsettings get "$SCHEMA" "$LIST_KEY"; }

remove_binding() {
  local cur new
  cur=$(list)
  [[ "$cur" == *"'$KEY'"* ]] || return 0
  new=$(python3 - "$cur" "$KEY" <<'EOF'
import ast, sys
cur = sys.argv[1].replace("@as ", "")
items = [x for x in ast.literal_eval(cur) if x != sys.argv[2]]
print(repr(items) if items else "@as []")
EOF
)
  gsettings set "$SCHEMA" "$LIST_KEY" "$new"
  gsettings reset-recursively "$SCHEMA.custom-keybinding:$KEY" 2>/dev/null || true
}

if [[ "${1:-}" == "uninstall" ]]; then
  remove_binding
  rm -f "$BIN" "$DESKTOP"
  echo "removed gits-restart, its launcher entry and its shortcut"
  exit 0
fi

BINDING="${1:-<Super><Shift>w}"
mkdir -p "$(dirname "$BIN")" "$(dirname "$DESKTOP")"
install -m 755 restart.sh "$BIN"
cat > "$DESKTOP" <<EOF2
[Desktop Entry]
Type=Application
Name=Restart wallpaper
Comment=Restart the gits-file-visualizer wallpaper from a clean state
Exec=$BIN
Icon=view-refresh
Terminal=false
Categories=Utility;
EOF2

remove_binding
cur=$(list)
new=$(python3 - "$cur" "$KEY" <<'EOF'
import ast, sys
cur = sys.argv[1].replace("@as ", "")
items = ast.literal_eval(cur)
items.append(sys.argv[2])
print(repr(items))
EOF
)
gsettings set "$SCHEMA" "$LIST_KEY" "$new"
P="$SCHEMA.custom-keybinding:$KEY"
gsettings set "$P" name 'Restart wallpaper'
gsettings set "$P" command "$BIN"
gsettings set "$P" binding "$BINDING"

echo "installed:"
echo "  command:   gits-restart   (--renderer for a quick one, --all to restart the collector too)"
echo "  launcher:  'Restart wallpaper' in the Activities search"
echo "  shortcut:  $BINDING"
