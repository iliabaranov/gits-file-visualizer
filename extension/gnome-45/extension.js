/* gits-file-visualizer — GNOME Shell 45+ extension (ES modules), Wayland and X11.
 * The GNOME 42–44 version lives in ../gnome-42; keep the two in step.
 *
 * Launches `gits-render wallpaper` through Meta.WaylandClient (so its windows
 * can be recognised reliably and hidden from window lists), keeps those windows
 * minimized, and shows a Clutter.Clone of each one inside every background
 * actor for its monitor: desktop, overview workspaces and workspace-switch
 * animations. The lock screen keeps its normal wallpaper.
 *
 * It also tells the renderer, over its stdin, how much of each monitor is
 * covered by windows (it stops drawing mostly covered ones), when the screen
 * is locked, and when a screencast or screen share is running (file names can
 * be obfuscated then).
 */
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Background from 'resource:///org/gnome/shell/ui/background.js';
import * as UnlockDialog from 'resource:///org/gnome/shell/ui/unlockDialog.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const ENCODER = new TextEncoder();

const TITLE_PREFIX = 'gits-file-visualizer|';
const FADE_MS = 900;
const RESPAWN_DELAY_MS = 3000;
const MAX_RESPAWNS = 5;
// gits-render exits with this code to ask for a restart (see app.rs EXIT_RESTART).
const EXIT_RESTART = 75;
const RESPAWN_WINDOW_US = 5 * 60 * 1000000;
const VISIBILITY_DEBOUNCE_MS = 150;
const VISIBILITY_POLL_S = 3;
// Sample grid used to measure how much of a monitor's work area is covered.
const GRID_X = 32;
const GRID_Y = 18;

const CONFIG_PATH = GLib.build_filenamev([GLib.get_user_config_dir(), 'gits-file-visualizer', 'config.toml']);
const SCREENSAVER_DEFAULT = {enabled: true, after: 300};
// A screensaver wait that was skipped because something inhibits idle (a video,
// a presentation) is retried this often.
const INHIBIT_RECHECK_S = 30;
// The shell reports idle a little before our own idle-delay watch fires.
const IDLE_SLACK_MS = 2000;
const SS_FADE_MS = 1500;
const SESSION_INHIBIT_IDLE = 8;
const SHIELD_OPACITY = 215;
const BLUR_RADIUS = 90; // GNOME's own value for the lock screen

function info(msg) {
    log(`[gits] ${msg}`);
}

/* "5m", "1h", "30s", "300" -> seconds, or null. */
function parseDuration(text) {
    const m = /^\s*([0-9.]+)\s*(ms|s|m|min|h|d)?\s*$/.exec(text);
    if (!m)
        return null;
    const unit = {ms: 0.001, s: 1, m: 60, min: 60, h: 3600, d: 86400}[m[2] ?? 's'];
    return parseFloat(m[1]) * unit;
}

/* The [screensaver] section of the renderer's config (the extension reads only
 * this; the renderer validates the rest). */
function readScreensaverConfig() {
    const cfg = {...SCREENSAVER_DEFAULT};
    let text;
    try {
        const [ok, bytes] = GLib.file_get_contents(CONFIG_PATH);
        if (!ok)
            return cfg;
        text = new TextDecoder().decode(bytes);
    } catch (e) {
        return cfg;
    }
    let section = '';
    for (const raw of text.split('\n')) {
        const line = raw.replace(/#.*$/, '').trim();
        const sec = /^\[([^\]]+)\]$/.exec(line);
        if (sec) {
            section = sec[1].trim();
            continue;
        }
        if (section !== 'screensaver')
            continue;
        const kv = /^([A-Za-z_]+)\s*=\s*(.+)$/.exec(line);
        if (!kv)
            continue;
        const value = kv[2].trim().replace(/^"(.*)"$/, '$1');
        if (kv[1] === 'enabled')
            cfg.enabled = value === 'true';
        else if (kv[1] === 'after')
            cfg.after = parseDuration(value) ?? cfg.after;
    }
    return cfg;
}

function findRenderer() {
    const env = GLib.getenv('GITS_RENDER_BIN');
    if (env && GLib.file_test(env, GLib.FileTest.IS_EXECUTABLE))
        return env;
    const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'gits-render']);
    if (GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE))
        return local;
    return GLib.find_program_in_path('gits-render');
}

/* Monitor index → connector name, by probing candidate names. */
function connectorNames() {
    const names = new Map();
    const candidates = [];
    try {
        const dir = Gio.File.new_for_path('/sys/class/drm');
        const e = dir.enumerate_children('standard::name', Gio.FileQueryInfoFlags.NONE, null);
        let fi;
        while ((fi = e.next_file(null))) {
            const m = fi.get_name().match(/^card\d+-(.+)$/);
            if (m)
                candidates.push(m[1]);
        }
    } catch (e) {
        // No DRM sysfs (e.g. a nested or headless session): fall through.
    }
    for (let i = 0; i < 8; i++)
        candidates.push(`Meta-${i}`);
    const mm = global.backend.get_monitor_manager();
    for (const c of candidates) {
        let idx = -1;
        try {
            idx = mm.get_monitor_for_connector(c);
        } catch (e) {}
        if (idx >= 0 && !names.has(idx))
            names.set(idx, c);
    }
    return names;
}

