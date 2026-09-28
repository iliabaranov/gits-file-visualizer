#!/usr/bin/env bash
# Headless, fully isolated GNOME Shell for testing the extension and renderer
# without touching the real desktop session.
#
#   tools/sandbox-shell.sh start [WxH ...]   start (default: 1920x1200 3840x2160 1440x2560)
#   tools/sandbox-shell.sh stop
#   tools/sandbox-shell.sh eval 'JS'         run JS in the shell (prints result)
#   tools/sandbox-shell.sh shot FILE.png     screenshot of all monitors
#   tools/sandbox-shell.sh log               shell + renderer log
#   tools/sandbox-shell.sh run CMD...        run a client inside the sandbox (clean env)
#   tools/sandbox-shell.sh env               print exports to talk to the sandbox bus
#   GITS_SANDBOX_X11=1 tools/sandbox-shell.sh start   ... with Xwayland (X11 clients)
#
# Isolation: own HOME, XDG_RUNTIME_DIR, session D-Bus and dconf database, no
# Xwayland, no connection to the real display. The extension under test is
# linked from extension/<version dir>; the renderer is linked into the sandbox
# ~/.local/bin (as a real install would put it), or taken from GITS_RENDER_BIN.
# GITS_COLLECTOR_SOCKET, if set, is linked in as the sandbox's collector socket.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
SB=${GITS_SANDBOX:-/tmp/opencode/gits-sandbox}
UUID=gits-file-visualizer@iliabaranov.github.io
HELPER=gits-sandbox-helper@test

bus() { cat "$SB/bus"; }
call() { DBUS_SESSION_BUS_ADDRESS=$(bus) gdbus call --session "$@"; }

shell_major() { gnome-shell --version | sed -E 's/[^0-9]*([0-9]+).*/\1/'; }
ext_dir() { if [[ $(shell_major) -ge 45 ]]; then echo extension/gnome-45; else echo extension/gnome-42; fi; }

case "${1:-}" in
start)
  shift
  mons=("$@"); [[ ${#mons[@]} -eq 0 ]] && mons=(1920x1200 3840x2160 1440x2560)
  "$0" stop >/dev/null 2>&1 || true
  rm -rf "$SB"; mkdir -p "$SB/home/.local/share/gnome-shell/extensions" "$SB/home/Desktop" "$SB/run"; chmod 700 "$SB/run"
  touch "$SB/home/Desktop/notes.txt"   # so desktop icons (DING) create their windows, as in a real session
  if [[ -n "${GITS_EXT_DIR:-}" ]]; then   # test an installed copy instead of the repo
    cp -r "$GITS_EXT_DIR" "$SB/home/.local/share/gnome-shell/extensions/$UUID"
  else
    ln -s "$REPO/$(ext_dir)" "$SB/home/.local/share/gnome-shell/extensions/$UUID"
  fi
  ln -s "$REPO/tools/sandbox/$HELPER" "$SB/home/.local/share/gnome-shell/extensions/$HELPER"
  # Like a real install: the extension finds the renderer in ~/.local/bin.
  mkdir -p "$SB/home/.local/bin"; ln -s "$REPO/target/release/gits-render" "$SB/home/.local/bin/gits-render"
  if [[ -n "${GITS_COLLECTOR_SOCKET:-}" ]]; then
    mkdir -p "$SB/run/gits-file-visualizer"; ln -s "$GITS_COLLECTOR_SOCKET" "$SB/run/gits-file-visualizer/collector.sock"
  fi
  args=(); for m in "${mons[@]}"; do args+=(--virtual-monitor "$m"); done
  x11flag=--no-x11; [[ -n "${GITS_SANDBOX_X11:-}" ]] && x11flag=
  env -i HOME="$SB/home" XDG_RUNTIME_DIR="$SB/run" PATH=/usr/bin:/bin USER="$USER" LANG="${LANG:-C.UTF-8}" \
      XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=ubuntu:GNOME \
      ${GITS_RENDER_BIN:+GITS_RENDER_BIN="$GITS_RENDER_BIN"} \
      RUST_LOG="${RUST_LOG:-info}" \
      SB="$SB" UUID="$UUID" HELPER="$HELPER" \
      setsid dbus-run-session -- bash -c '
        echo "$DBUS_SESSION_BUS_ADDRESS" > "$SB/bus"
        dconf write /org/gnome/shell/enabled-extensions "[\"$HELPER\", \"$UUID\"]"
        dconf write /org/gnome/shell/disable-user-extensions false
        dconf write /org/gnome/desktop/session/idle-delay "uint32 0"
        dconf write /org/gnome/desktop/screensaver/lock-enabled false
        dconf write /org/gnome/shell/welcome-dialog-last-shown-version "\"999\""
        exec gnome-shell --headless --wayland '"$x11flag"' --mode=ubuntu '"${args[*]}"' ' \
      >"$SB/shell.log" 2>&1 &
  echo $! > "$SB/pid"
  for _ in $(seq 1 100); do
    if [[ -s "$SB/bus" ]] && call --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval 1 2>/dev/null | grep -q "true"; then
      echo "sandbox shell up (${mons[*]}); log: $SB/shell.log"; exit 0
    fi
    sleep 0.2
  done
  echo "sandbox shell did not come up; see $SB/shell.log" >&2; tail -20 "$SB/shell.log" >&2; exit 1 ;;
stop)
  if [[ -f "$SB/pid" ]]; then kill -- -"$(cat "$SB/pid")" 2>/dev/null || kill "$(cat "$SB/pid")" 2>/dev/null || true; fi
  pkill -f "gnome-shell --headless --wayland" 2>/dev/null || true
  # Anything else started inside the sandbox (renderer, DING, ibus, portals...).
  for p in $(pgrep -u "$USER"); do
    grep -qs "XDG_RUNTIME_DIR=$SB/run" "/proc/$p/environ" && kill "$p" 2>/dev/null
  done
  rm -f "$SB/pid"; echo stopped ;;
eval)
  call --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval "$2" ;;
shot)
  out=$(realpath -m "$2")
  call --dest org.gnome.Shell.Screenshot --object-path /org/gnome/Shell/Screenshot \
       --method org.gnome.Shell.Screenshot.Screenshot false false "$out" >/dev/null && echo "$out" ;;
log)
  cat "$SB/shell.log" ;;
run)
  shift
  # Clean environment: no DISPLAY, so nothing can fall back to the real X session.
  exec env -i HOME="$SB/home" XDG_RUNTIME_DIR="$SB/run" WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland \
    DBUS_SESSION_BUS_ADDRESS="$(bus)" PATH=/usr/bin:/bin USER="$USER" LANG="${LANG:-C.UTF-8}" \
    RUST_LOG="${RUST_LOG:-warn}" "$@" ;;
env)
  echo "unset DISPLAY;"
  echo "export DBUS_SESSION_BUS_ADDRESS='$(bus)' XDG_RUNTIME_DIR='$SB/run' WAYLAND_DISPLAY=wayland-0" ;;
*)
  sed -n '2,15p' "$0"; exit 1 ;;
esac
