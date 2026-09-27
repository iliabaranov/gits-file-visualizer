# gits-file-visualizer — Design (DRAFT v0)

Status: planning. Nothing here is final; see **Open questions** at the bottom.

## 1. Goal

A live desktop wallpaper for Ubuntu 22.04 / 24.04 / 26.04 that renders the
filesystem as a glowing gold/amber 3D cylinder, in the style of the LOCUS SOLUS
security-system hack in *Ghost in the Shell 2: Innocence* (2004). Files are
segments of the cylinder; access, modification, creation and deletion are shown
as visual events in real time.

Hard requirements:

- Runs *as the desktop background* (under all windows, on every workspace).
- Efficient enough to leave on all day on a laptop: near-zero cost when not visible.
- Works on GNOME 42 (22.04), GNOME 46 (24.04), GNOME 50 (26.04, Wayland-only).

## 2. Aesthetic reference

*Innocence* deliberately switched the franchise's computer graphics from the
first film's green to a warm amber/gold palette. Target look:

- Translucent, stacked, segmented rings forming a tall cylinder; additive glow.
- Palette: near-black warm background → deep bronze → amber → gold → white-hot
  for peak activity. Intrusion/deletion accents in red-orange.
- Thin wireframe edges, scan-line sweeps travelling along the axis, depth fog,
  fine glyph/HUD text (hex, paths, status strings) floating near activity.