function currentMonitors() {
    const names = connectorNames();
    return Main.layoutManager.monitors.map(m => ({
        index: m.index,
        width: m.width,
        height: m.height,
        name: names.get(m.index) ?? `monitor-${m.index}`,
    }));
}

/* Wayland session: the renderer is a Wayland client launched through
 * Meta.WaylandClient (reliable ownership, hidden from window lists, minimized).
 * X11 session: it is a plain subprocess whose override-redirect windows are
 * recognised by pid; the window manager never manages them. */
class RendererProcess {
    constructor(onExit) {
        this._onExit = onExit;
        this._x11 = !Meta.is_wayland_compositor();
        this._pid = 0;
        this._client = null;
        this._proc = null;
        this._stdin = null;
    }

    get running() {
        return this._proc !== null;
    }

    start(monitors) {
        const bin = findRenderer();
        if (!bin) {
            info('gits-render not found (GITS_RENDER_BIN, ~/.local/bin, PATH); showing the normal wallpaper');
            return false;
        }
        const argv = [bin, 'wallpaper'];
        for (const m of monitors)
            argv.push('--output', `${m.index}:${m.width}x${m.height}:${m.name}`);
        const launcher = new Gio.SubprocessLauncher({ flags: Gio.SubprocessFlags.STDIN_PIPE });
        if (!GLib.getenv('RUST_LOG'))
            launcher.setenv('RUST_LOG', 'info', false);
        let proc;
        if (this._x11) {
            launcher.unsetenv('WAYLAND_DISPLAY');
            proc = launcher.spawnv(argv);
        } else {
            this._client = Meta.WaylandClient.new(global.context, launcher);
            proc = this._client.spawnv(global.display, argv);
        }
        this._proc = proc;
        this._pid = parseInt(proc.get_identifier());
        this._stdin = proc.get_stdin_pipe();
        info(`started ${argv.join(' ')} (pid ${proc.get_identifier()})`);
        proc.wait_async(null, (p, res) => {
            try {
                p.wait_finish(res);
            } catch (e) {}
            if (this._proc !== p)
                return;
            const status = p.get_if_exited() ? `exit ${p.get_exit_status()}` : 'killed';
            this._proc = null;
            this._stdin = null;
            this._client = null;
            this._onExit(status);
        });
        return true;
    }

    owns(window) {
        try {
            if (this._x11)
                return this._proc !== null && window.get_pid() === this._pid;
            return this._client !== null && this._client.owns_window(window);
        } catch (e) {
            return false;
        }
    }

    hideFromWindowList(window) {
        this._client?.hide_from_window_list(window);
    }

    showInWindowList(window) {
        this._client?.show_in_window_list(window);
    }

    get x11() {
        return this._x11;
    }

    send(line) {
        if (!this._stdin)
            return;
        try {
            this._stdin.write_all(ENCODER.encode(`${line}\n`), null);
            this._stdin.flush(null);
        } catch (e) {
            info(`control write failed: ${e.message}`);
        }
    }

    stop() {
        const p = this._proc;
        this._proc = null;
        try {
            this._stdin?.close(null);
        } catch (e) {}
        this._stdin = null;
        this._client = null;
        p?.force_exit();
    }
}

class Wallpaper {
    constructor() {
        this._renderer = null;
        this._origCreate = null;
        this._holders = new Set();
        this._sources = new Map(); // monitor index → renderer window actor
        this._windows = new Map(); // MetaWindow → signal ids (renderer windows)
        this._actorSignals = new Map(); // renderer window actor → 'show' signal id
        this._tracked = new Map(); // MetaWindow → signal ids (other windows)
        this._signals = [];
        this._sent = new Map();
        this._handles = new Set();
        this._timers = new Set();
        this._respawns = [];
        this._enabled = false;
        this._idleMonitor = null;
        this._ssCfg = {...SCREENSAVER_DEFAULT};
        this._ssWatch = 0;
        this._blankWatch = 0;
        this._activeWatch = 0;
        this._ssActive = false;
        this._ssActors = [];
        this._ssRecheck = 0;
        this._blanked = false;
        this._pendingActive = false;
        this._origEffects = null;
        this._powerProxy = null;
        this._powerId = 0;
        this._cfgMonitor = null;
        this._sessionSettings = null;
    }

