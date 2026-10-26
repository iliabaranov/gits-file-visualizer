#!/usr/bin/env bash
# Installs the restart tool as `gits-restart`, with an icon, an app-launcher
# entry, a pin in the dock (the taskbar on the left) and a keyboard shortcut
# (default Super+Shift+W).
#
#   tools/install-shortcut.sh                    install everything
#   tools/install-shortcut.sh '<Super><Alt>r'    install with another key combination
#   tools/install-shortcut.sh --no-dock          install without pinning it to the dock
#   tools/install-shortcut.sh uninstall          remove all of it
set -euo pipefail
cd "$(dirname "$0")"
[[ -f restart.sh ]] || cd tools   # in a bundle this script sits next to bin/

BIN="$HOME/.local/bin/gits-restart"
DESKTOP="$HOME/.local/share/applications/gits-restart.desktop"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/gits-restart.svg"
APP=gits-restart.desktop
KEY=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/gits-restart/
SCHEMA=org.gnome.settings-daemon.plugins.media-keys
LIST_KEY=custom-keybindings

# Prints the gsettings list `$1` with the item `$2` removed (and added again at
# the end when `$3` is "add").
edit_list() {
  python3 - "$1" "$2" "${3:-}" <<'PYEOF'
import ast, sys
cur, item, add = sys.argv[1].replace("@as ", ""), sys.argv[2], sys.argv[3] == "add"
items = [x for x in ast.literal_eval(cur) if x != item]
if add:
    items.append(item)
print(repr(items) if items else "@as []")
PYEOF
}

set_list() {  # schema key item [add]
  local cur new
  cur=$(gsettings get "$1" "$2") || return 0
  new=$(edit_list "$cur" "$3" "${4:-}")
  [[ "$new" == "$cur" ]] || gsettings set "$1" "$2" "$new"
}

remove_binding() {
  set_list "$SCHEMA" "$LIST_KEY" "$KEY"
  gsettings reset-recursively "$SCHEMA.custom-keybinding:$KEY" 2>/dev/null || true
}

PIN=1
BINDING='<Super><Shift>w'
for arg in "$@"; do
  case "$arg" in
    uninstall)
      remove_binding
      set_list org.gnome.shell favorite-apps "$APP"
      rm -f "$BIN" "$DESKTOP" "$ICON"
      echo "removed gits-restart, its icon, launcher entry, dock pin and shortcut"
      exit 0 ;;
    --no-dock) PIN=0 ;;
    *) BINDING="$arg" ;;
  esac
done

mkdir -p "$(dirname "$BIN")" "$(dirname "$DESKTOP")" "$(dirname "$ICON")"
install -m 755 restart.sh "$BIN"
install -m 644 ../packaging/icons/gits-restart.svg "$ICON"
gtk-update-icon-cache -q -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
cat > "$DESKTOP" <<EOF2
[Desktop Entry]
Type=Application
Name=Restart wallpaper
Comment=Restart the gits-file-visualizer wallpaper from a clean state
Exec=$BIN
Icon=gits-restart
Terminal=false
Categories=Utility;
StartupNotify=false
EOF2

remove_binding
set_list "$SCHEMA" "$LIST_KEY" "$KEY" add
P="$SCHEMA.custom-keybinding:$KEY"
gsettings set "$P" name 'Restart wallpaper'
gsettings set "$P" command "$BIN"
gsettings set "$P" binding "$BINDING"
if (( PIN )); then
  set_list org.gnome.shell favorite-apps "$APP" add
fi

echo "installed:"
echo "  command:   gits-restart   (--renderer for a quick one, --all to restart the collector too)"
echo "  launcher:  'Restart wallpaper' in the Activities search"
if (( PIN )); then
  echo "  dock:      pinned: click the icon to restart (drag to move it, right-click > Unpin to remove)"
fi
echo "  shortcut:  $BINDING"
