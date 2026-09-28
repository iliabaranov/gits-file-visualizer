/* gits-file-visualizer — GNOME Shell 42–44 extension.
 *
 * Launches `gits-render wallpaper` through Meta.WaylandClient (so its windows
 * can be recognised reliably and hidden from window lists), keeps those windows
 * minimized, and shows a Clutter.Clone of each one inside every background
 * actor for its monitor: desktop, overview workspaces and workspace-switch
 * animations. The lock screen keeps its normal wallpaper.
 *
 * It also tells the renderer, over its stdin, which monitors are covered by
 * windows (so it stops drawing them), when the screen is locked, and when a
 * screencast or screen share is running (so file names get obfuscated).
 */
'use strict';

const { Clutter, Gio, GLib, Meta, St } = imports.gi;
const ByteArray = imports.byteArray;
const Main = imports.ui.main;
const Background = imports.ui.background;
const Config = imports.misc.config;

const TITLE_PREFIX = 'gits-file-visualizer|';
const SHELL_MAJOR = parseInt(Config.PACKAGE_VERSION.split('.')[0]);
const FADE_MS = 900;
const RESPAWN_DELAY_MS = 3000;
const MAX_RESPAWNS = 5;
// gits-render exits with this code to ask for a restart (see app.rs EXIT_RESTART).
const EXIT_RESTART = 75;
const RESPAWN_WINDOW_US = 5 * 60 * 1000000;
const VISIBILITY_DEBOUNCE_MS = 150;
const VISIBILITY_POLL_S = 3;
// Sample grid used to decide whether a monitor's work area is fully covered.
const GRID_X = 16;
const GRID_Y = 10;

function info(msg) {
    log(`[gits] ${msg}`);
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

class RendererProcess {
    constructor(onExit) {
        this._onExit = onExit;
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
        this._client = SHELL_MAJOR >= 43
            ? Meta.WaylandClient.new(global.context, launcher)
            : Meta.WaylandClient.new(launcher);
        const proc = this._client.spawnv(global.display, argv);
        this._proc = proc;
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
            return this._client !== null && this._client.owns_window(window);
        } catch (e) {
            return false;
        }
    }

    hideFromWindowList(window) {
        this._client?.hide_from_window_list(window);
    }

    send(line) {
        if (!this._stdin)
            return;
        try {
            this._stdin.write_all(ByteArray.fromString(`${line}\n`), null);
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

class Extension {
    constructor() {
        this._renderer = null;
        this._origCreate = null;
        this._holders = new Set();
        this._sources = new Map(); // monitor index → renderer window actor
        this._windows = new Map(); // MetaWindow → signal ids (renderer windows)
        this._tracked = new Map(); // MetaWindow → signal ids (other windows)
        this._signals = [];
        this._sent = new Map();
        this._handles = new Set();
        this._timers = new Set();
        this._respawns = [];
        this._enabled = false;
    }

    enable() {
        this._enabled = true;
        this._hookBackgrounds();
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
        this._handles.clear();
        this._renderer?.stop();
        this._renderer = null;
        this._sources.clear();
        this._unhookBackgrounds();
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
            Main.overview._overview._controls._workspacesDisplay._updateWorkspacesViews();
        } catch (e) {
            info(`overview backgrounds not refreshed: ${e.message}`);
        }
    }

    _decorate(manager, bgActor) {
        const container = manager._container;
        if (container instanceof St.Widget && container.has_style_class_name('screen-shield-background'))
            return; // lock screen: keep the normal wallpaper
        // No layout manager: children keep the position/size their constraints give them.
        const holder = new Clutter.Actor({ reactive: false, opacity: 0 });
        holder.add_constraint(new Clutter.BindConstraint({ source: bgActor, coordinate: Clutter.BindCoordinate.SIZE }));
        holder._gitsMonitor = bgActor.monitor;
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
            holder.opacity = 0;
            return;
        }
        // A Clone paints its source scaled to its own allocation: the holder's size.
        const scaled = new Clutter.Clone({ source, x: 0, y: 0 });
        scaled._gitsSource = source;
        scaled.add_constraint(new Clutter.BindConstraint({ source: holder, coordinate: Clutter.BindCoordinate.SIZE }));
        holder.add_child(scaled);
        holder.ease({ opacity: 255, duration: FADE_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD });
    }

    _refill(monitor) {
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

    _onWindowCreated(window) {
        if (!this._renderer?.owns(window)) {
            this._track(window);
            return;
        }
        // Suppress the map animation: the window must never flash on screen.
        const actor = window.get_compositor_private();
        if (actor)
            Main.wm.skipNextEffect(actor);
        this._renderer.hideFromWindowList(window);
    }

    _onMap(actor) {
        const window = actor.meta_window;
        if (!this._renderer?.owns(window) || this._windows.has(window))
            return;
        const monitor = this._monitorOf(window);
        if (monitor === null || monitor >= Main.layoutManager.monitors.length) {
            info(`ignoring renderer window with title ${window.get_title()}`);
            return;
        }
        this._renderer.hideFromWindowList(window);
        window.stick();
        window.move_to_monitor(monitor);
        // The window is smaller than the monitor (the renderer's internal
        // resolution); the clones below scale it up.
        Main.wm.skipNextEffect(actor);
        window.minimize();
        const ids = [
            window.connect('notify::minimized', () => {
                if (!window.minimized) {
                    Main.wm.skipNextEffect(actor);
                    window.minimize();
                }
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

    _forgetWindow(window, monitor) {
        this._windows.get(window)?.forEach(id => window.disconnect(id));
        this._windows.delete(window);
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
        if (!this._enabled || status === 'exit 0')
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

    _covered(monitorIndex, rects) {
        const a = Main.layoutManager.getWorkAreaForMonitor(monitorIndex);
        const inside = (x, y) => rects.some(r => x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
        for (let j = 0; j < GRID_Y; j++) {
            for (let i = 0; i < GRID_X; i++) {
                const x = a.x + Math.floor((i + 0.5) * a.width / GRID_X);
                const y = a.y + Math.floor((j + 0.5) * a.height / GRID_Y);
                if (!inside(x, y))
                    return false;
            }
        }
        return true;
    }

    _send(key, line) {
        if (this._sent.get(key) === line)
            return;
        this._sent.set(key, line);
        this._renderer?.send(line);
    }

    _updateVisibility() {
        if (!this._renderer?.running)
            return;
        const locked = Main.sessionMode.isLocked;
        this._send('locked', `locked ${locked ? 1 : 0}`);
        this._send('sharing', `sharing ${this._handles.size > 0 ? 1 : 0}`);
        const overview = Main.overview.visible;
        const rects = overview ? [] : this._occluders();
        for (const m of Main.layoutManager.monitors) {
            const visible = overview || !this._covered(m.index, rects);
            const key = `visible ${m.index}`;
            const line = `${key} ${visible ? 1 : 0}`;
            if (this._sent.has(key) && this._sent.get(key) !== line)
                info(`monitor ${m.index} ${visible ? 'visible' : 'covered'} (${rects.length} windows)`);
            this._send(key, line);
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

/* exported init */
function init() {
    return new Extension();
}