    enable() {
        this._enabled = true;
        this._hookBackgrounds();
        this._hookLockScreen();
        this._watchDisplayPower();
        this._renderer = new RendererProcess(status => this._onRendererExit(status));
        this._connect(global.display, 'window-created', (d, w) => this._onWindowCreated(w));
        this._connect(global.window_manager, 'map', (wm, actor) => this._onMap(actor));
        this._connect(global.display, 'restacked', () => this._queueVisibility());
        this._connect(global.display, 'in-fullscreen-changed', () => this._queueVisibility());
        this._connect(global.display, 'workareas-changed', () => this._queueVisibility());
        this._connect(global.workspace_manager, 'active-workspace-changed', () => this._queueVisibility());
        this._connect(Main.overview, 'showing', () => this._queueVisibility());
        this._connect(Main.overview, 'hidden', () => this._queueVisibility());
        this._connect(Main.sessionMode, 'updated', () => this._queueVisibility());
        this._connect(Main.layoutManager, 'monitors-changed', () => this._restartRenderer('monitors changed'));
        const rac = global.backend.get_remote_access_controller?.();
        if (rac)
            this._connect(rac, 'new-handle', (c, handle) => this._onRemoteHandle(handle));
        for (const actor of global.get_window_actors())
            this._track(actor.meta_window);
        this._poll = GLib.timeout_add_seconds(GLib.PRIORITY_LOW, VISIBILITY_POLL_S, () => {
            this._updateVisibility();
            return GLib.SOURCE_CONTINUE;
        });
        this._reloadBackgrounds();
        this._concealStrays();
        this._setupIdle();
        this._renderer.start(currentMonitors());
    }

    disable() {
        this._enabled = false;
        if (this._poll) {
            GLib.source_remove(this._poll);
            this._poll = 0;
        }
        for (const id of this._timers)
            GLib.source_remove(id);
        this._timers.clear();
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        for (const [w, ids] of [...this._tracked, ...this._windows])
            ids.forEach(id => w.disconnect(id));
        this._tracked.clear();
        this._windows.clear();
        this._releaseActors();
        this._handles.clear();
        this._teardownIdle();
        this._renderer?.stop();
        this._renderer = null;
        this._sources.clear();
        this._unhookBackgrounds();
        this._unhookLockScreen();
        this._unwatchDisplayPower();
        for (const h of [...this._holders])
            h.destroy();
        this._holders.clear();
        this._sent.clear();
    }

    _connect(obj, signal, fn) {
        this._signals.push([obj, obj.connect(signal, fn)]);
    }