- Slow, drifting "virtual camera" (Oshii's gliding camera), never jerky.

> TODO: collect 3–6 reference frames of the actual scene into `docs/ref/`
> (timestamped) so look-dev is matched against the real thing, not memory.

## 3. Platform constraints (the hard part)

| Ubuntu | GNOME | Session | Extension API |
|--------|-------|---------|---------------|
| 22.04  | 42    | Wayland (default) or X11 | legacy `imports.*` |
| 24.04  | 46    | Wayland (default) or X11 | ES modules (45+) |
| 26.04  | 50    | **Wayland only** | ES modules |

- GNOME/Mutter does **not** implement `wlr-layer-shell`, so a regular app cannot
  place itself as a background layer on Wayland.
- X11 tricks (`xwinwrap`, `_NET_WM_WINDOW_TYPE_DESKTOP`) stop working under
  Wayland, and 26.04 no longer has a GNOME-on-X11 session at all.
- ⇒ A **GNOME Shell extension is required** for embedding. Prior art:
  [Hanabi](https://github.com/jeffshee/gnome-ext-hanabi): a separate renderer
  process opens a normal window, and the extension picks up that window's actor
  and moves it into the background layer on each monitor, hiding it from alt-tab,
  the overview and the dock. Hanabi keeps separate branches for GNOME 42–44,
  45–50, and 50+. We will need at least two extension builds (42–44 and 45+).

Rejected: rendering inside gnome-shell itself (Clutter shaders / GJS). It would
have the lowest overhead, but any bug or GPU stall freezes or crashes the whole
compositor, and on Wayland that takes the entire session down with it.

## 4. Architecture (proposed)

```
┌───────────────────────┐   unix socket    ┌────────────────────────┐
│ gits-collector (Rust) │ ───────────────▶ │ gits-render (Rust)     │
│  user systemd service │  snapshot +      │  wgpu, one window per  │
│  scan + fs events     │  event stream    │  monitor (or shared)   │
└──────────┬────────────┘                  └──────────▲─────────────┘
           │ optional                                 │ D-Bus: visible/
┌──────────▼────────────┐                  ┌──────────┴─────────────┐
│ gits-fanotify-helper  │                  │ GNOME Shell extension  │
│  root, whole-FS events│                  │  embed as wallpaper,   │
└───────────────────────┘                  │  occlusion/lock signals│
                                           └────────────────────────┘
```

- **gits-collector**: initial scan with `statx` (size, atime, mtime, ctime,
  btime, mode, uid). It then watches for live changes and keeps a compact model
  in memory (~40 B/node), with a snapshot saved to `~/.cache` so restarts are
  instant. Changes are batched into updates at up to 10 Hz. The scan runs at
  `nice`/`ionice` idle priority.
- **gits-render**: Rust + `wgpu` (Vulkan with a GL fallback), `winit` window. All
  geometry sits on the GPU. Each file stores a small record there (arc position,
  ring, size, last-access time, last-modify time, flags), and the shaders animate
  everything from a single `now` value. The CPU does almost nothing per frame.
- **Extension**: embeds the renderer windows as the wallpaper and forwards
  "fully covered / fullscreen app / screen locked / overview open" to the
  renderer so it can stop drawing.
- The renderer and collector are separate processes so that the renderer can be
  stopped when hidden, and so that a renderer crash never loses the model.

## 5. Visual mapping (proposal — needs your input)

**Layout A: cylindrical sunburst (default proposal)**
- The cylinder's axis is directory depth: the top ring holds the root's
  children, and each ring below subdivides its parent's arc.
- Angular width is proportional to size (log-scaled so tiny files stay visible),
  and a directory's subtree is one contiguous arc. Positions stay stable over
  time so you can learn "where" things are.
- Radial extrusion (how far the segment sticks out) ∝ size; brightness ∝ how
  recently the file was touched (exponential decay).
- Beyond a configurable depth, or below a size threshold, files are merged into
  aggregate "cluster" segments, which caps the segment count.

**Layout B: time cylinder / waterfall**
- The top band is "now". Every event leaves a mark at the file's angular
  position, and marks scroll downward and fade, so the cylinder becomes a
  history tape of disk activity.

A hybrid is possible: layout A on top with a short layout-B history trail below it.

**Event language (both layouts)**

| Event | Visual |
|-------|--------|
| read/open | brief amber pulse, glow decays over minutes |
| modify/write | white-hot flash + ripple along the ring |
| create | segment "scans in" as wireframe, then fills |
| delete | turns red-orange, fractures, particles drift up; dark "scar" gap fades over hours |
| move/rename | segment detaches and slides along an arc to its new slot |
| burst (many events) | intrusion-style tendril/beam piercing multiple rings |

Static attributes: file type gives a subtle hue shift within gold (or a glyph
texture); root-owned or read-only files get a hatched pattern; hidden files are
dimmer.

## 6. Efficiency plan

- **Occlusion-aware:** render 0 fps when the monitor is covered, a fullscreen
  app is running, the screen is locked, or the display has blanked. The
  extension knows window geometry, so this is cheap to detect.
- **Adaptive rate:** ~24–30 fps while visible and active, easing down to ~5 fps
  (or holding a static frame) when nothing is happening. Lower cap on battery.
- **Resolution:** render internally at 0.5–0.75× scale on 4K and upscale; bloom
  at quarter resolution. Most of the visual is glow, which hides upscaling well.
- **GPU choice:** request the low-power adapter so hybrid laptops use the iGPU
  and never wake the NVIDIA dGPU.
- **Rendering:** instanced draws, a few draw calls per frame, a fixed segment
  budget (e.g. ≤100k) with level of detail.
- **Budgets to verify:** idle-visible <2% of one core, hidden ≈0%, RSS <150 MB
  total, collector idle ≈0% CPU.

## 7. Data-source realities

- **atime is unreliable.** Ubuntu mounts ext4 with `relatime`, so atime only
  updates if it is older than mtime/ctime or more than 24 h old. Live events are
  the real source of "access" visuals; stat atime is only a rough historical hint.
- **inotify** (unprivileged) needs one watch per directory. This machine's limit
  is `max_user_watches = 65536`, and `$HOME` can exceed that (node_modules,
  .cache, build trees). That means default excludes, or a raised sysctl.
  inotify does report `IN_ACCESS`/`IN_OPEN`/`IN_MODIFY`/create/delete/move per
  watched directory.
- **fanotify** with `FAN_MARK_FILESYSTEM` + `FAN_REPORT_DFID_NAME` (kernel ≥5.9;
  22.04 ships 5.15/6.8, 26.04 ships 7.0) gives whole-filesystem events without
  per-directory watches, but requires `CAP_SYS_ADMIN`, i.e. a small root helper
  service. It also reports the PID, so events could be attributed to processes.
- Birth time is available via `statx` on ext4/btrfs.

## 8. Privacy

A wallpaper that shows real file paths will show them during screen sharing and
in screenshots. Options: show real names, obfuscate (hash glyphs), show
directory names only, or hide text automatically while screen sharing is active
(GNOME exposes screencast state to extensions).

## 9. Milestones (proposed)

1. **M0 look-dev:** standalone windowed renderer driven by synthetic data. Nail
   the aesthetic before touching the filesystem.
2. **M1 collector:** real `$HOME` scan, inotify events, snapshot cache, IPC.
3. **M2 embed:** GNOME 42 extension on this machine (Wayland, 3 monitors, incl.
   portrait).
4. **M3 efficiency:** occlusion pause, adaptive fps, iGPU selection, measured
   budgets.
5. **M4 portability:** GNOME 46/50 extension build, test in 24.04/26.04 VMs,
   install script or `.deb`, user systemd units.

## 10. Open questions

See the Q&A log appended below once answered.
