#!/usr/bin/env python3
"""Remembers your monitor layout and puts it back after login.

    tools/display-layout.py save        remember the current layout
    tools/display-layout.py show        print the saved and the current layout
    tools/display-layout.py apply       apply the saved layout now (if its monitors are connected)
    tools/display-layout.py watch       at login: apply, then keep checking for a while
    tools/display-layout.py install     run `watch` at every login (autostart entry)
    tools/display-layout.py uninstall

Why: GNOME matches its stored layouts by connector name, and the same monitors
can come up on different connectors (DP-5 on the Intel GPU, DP-9 on the NVIDIA
one), or one may come up late or without its identity (EDID) at first. GNOME
then falls back to a default layout: the portrait screen at a fallback
1024x768, monitors in the wrong places. This tool identifies monitors by vendor,
product and serial (falling back to the connector), sets exactly the saved
positions, rotation, scale, primary monitor and modes, and during `watch`
re-checks every few seconds, so monitors that are slow to come up are fixed too.

Uses GNOME's display configuration D-Bus API (org.gnome.Mutter.DisplayConfig)
through python3-gi, which every Ubuntu desktop has.
"""

import argparse
import json
import os
import sys
import time

from gi.repository import Gio, GLib

BUS_NAME = "org.gnome.Mutter.DisplayConfig"
PATH = "/org/gnome/Mutter/DisplayConfig"
SAVED = os.path.join(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")), "gits-file-visualizer", "display-layout.json")
AUTOSTART = os.path.join(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")), "autostart", "gits-display-layout.desktop")
# ApplyMonitorsConfig methods.
VERIFY, TEMPORARY, PERSISTENT = 0, 1, 2


def log(msg):
    print(f"[display-layout {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def call(method, args=None):
    bus = Gio.bus_get_sync(Gio.BusType.SESSION)
    return bus.call_sync(BUS_NAME, PATH, BUS_NAME, method, args, None, Gio.DBusCallFlags.NONE, 10000, None).unpack()


def current():
    """(serial, monitors, logical monitors, properties) of the live state."""
    serial, monitors, logical, props = call("GetCurrentState")
    mons = []
    for (connector, vendor, product, mserial), modes, mprops in monitors:
        mons.append({
            "connector": connector, "vendor": vendor, "product": product, "serial": mserial,
            "name": mprops.get("display-name", connector),
            "modes": [{"id": m[0], "width": m[1], "height": m[2], "rate": m[3], "current": bool(m[6].get("is-current"))} for m in modes],
        })
    lms = [{"x": x, "y": y, "scale": scale, "transform": transform, "primary": primary, "connectors": [s[0] for s in specs]}
           for x, y, scale, transform, primary, specs, _ in logical]
    return serial, mons, lms, props


def snapshot():
    """The live layout in the saved format."""
    _, mons, lms, props = current()
    by_conn = {m["connector"]: m for m in mons}
    out = {"layout-mode": props.get("layout-mode"), "logical": []}
    for lm in lms:
        monitors = []
        for c in lm["connectors"]:
            m = by_conn[c]
            mode = next((md for md in m["modes"] if md["current"]), None)
            monitors.append({"connector": c, "vendor": m["vendor"], "product": m["product"], "serial": m["serial"], "name": m["name"],
                             "width": mode["width"], "height": mode["height"], "rate": round(mode["rate"], 3)})
        out["logical"].append({k: lm[k] for k in ("x", "y", "scale", "transform", "primary")} | {"monitors": monitors})
    return out


def find_monitor(want, mons):
    """The connected monitor matching a saved one: same identity (connector
    preferred among equals), else the same connector with the same vendor."""
    same = [m for m in mons if (m["vendor"], m["product"], m["serial"]) == (want["vendor"], want["product"], want["serial"])]
    if same:
        return next((m for m in same if m["connector"] == want["connector"]), same[0])
    return next((m for m in mons if m["connector"] == want["connector"] and m["vendor"] == want["vendor"]), None)


def find_mode(want, mon):
    fits = [md for md in mon["modes"] if md["width"] == want["width"] and md["height"] == want["height"]]
    if not fits:
        return None
    return min(fits, key=lambda md: abs(md["rate"] - want["rate"]))


def plan(saved):
    """Returns (serial, logical monitor config for ApplyMonitorsConfig, whether
    it differs from the live state, reason) or (None, None, None, reason)."""
    serial, mons, lms, _ = current()
    config, used = [], set()
    for lm in saved["logical"]:
        specs = []
        for want in lm["monitors"]:
            mon = find_monitor(want, mons)
            if mon is None:
                return None, None, None, f"{want['name']} ({want['connector']}) is not connected yet"
            mode = find_mode(want, mon)
            if mode is None:
                return None, None, None, f"{want['name']} on {mon['connector']} has no {want['width']}x{want['height']} mode yet"
            specs.append((mon["connector"], mode["id"]))
            used.add(mon["connector"])
        config.append((lm["x"], lm["y"], lm["scale"], lm["transform"], lm["primary"], specs))
    # Compare with the live state (positions, rotation, scale, primary, modes).
    live = {}
    for lm in lms:
        for c in lm["connectors"]:
            m = next(x for x in mons if x["connector"] == c)
            mode = next((md["id"] for md in m["modes"] if md["current"]), None)
            live[c] = (lm["x"], lm["y"], round(lm["scale"], 3), lm["transform"], lm["primary"], mode)
    want = {c: (x, y, round(s, 3), t, p, mode) for x, y, s, t, p, specs in config for c, mode in specs}
    differs = live != want
    return serial, config, differs, "differs" if differs else "already applied"


def apply_config(serial, config, method):
    logical = [(x, y, float(s), t, p, [(c, mode, {}) for c, mode in specs]) for x, y, s, t, p, specs in config]
    # No properties: GNOME keeps its layout mode (it refuses to set it explicitly
    # unless the user may change it).
    args = GLib.Variant("(uua(iiduba(ssa{sv}))a{sv})", (serial, method, logical, {}))
    call("ApplyMonitorsConfig", args)


def load():
    try:
        with open(SAVED) as f:
            return json.load(f)
    except FileNotFoundError:
        sys.exit(f"no saved layout ({SAVED}); run `{sys.argv[0]} save` first")


def describe(layout):
    rot = {0: "", 1: " rotated left", 2: " upside down", 3: " rotated right"}
    for lm in layout["logical"]:
        for m in lm["monitors"]:
            print(f"  {m['connector']:<7} {m['name'][:28]:<28} {m['width']}x{m['height']}@{m['rate']:.2f} at "
                  f"{lm['x']},{lm['y']} scale {lm['scale']:g}{rot.get(lm['transform'], ' transform %d' % lm['transform'])}"
                  f"{'  (primary)' if lm['primary'] else ''}")


def try_apply(saved, method=PERSISTENT, quiet=False):
    """One attempt. Returns 'applied', 'ok' (nothing to do) or 'waiting'."""
    try:
        serial, config, differs, reason = plan(saved)
        if config is None:
            if not quiet:
                log(f"waiting: {reason}")
            return "waiting"
        if not differs:
            return "ok"
        apply_config(serial, config, method)
        log("applied the saved layout" if method != VERIFY else "the saved layout is valid (not applied)")
        return "applied"
    except GLib.Error as e:
        log(f"could not apply yet: {e.message}")
        return "waiting"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=["save", "show", "apply", "watch", "install", "uninstall"])
    ap.add_argument("--seconds", type=float, default=180, help="watch: how long to keep checking after login (default 180)")
    ap.add_argument("--every", type=float, default=3, help="watch: seconds between checks (default 3)")
    ap.add_argument("--verify", action="store_true", help="apply: only check that the saved layout would be accepted")
    a = ap.parse_args()

    if a.command == "save":
        layout = snapshot()
        os.makedirs(os.path.dirname(SAVED), exist_ok=True)
        with open(SAVED, "w") as f:
            json.dump(layout, f, indent=2)
        print(f"saved to {SAVED}:")
        describe(layout)
    elif a.command == "show":
        print(f"saved ({SAVED}):")
        describe(load())
        print("current:")
        describe(snapshot())
        _, _, differs, reason = plan(load())
        print(f"-> {reason}")
    elif a.command == "apply":
        r = try_apply(load(), VERIFY if a.verify else PERSISTENT)
        print({"ok": "the saved layout is already in place", "applied": "done", "waiting": "not applied"}[r])
    elif a.command == "watch":
        saved = load()
        end = time.time() + a.seconds
        applied, last_wait = 0, None
        log(f"checking the monitor layout for {a.seconds:.0f} s")
        while time.time() < end:
            r = try_apply(saved, quiet=last_wait is not None and time.time() - last_wait < 15)
            if r == "applied":
                applied += 1
                time.sleep(2)  # let the monitors settle before checking again
            elif r == "waiting":
                last_wait = last_wait or time.time()
            time.sleep(a.every)
        log(f"done ({applied} correction{'s' if applied != 1 else ''})")
    elif a.command == "install":
        if not os.path.exists(SAVED):
            sys.exit(f"no saved layout; run `{sys.argv[0]} save` first")
        os.makedirs(os.path.dirname(AUTOSTART), exist_ok=True)
        with open(AUTOSTART, "w") as f:
            f.write("[Desktop Entry]\nType=Application\nName=Restore monitor layout (gits-file-visualizer)\n"
                    f"Exec=/usr/bin/python3 {os.path.abspath(__file__)} watch\n"
                    "NoDisplay=true\nX-GNOME-Autostart-enabled=true\n")
        print(f"installed {AUTOSTART}: the saved layout is restored at every login")
    elif a.command == "uninstall":
        try:
            os.remove(AUTOSTART)
            print(f"removed {AUTOSTART}")
        except FileNotFoundError:
            print("not installed")


if __name__ == "__main__":
    main()