    _timeout(ms, fn) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._timers.delete(id);
            fn();
            return GLib.SOURCE_REMOVE;
        });
        this._timers.add(id);
    }

    // --- backgrounds --------------------------------------------------------

    _hookBackgrounds() {
        const ext = this;
        const proto = Background.BackgroundManager.prototype;
        this._origCreate = proto._createBackgroundActor;
        const orig = this._origCreate;
        proto._createBackgroundActor = function () {
            const actor = orig.call(this);
            try {
                ext._decorate(this, actor);
            } catch (e) {
                logError(e, '[gits] decorating background');
            }
            return actor;
        };
    }

    _unhookBackgrounds() {
        if (this._origCreate) {
            Background.BackgroundManager.prototype._createBackgroundActor = this._origCreate;
            this._origCreate = null;
        }
    }

    /* Recreate existing background actors so they pass through our hook. */
    _reloadBackgrounds() {
        Main.layoutManager._updateBackgrounds();
        try {
            Main.screenShield?._dialog?._updateBackgrounds();
        } catch (e) {
            info(`lock screen backgrounds not refreshed: ${e.message}`);
        }
        try {
            Main.overview._overview._controls._workspacesDisplay._updateWorkspacesViews();
        } catch (e) {
            info(`overview backgrounds not refreshed: ${e.message}`);
        }
    }

    _decorate(manager, bgActor) {
        const container = manager._container;
        const shield = container instanceof St.Widget && container.has_style_class_name('screen-shield-background');
        if (shield && !this._lockSettings().live)
            return; // lock screen: keep GNOME's own wallpaper unless [lock_screen] live
        // No layout manager: children keep the position/size their constraints give them.
        // The lock screen gets an opaque black backing, so GNOME's wallpaper never shows through.
        const holder = shield
            ? new St.Widget({ reactive: false, opacity: 255, style: 'background-color: black;' })
            : new Clutter.Actor({ reactive: false, opacity: 0 });
        holder.add_constraint(new Clutter.BindConstraint({ source: bgActor, coordinate: Clutter.BindCoordinate.SIZE }));
        holder._gitsMonitor = bgActor.monitor;
        holder._gitsShield = shield;
        bgActor.add_child(holder);
        this._holders.add(holder);
        holder.connect('destroy', () => this._holders.delete(holder));
        this._fill(holder);
    }

    _fill(holder) {
        const source = this._sources.get(holder._gitsMonitor) ?? null;
        // Compare with our own record of the clone's source: reading
        // `clone.source` throws once the renderer window's actor is disposed,
        // which would leave the stale clone in place.
        const clone = holder.get_first_child();
        if (clone && clone._gitsSource === source)
            return;
        holder.destroy_all_children();
        if (!source) {
            holder.remove_all_transitions();
            holder.opacity = holder._gitsShield ? 255 : 0;
            return;
        }
        // A Clone paints its source scaled to its own allocation: the holder's size.
        const scaled = new Clutter.Clone({ source, x: 0, y: 0 });
        scaled._gitsSource = source;
        scaled.add_constraint(new Clutter.BindConstraint({ source: holder, coordinate: Clutter.BindCoordinate.SIZE }));
        holder.add_child(scaled);
        if (holder._gitsShield) {
            // Slightly dimmed so the clock stays readable; fades in over the black.
            scaled.opacity = 0;
            scaled.ease({ opacity: SHIELD_OPACITY, duration: FADE_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD });
        } else {
            holder.ease({ opacity: 255, duration: FADE_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD });
        }
    }

    _refill(monitor) {
        this._syncOverlay();
        for (const h of this._holders) {
            if (monitor !== undefined && h._gitsMonitor !== monitor)
                continue;
            try {
                this._fill(h);
            } catch (e) {
                logError(e, '[gits] refreshing a wallpaper clone');
            }
        }
    }

    // --- renderer windows ---------------------------------------------------

    _monitorOf(window) {
        const title = window.get_title() ?? '';
        if (!title.startsWith(TITLE_PREFIX))
            return null;
        const n = parseInt(title.slice(TITLE_PREFIX.length));
        return Number.isInteger(n) ? n : null;
    }

    /* X11: any override-redirect window that looks like ours, including one left
     * by an earlier renderer (a shell restart, a crash) or whose pid is not known
     * yet. These must never be visible: the wallpaper is only shown through the
     * background clones. */
    _isRendererWindowX11(window) {
        try {
            return window.is_override_redirect() &&
                (window.get_title() ?? '').startsWith(TITLE_PREFIX) &&
                window.get_wm_class() === 'gits-file-visualizer';
        } catch (e) {
            return false;
        }
    }

    _concealStrays() {
        if (!this._renderer?.x11)
            return;
        for (const actor of global.get_window_actors()) {
            const w = actor.meta_window;
            if (w && this._isRendererWindowX11(w) && (actor.visible || !this._actorSignals.has(actor)))
                this._conceal(actor);
        }
    }

    _onWindowCreated(window) {
        if (this._renderer?.x11 && this._isRendererWindowX11(window)) {
            const a = window.get_compositor_private();
            if (a)
                this._conceal(a);
        }
        if (!this._renderer?.owns(window)) {
            this._track(window);
            return;
        }
        // Suppress the map animation: the window must never flash on screen.
        const actor = window.get_compositor_private();
        if (actor)
            Main.wm.skipNextEffect(actor);
        if (this._renderer.x11 && actor)
            this._conceal(actor);
        this._renderer.hideFromWindowList(window);
    }

    _onMap(actor) {
        const window = actor.meta_window;
        if (this._renderer?.x11 && this._isRendererWindowX11(window))
            this._conceal(actor);
        if (!this._renderer?.owns(window) || this._windows.has(window))
            return;
        const monitor = this._monitorOf(window);
        if (monitor === null || monitor >= Main.layoutManager.monitors.length) {
            info(`ignoring renderer window with title ${window.get_title()}`);
            return;
        }
        if (this._renderer.x11) {
            // Override-redirect window: no minimizing, sticking or moving.
            this._conceal(actor);
            this._windows.set(window, [window.connect('unmanaged', () => this._forgetWindow(window, monitor))]);
            this._sources.set(monitor, actor);
            info(`renderer window for monitor ${monitor} adopted (X11)`);
            this._refill(monitor);
            this._sent.clear();
            this._queueVisibility();
            return;
        }
        window.stick();
        window.move_to_monitor(monitor);
        // Shown only through the clones below (scaled to each background).
        Main.wm.skipNextEffect(actor);
        this._minimize(window);
        this._conceal(actor);
        const ids = [
            window.connect('notify::minimized', () => {
                if (!window.minimized) {
                    Main.wm.skipNextEffect(actor);
                    this._minimize(window);
                }
            }),
            // Never a focus target (e.g. something activating the top window).
            window.connect('focus', () => {
                info(`renderer window for monitor ${monitor} was focused; minimizing it`);
                Main.wm.skipNextEffect(actor);
                this._minimize(window);
            }),
            window.connect('unmanaged', () => this._forgetWindow(window, monitor)),
        ];
        this._windows.set(window, ids);
        this._sources.set(monitor, actor);
        info(`renderer window for monitor ${monitor} adopted`);
        this._refill(monitor);
        this._sent.clear();
        this._queueVisibility();
    }

    /* Mutter 45+ leaves a minimized renderer window's actor shown (and painting
     * at its own size over the wallpaper), so hide it ourselves and keep it
     * hidden. The clones still paint a hidden source. */
    _conceal(actor) {
        actor.hide();
        if (this._actorSignals.has(actor))
            return;
        this._actorSignals.set(actor, actor.connect('show', () => actor.hide()));
    }

    _releaseActor(actor) {
        const id = this._actorSignals.get(actor);
        this._actorSignals.delete(actor);
        if (id === undefined)
            return;
        try {
            actor.disconnect(id);
        } catch (e) {}
    }

    _releaseActors() {
        for (const actor of [...this._actorSignals.keys()])
            this._releaseActor(actor);
    }

    /* Minimizing is refused while a window is hidden from the window list. */
    _minimize(window) {
        this._renderer?.showInWindowList(window);
        window.minimize();
        this._renderer?.hideFromWindowList(window);
    }

    _forgetWindow(window, monitor) {
        this._windows.get(window)?.forEach(id => window.disconnect(id));
        this._windows.delete(window);
        const actor = window.get_compositor_private();
        if (actor)
            this._releaseActor(actor);
        if (this._sources.get(monitor) === window.get_compositor_private())
            this._sources.delete(monitor);
        this._refill(monitor);
    }

    _onRendererExit(status) {
        info(`gits-render stopped (${status})`);
        for (const [w, ids] of this._windows)
            ids.forEach(id => w.disconnect(id));
        this._windows.clear();
        this._sources.clear();
        this._refill();
        this._sent.clear();
        // 'restart' is our own deliberate restart (see _restartRenderer), not a failure.
        if (!this._enabled || status === 'exit 0' || status === 'restart')
            return;
        if (status === `exit ${EXIT_RESTART}`) {
            // Requested restart (e.g. power source changed, switch GPU): not a failure.
            this._timeout(200, () => {
                if (this._enabled && !this._renderer.running)
                    this._renderer.start(currentMonitors());
            });
            return;
        }
        const now = GLib.get_monotonic_time();
        this._respawns = this._respawns.filter(t => now - t < RESPAWN_WINDOW_US);
        if (this._respawns.length >= MAX_RESPAWNS) {
            info('gits-render keeps failing; giving up until the extension is re-enabled');
            return;
        }
        this._respawns.push(now);
        this._timeout(RESPAWN_DELAY_MS, () => {
            if (this._enabled && !this._renderer.running)
                this._renderer.start(currentMonitors());
        });
    }

    _restartRenderer(why) {
        info(`restarting gits-render: ${why}`);
        this._renderer.stop();
        this._onRendererExit('restart');
        this._timeout(500, () => {
            if (this._enabled && !this._renderer.running)
                this._renderer.start(currentMonitors());
        });
    }

    // --- screensaver, lock screen, blanking ----------------------------------------

    _idleDelayMs() {
        return (this._sessionSettings?.get_uint('idle-delay') ?? 0) * 1000;
    }

    _setupIdle() {
        this._idleMonitor = global.backend.get_core_idle_monitor();
        this._sessionSettings = new Gio.Settings({schema_id: 'org.gnome.desktop.session'});
        this._sessionSettingsId = this._sessionSettings.connect('changed::idle-delay', () => this._armIdle());
        const file = Gio.File.new_for_path(CONFIG_PATH);
        try {
            this._cfgMonitor = file.monitor_file(Gio.FileMonitorFlags.NONE, null);
            this._cfgMonitorId = this._cfgMonitor.connect('changed', () => this._armIdle());
        } catch (e) {
            info(`cannot watch ${CONFIG_PATH}: ${e.message}`);
        }
        this._hookScreenSaverSignal();
        this._hookShieldFade();
        this._connect(Main.screenShield, 'active-changed', () => this._onShieldChanged());
        this._armIdle();
    }

    _teardownIdle() {
        this._stopScreensaver(false);
        this._removeWatches();
        if (this._ssRecheck) {
            GLib.source_remove(this._ssRecheck);
            this._ssRecheck = 0;
        }
        if (this._sessionSettings && this._sessionSettingsId)
            this._sessionSettings.disconnect(this._sessionSettingsId);
        this._sessionSettings = null;
        if (this._cfgMonitor) {
            this._cfgMonitor.disconnect(this._cfgMonitorId);
            this._cfgMonitor.cancel();
            this._cfgMonitor = null;
        }
        this._unhookScreenSaverSignal();
        this._blanked = false;
        this._idleMonitor = null;
    }

    _removeWatches() {
        for (const key of ['_ssWatch', '_blankWatch', '_activeWatch']) {
            if (this[key]) {
                this._idleMonitor?.remove_watch(this[key]);
                this[key] = 0;
            }
        }
    }

    /* (Re)reads the config and the blank time and installs the idle watches. */
    _armIdle() {
        if (!this._idleMonitor)
            return;
        for (const key of ['_ssWatch', '_blankWatch']) {
            if (this[key]) {
                this._idleMonitor.remove_watch(this[key]);
                this[key] = 0;
            }
        }
        this._ssCfg = readScreensaverConfig();
        if (this._ssCfg.enabled && this._ssCfg.after > 0)
            this._ssWatch = this._idleMonitor.add_idle_watch(Math.round(this._ssCfg.after * 1000), () => this._onScreensaverIdle());
        const blankMs = this._idleDelayMs();
        if (blankMs > 0)
            this._blankWatch = this._idleMonitor.add_idle_watch(blankMs, () => this._onBlankIdle());
        if (!this._ssCfg.enabled && this._ssActive)
            this._stopScreensaver(true);
        info(`screensaver ${this._ssCfg.enabled && this._ssCfg.after > 0 ? `after ${this._ssCfg.after}s idle` : 'off'}; blank at ${blankMs / 1000}s idle`);
    }

    /* One user-activity watch serves the screensaver and the blanked state. */
    _ensureActiveWatch() {
        if (this._activeWatch || !this._idleMonitor)
            return;
        this._activeWatch = this._idleMonitor.add_user_active_watch(() => {
            this._activeWatch = 0;
            this._onUserActive();
        });
    }

    _onUserActive() {
        this._stopScreensaver(true);
        if (this._ssRecheck) {
            GLib.source_remove(this._ssRecheck);
            this._ssRecheck = 0;
        }
        if (this._blanked) {
            this._blanked = false;
            this._updateVisibility();
        }
    }

    _onScreensaverIdle() {
        if (this._ssActive || Main.screenShield?.active || Main.sessionMode.isLocked)
            return;
        this._ensureActiveWatch();
        this._tryStartScreensaver();
    }

    _tryStartScreensaver() {
        this._ssRecheck = 0;
        if (this._ssActive || Main.screenShield?.active || !this._renderer)
            return;
        if (this._idleMonitor.get_idletime() < this._ssCfg.after * 1000 - IDLE_SLACK_MS)
            return; // the user came back
        Gio.DBus.session.call('org.gnome.SessionManager', '/org/gnome/SessionManager',
            'org.freedesktop.DBus.Properties', 'Get',
            new GLib.Variant('(ss)', ['org.gnome.SessionManager', 'InhibitedActions']),
            new GLib.VariantType('(v)'), Gio.DBusCallFlags.NONE, 1000, null, (conn, res) => {
                let inhibited = false;
                try {
                    const flags = conn.call_finish(res).deepUnpack()[0].deepUnpack();
                    inhibited = (flags & SESSION_INHIBIT_IDLE) !== 0;
                } catch (e) {
                    // No session manager (a nested shell): nothing inhibits.
                }
                if (!this._enabled)
                    return;
                if (inhibited) {
                    this._ssRecheck = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, INHIBIT_RECHECK_S, () => {
                        this._tryStartScreensaver();
                        return GLib.SOURCE_REMOVE;
                    });
                    return;
                }
                this._startScreensaver();
            });
    }

    _startScreensaver() {
        if (this._ssActive || Main.screenShield?.active)
            return;
        this._ssActive = true;
        info('screensaver on');
        this._syncOverlay();
        this._updateVisibility();
    }

    _stopScreensaver(animate) {
        if (!this._ssActive && this._ssActors.length === 0)
            return;
        this._ssActive = false;
        this._destroyOverlay(animate);
        if (this._enabled)
            this._updateVisibility();
        info('screensaver off');
    }

    _destroyOverlay(animate) {
        const actors = this._ssActors;
        this._ssActors = [];
        for (const a of actors) {
            if (animate) {
                a.remove_all_transitions();
                a.ease({opacity: 0, duration: 300, mode: Clutter.AnimationMode.EASE_OUT_QUAD, onComplete: () => a.destroy()});
            } else {
                a.destroy();
            }
        }
    }

    /* A black, full-monitor actor per monitor with a clone of that monitor's
     * renderer window, above everything (panel, windows, notifications). */
    _syncOverlay() {
        if (!this._ssActive)
            return;
        this._destroyOverlay(false);
        for (const m of Main.layoutManager.monitors) {
            const source = this._sources.get(m.index);
            if (!source)
                continue;
            const box = new St.Widget({x: m.x, y: m.y, width: m.width, height: m.height, reactive: false, opacity: 0, style: 'background-color: black;'});
            const clone = new Clutter.Clone({source});
            clone.add_constraint(new Clutter.BindConstraint({source: box, coordinate: Clutter.BindCoordinate.SIZE}));
            box.add_child(clone);
            Main.uiGroup.add_child(box);
            Main.uiGroup.set_child_above_sibling(box, null);
            box.ease({opacity: 255, duration: SS_FADE_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
            this._ssActors.push(box);
        }
    }

    /* The lock screen shows its own copy of the tower: drop the overlay. */
    _onShieldChanged() {
        if (Main.screenShield.active)
            this._stopScreensaver(false);
        else
            this._pendingActive = false;
        this._updateVisibility();
    }

    /* Idle for as long as GNOME's blank time: the display is about to go dark. */
    _onBlankIdle() {
        this._blanked = true;
        this._ensureActiveWatch();
        this._releasePendingActive();
        this._updateVisibility();
    }

    /* gnome-settings-daemon blanks the display the moment the shield reports
     * itself active (any lock), whatever the idle time. The tower should stay
     * lit on the lock screen, so that signal is held back until the session has
     * been idle for the blank time (the shell's own idle lock arrives only after
     * that); the blank watch above releases it if the lock came earlier. */
    _hookScreenSaverSignal() {
        const impl = Main.screenShield?._screenSaverDBus?._dbusImpl;
        if (!impl) {
            info('screen saver D-Bus object not found; the lock screen will blank as usual');
            return;
        }
        this._ssImpl = impl;
        this._origEmit = impl.emit_signal;
        const ext = this;
        impl.emit_signal = function (name, params) {
            if (name === 'ActiveChanged' && ext._holdActiveChanged(params))
                return undefined;
            return ext._origEmit.call(this, name, params);
        };
    }

    _unhookScreenSaverSignal() {
        if (this._shieldFadeHooked) {
            delete Main.screenShield._activateFade;
            this._shieldFadeHooked = false;
        }
        if (!this._ssImpl)
            return;
        delete this._ssImpl.emit_signal;
        this._releasePendingActive();
        this._ssImpl = null;
        this._origEmit = null;
    }

    /* A lock requested by the user (not by idling) slides the shield in and then
     * fades the whole screen to black, so the lock screen is only seen after the
     * next keypress. Skip that fade while the blank time has not been reached,
     * but still complete the lock (the shield becomes active, `Lock` returns). */
    _hookShieldFade() {
        const shield = Main.screenShield;
        const orig = shield._activateFade;
        if (!orig)
            return;
        const ext = this;
        shield._activateFade = function (lightbox, time) {
            if (lightbox === this._shortLightbox && ext._enabled && ext._lockLive() && !ext._idleLongEnough()) {
                this._completeLockScreenShown();
                return;
            }
            orig.call(this, lightbox, time);
        };
        this._shieldFadeHooked = true;
    }

    _idleLongEnough() {
        const blankMs = this._idleDelayMs();
        return blankMs > 0 && this._idleMonitor.get_idletime() >= blankMs - IDLE_SLACK_MS;
    }

    _holdActiveChanged(params) {
        const active = params.deepUnpack()[0];
        if (!active) {
            this._pendingActive = false;
            return false;
        }
        if (!this._lockLive() || this._idleLongEnough())
            return false;
        this._pendingActive = true;
        info('lock screen shown; keeping the display lit until the blank time');
        return true;
    }

    _releasePendingActive() {
        if (!this._pendingActive || !this._ssImpl)
            return;
        this._pendingActive = false;
        if (Main.screenShield.active)
            this._origEmit.call(this._ssImpl, 'ActiveChanged', GLib.Variant.new('(b)', [true]));
    }

    // --- display power and lock screen settings --------------------------------

    /* Mutter's PowerSaveMode: 0 = displays on, > 0 = switched off. */
    _watchDisplayPower() {
        try {
            Gio.DBusProxy.new_for_bus(Gio.BusType.SESSION, Gio.DBusProxyFlags.DO_NOT_AUTO_START_AT_CONSTRUCTION,
                null, 'org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig',
                'org.gnome.Mutter.DisplayConfig', null, (src, res) => {
                    try {
                        const proxy = Gio.DBusProxy.new_for_bus_finish(res);
                        if (!this._enabled)
                            return;
                        this._powerProxy = proxy;
                        this._powerId = proxy.connect('g-properties-changed', () => this._updateVisibility());
                        this._updateVisibility();
                    } catch (e) {
                        info(`display power state unavailable: ${e.message}`);
                    }
                });
        } catch (e) {
            info(`display power state unavailable: ${e.message}`);
        }
    }

    _unwatchDisplayPower() {
        if (this._powerProxy && this._powerId)
            this._powerProxy.disconnect(this._powerId);
        this._powerProxy = null;
        this._powerId = 0;
    }

    _displaysOff() {
        const v = this._powerProxy?.get_cached_property('PowerSaveMode');
        return !!v && v.unpack() > 0;
    }

    /* `[lock_screen]` settings, as the renderer left them (write_lock_info in app.rs). */
    _lockSettings() {
        const fallback = {live: false, blur: 0, dim: 0.7};
        try {
            const path = GLib.build_filenamev([GLib.get_user_runtime_dir(), 'gits-file-visualizer', 'lock-screen.json']);
            const [ok, bytes] = GLib.file_get_contents(path);
            if (!ok)
                return fallback;
            const j = JSON.parse(new TextDecoder().decode(bytes));
            const num = (v, d, lo, hi) => Number.isFinite(Number(v)) ? Math.min(hi, Math.max(lo, Number(v))) : d;
            return {live: j.live === true, blur: num(j.blur, 0, 0, 1), dim: num(j.dim, 0.7, 0.05, 1)};
        } catch (e) {
            return fallback;
        }
    }

    /* GNOME blurs and darkens the lock screen background heavily; with a live lock
     * screen use the configured amounts (no blur at all drops the effect: a blurred
     * live picture is re-blurred every frame). It sets its own values after building
     * the backgrounds, so this runs after it. */
    _hookLockScreen() {
        const proto = UnlockDialog.UnlockDialog?.prototype;
        if (!proto || typeof proto._updateBackgroundEffects !== 'function')
            return;
        this._origEffects = proto._updateBackgroundEffects;
        const orig = this._origEffects;
        const ext = this;
        proto._updateBackgroundEffects = function () {
            orig.call(this);
            try {
                ext._tuneLockEffects(this);
            } catch (e) {
                logError(e, '[gits] lock screen effects');
            }
        };
    }

    _unhookLockScreen() {
        if (!this._origEffects)
            return;
        UnlockDialog.UnlockDialog.prototype._updateBackgroundEffects = this._origEffects;
        this._origEffects = null;
        const dialog = Main.screenShield?._dialog;
        try {
            for (const widget of dialog?._backgroundGroup ?? [])
                widget.get_effect('blur')?.set_enabled(true);
            dialog?._updateBackgroundEffects();
        } catch (e) {}
    }

    _tuneLockEffects(dialog) {
        const s = this._lockSettings();
        if (!s.live || !dialog._backgroundGroup)
            return;
        const scale = St.ThemeContext.get_for_stage(global.stage).scale_factor;
        for (const widget of dialog._backgroundGroup) {
            const effect = widget.get_effect('blur');
            if (!effect)
                continue;
            effect.set_enabled(s.blur > 0.001);
            effect.set({brightness: s.dim, radius: Math.max(1, BLUR_RADIUS * scale * s.blur)});
        }
    }

    /* The live lock screen is on: hold back GNOME's black fade and display blanking. */
    _lockLive() {
        return this._lockSettings().live;
    }

    // --- visibility -----------------------------------------------------------

    _track(window) {
        if (this._tracked.has(window) || this._renderer?.owns(window))
            return;
        const q = () => this._queueVisibility();
        const ids = [
            window.connect('size-changed', q),
            window.connect('position-changed', q),
            window.connect('notify::minimized', q),
            window.connect('unmanaged', () => {
                this._tracked.get(window)?.forEach(id => window.disconnect(id));
                this._tracked.delete(window);
                q();
            }),
        ];
        this._tracked.set(window, ids);
    }

    _queueVisibility() {
        if (this._visTimer)
            return;
        this._visTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, VISIBILITY_DEBOUNCE_MS, () => {
            this._timers.delete(this._visTimer);
            this._visTimer = 0;
            this._updateVisibility();
            return GLib.SOURCE_REMOVE;
        });
        this._timers.add(this._visTimer);
    }

    /* Windows that hide the wallpaper. Excludes our own windows, desktop-icon
     * windows (DING titles them "@!x,y;…" and hides them from window lists; they
     * are transparent), anything hidden from window lists, and translucent or
     * minimized windows. */
    _occluders() {
        const ws = global.workspace_manager.get_active_workspace();
        const types = [Meta.WindowType.NORMAL, Meta.WindowType.DIALOG, Meta.WindowType.MODAL_DIALOG];
        return global.get_window_actors()
            .map(a => a.meta_window)
            .filter(w => w && !w.minimized && w.showing_on_its_workspace() && !w.skip_taskbar &&
                types.includes(w.get_window_type()) &&
                !(w.get_title() ?? '').startsWith('@!') &&
                (w.is_on_all_workspaces() || w.get_workspace() === ws) &&
                !this._windows.has(w) && !this._renderer?.owns(w) &&
                w.get_compositor_private()?.opacity === 255)
            .map(w => w.get_frame_rect());
    }

    /* Share (0..1) of the monitor's work area under the given window rects. */
    _coverage(monitorIndex, rects) {
        const a = Main.layoutManager.getWorkAreaForMonitor(monitorIndex);
        const inside = (x, y) => rects.some(r => x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
        let covered = 0;
        for (let j = 0; j < GRID_Y; j++) {
            for (let i = 0; i < GRID_X; i++) {
                const x = a.x + Math.floor((i + 0.5) * a.width / GRID_X);
                const y = a.y + Math.floor((j + 0.5) * a.height / GRID_Y);
                if (inside(x, y))
                    covered++;
            }
        }
        return covered / (GRID_X * GRID_Y);
    }

    _send(key, line) {
        if (this._sent.get(key) === line)
            return;
        this._sent.set(key, line);
        this._renderer?.send(line);
    }

    _updateVisibility() {
        this._concealStrays();
        if (!this._renderer?.running)
            return;
        // `locked` hides file names and, unless [lock_screen] live, stops drawing; `blanked`
        // (displays off, or idle past the blank time) stops it regardless.
        this._send('locked', `locked ${Main.sessionMode.isLocked ? 1 : 0}`);
        this._send('blanked', `blanked ${this._blanked || this._displaysOff() ? 1 : 0}`);
        this._send('sharing', `sharing ${this._handles.size > 0 ? 1 : 0}`);
        // Windows do not hide the wallpaper from the overview, the screensaver or the lock screen.
        const unobstructed = Main.overview.visible || this._ssActive || Main.sessionMode.isLocked;
        const rects = unobstructed ? [] : this._occluders();
        for (const m of Main.layoutManager.monitors) {
            // The renderer decides what counts as mostly covered (its config).
            const coverage = unobstructed ? 0 : this._coverage(m.index, rects);
            this._send(`coverage ${m.index}`, `coverage ${m.index} ${coverage.toFixed(2)}`);
        }
    }

    _onRemoteHandle(handle) {
        this._handles.add(handle);
        handle.connect('stopped', () => {
            this._handles.delete(handle);
            this._updateVisibility();
        });
        this._updateVisibility();
    }
}

export default class GitsFileVisualizer extends Extension {
    enable() {
        this._wallpaper = new Wallpaper();
        this._wallpaper.enable();
    }

    disable() {
        this._wallpaper?.disable();
        this._wallpaper = null;
    }
}
