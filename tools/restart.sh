#!/usr/bin/env bash
# Restarts the wallpaper from a clean state (installed as `gits-restart`).
#
#   gits-restart                 full reset: put the saved monitor layout back,
#                                turn the GNOME extension off and on (rebuilds
#                                its copies of the wallpaper on every monitor
#                                and starts a fresh renderer), then check that
#                                every monitor picked it up
#   gits-restart --renderer      only restart the renderer (the extension
#                                respawns it; counts toward its 5 per 5 minutes)
#   gits-restart --all           full reset, and the file-watching collector too
#                                (rescans your files: a few seconds)
#
# Started by the dock icon, the launcher entry and Super+Shift+W
# (tools/install-shortcut.sh).
set -u

UUID=gits-file-visualizer@iliabaranov.github.io
mode=full
case "${1:-}" in
  --renderer) mode=renderer ;;
  --all) mode=all ;;
  ""|--full) ;;
  *) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 2 ;;
esac

say() { echo "$*"; }
# One notification that updates in place (when started from the dock or a shortcut).
notify() {
  command -v notify-send >/dev/null || return 0
  notify-send -a gits-file-visualizer -i gits-restart -h string:x-canonical-private-synchronous:gits-restart "Wallpaper" "$1" 2>/dev/null || true
}
[[ -t 1 ]] || notify "Restarting..."

# Monitors the renderer was started for, to know how many windows to wait for.
want=$(pgrep -a gits-render | head -1 | grep -o -- '--output' | wc -l)
(( want > 0 )) || want=1
since=$(date '+%Y-%m-%d %H:%M:%S')

adopted() {
  journalctl --user -o cat /usr/bin/gnome-shell --since "$since" 2>/dev/null | grep -c 'renderer window for monitor .* adopted'
}

wait_for_renderer() {  # $1 = seconds
  local i
  for ((i = 0; i < $1 * 2; i++)); do
    if (( $(adopted) >= want )); then return 0; fi
    sleep 0.5
  done
  return 1
}

# 1. The monitor layout (GNOME sometimes comes up with the wrong one).
if [[ $mode != renderer ]]; then
  layout=$(sed -n 's/^Exec=//p' "$HOME/.config/autostart/gits-display-layout.desktop" 2>/dev/null | sed 's/ watch$/ apply/')
  if [[ -n $layout ]]; then
    say "checking the monitor layout"
    $layout >/dev/null 2>&1 || true
  fi
fi

# 2. The collector, only when asked (or when it is not running).
if [[ $mode == all ]] || ! systemctl --user -q is-active gits-collector.service; then
  say "restarting the collector"
  systemctl --user restart gits-collector.service
fi

# 3. The extension and renderer.
if [[ $mode == renderer ]]; then
  say "restarting the renderer"
  pkill -x gits-render || true
else
  say "resetting the wallpaper extension"
  gnome-extensions disable "$UUID" 2>/dev/null
  sleep 1.5
  pkill -x gits-render 2>/dev/null || true   # a leftover renderer, if disabling did not stop it
  gnome-extensions enable "$UUID" 2>/dev/null
fi

# 4. Check, and fall back to killing the renderer once.
if wait_for_renderer 20; then
  say "done: $(adopted) monitor(s) running"
  notify "Restarted: $want monitor(s) running"
  exit 0
fi
say "the extension did not start it; killing the renderer once more"
pkill -x gits-render 2>/dev/null || true
if wait_for_renderer 15; then
  say "done"
  notify "Restarted"
  exit 0
fi
say "still not running. Look at: journalctl --user -f -o cat /usr/bin/gnome-shell | grep gits" >&2
notify "Restart failed: see the shell log (journalctl --user -o cat /usr/bin/gnome-shell | grep gits)"
exit 1
