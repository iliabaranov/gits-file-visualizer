# gits-file-visualizer — Design (v3)

Status: M0–M3 done, M4 (compositions) mostly done; installed on the development machine. Decided items are marked ✅.

## 1. Goal

A live desktop wallpaper for Ubuntu 22.04 / 24.04 / 26.04 that renders the
filesystem as the glowing security-system tower from the LOCUS SOLUS hack in
*Ghost in the Shell 2: Innocence* (2004). Files are arc segments of stacked
rings; reads, writes, creation and deletion drive the animation in real time.

Priority order: **looks like the film > peaceful > efficient > informative.**
It is a wallpaper: it should be calm to have behind your work all day, with the
motion coming from the data, not from the camera.

## 2. Decisions

| Topic | Decision |
|-------|----------|
| Reference | ✅ Togusa hacking the LOCUS SOLUS ship (YouTube `idEMI562YM0`, "Ghost in the Shell [Hacking Scene]") |
| Scope | ✅ `$HOME` first (unprivileged inotify); pipeline designed so a root fanotify helper can be added later |
| Layout | ✅ One tower, drum ↔ coil states, a fixed composition per monitor (§5) |
| Camera | ✅ Largely static and peaceful: fixed composition, at most imperceptible drift, no cuts by default |
| Colours | ✅ Crimson for writes (not only deletes), cream-white for reads. All colours and decay times are configurable (§8) |
| Alerts | ✅ Escalation only raises intensity, then everything simply fades back. Nothing latches, no acknowledgement |
| Calligraphy | ✅ Zeami verse columns as a dim background layer (on by default, configurable) |
| Monitors | ✅ One view per monitor: shared data, own composition per monitor |
| Text | ✅ Real paths; optionally obfuscated while screen sharing (`obfuscate_when_sharing`, off by default) |
| Performance | ✅ Up to 24 fps (film rate), much lower when calm, ≈0 when hidden, iGPU preferred |
| Interaction | ✅ Passive only |
| Desktops | ✅ GNOME first, behind a small surface-backend interface; layer-shell backend optional later (§4.1) |
| Stack | ✅ Rust + wgpu |

## 3. Visual reference: what the scene actually shows

Reference frames are in `docs/ref/` (low resolution, committed to this private
repo so look-dev works on any machine). They come from YouTube storyboard sprites of the clip plus one
still of the ship sequence.

1. **The drum** (`hack-keyframes.jpg`, panels 2, 5, 6). A massive glowing
   amber cylinder made of thousands of *fine horizontal ring-lines* stacked so
   tightly they merge into a surface. Micro-text texture, grain and sparkle,
   darker vertical banding. Overexposed, nearly solid gold. This is the intact
   system seen up close.
2. **The coil** (`hack-tower-wide.jpg`, panels 13–15). The same tower opened
   up. Rings are separated, and each ring is broken into **arc segments of
   varying length with gaps**. Colours: pale cream/white, gold, orange and
   **crimson red**, with some arcs dashed or dotted. **Tall thin white vertical
   cards** stick to the surface like index tabs or labels. Arcs appear to spin
   at different rates, smeared by motion blur.
3. **The chamber.** The tower stands in a dark room. A **teal/cyan lattice
   torus** of points and lines arcs around the base, and cyan point lights sit
   on the floor. Teal is the complementary environment colour; the data itself
   is never teal.
4. **Segmented bands** (panel 3). A thick gold ring band with dark rectangular
   blocks in it, like a film strip or a disk sector map, plus horizontal bars of
   different lengths.
5. **The dial** (panels 4, 7). Concentric circles seen face-on: **text set
   along the arcs**, tick marks, block rectangles. Leader lines run out to labels
   (`INSTA…`), and a central text box reads like `TRANSMITTING … TYPE …`.
   Martial-arts program names (`MARTIAL ARTS`, `ATEMI`) are bent around rings as
   the combat software loads.
6. **The probe** (panels 8, 11). A small **wireframe gimbal sphere** (nested
   wire circles) rides along the ribbons, together with **white and red
   triangular arrowheads**. This is the intruder / virus / counter-virus.
7. **The vortex** (panels 10–12). The camera flies *through* sweeping ribbons,
   following the probe.
8. **Calligraphy walls** (`ship-sequence-zeami-verse.jpg`, from the ship
   interior, not the hack itself). Gold vertical columns of Zeami's verse
   生死去来 棚頭傀儡 一線断時 落落磊磊: "Life and death come and go / like
   marionettes dancing on a table. / Once their strings are cut, / they easily
   collapse". Possible optional backdrop.

**Palette:** warm black; amber-gold (main); cream-white (hottest); orange;
crimson (alarm/infected); teal/cyan (environment only). Heavy bloom/halation,
film grain, and motion smear on everything that moves.

The sequence shows the **same object at different states and camera
distances**: a dense gold drum when intact, an open multicoloured coil when
under attack, concentric dials when seen from above. That shapes the design
below.

## 4. Platform

| Ubuntu | GNOME | Session | Extension API |
|--------|-------|---------|---------------|
| 22.04  | 42    | Wayland (default) or X11 | legacy `imports.*` |
| 24.04  | 46    | Wayland (default) or X11 | ES modules (45+) |
| 26.04  | 50    | **Wayland only** | ES modules |

GNOME has no `wlr-layer-shell`, and X11 desktop-window tricks do not work on
Wayland (26.04 has no GNOME-on-X11 at all), so a GNOME Shell extension is
required to embed the renderer as the wallpaper. Prior art:
[Hanabi](https://github.com/jeffshee/gnome-ext-hanabi). Rendering inside
gnome-shell itself is rejected: a stall there freezes the whole session.

### 4.1 Desktop support ✅ GNOME first, layer-shell later

| | GNOME extension backend | layer-shell backend |
|---|---|---|
| Covers | Ubuntu default on 22.04/24.04/26.04 | Kubuntu (Plasma 6), COSMIC, Sway/Hyprland/niri |
| Mechanism | Extension moves our window's actor into the background group | Standard protocol: `background` layer surface per output |
| Robustness | API breaks between GNOME versions (42–44 legacy, 45+ ESM); runs inside gnome-shell | Stable protocol, no compositor code |
| Occlusion / lock / overview signals | Full (extension sees window geometry) | Weak: frame-callback throttling, varies per compositor |
| Screen-share detection (privacy) | Yes | Per-DE, often unavailable |
| Rust plumbing | `winit` toplevel window | `smithay-client-toolkit` + raw wgpu surface |
| Extra test burden | 3 GNOME versions (pinned by LTS) | 1–2 more environments |

Decision: a small `SurfaceBackend` interface from day one, with GNOME
implemented first. The GNOME backend's extra signals (visibility, lock,
screencast) are optional capabilities, so other backends can report "unknown".
layer-shell is an optional later milestone. Universal fallback for any desktop:
render a still frame every N seconds and set it as the normal wallpaper.

## 5. Visual concept: one tower, still compositions

Instead of separate modes with separate geometry there is **one scene**: the
tower in its chamber. It has **two states** (drum ↔ coil) and several
**compositions** (camera framings taken from the film's shots). Each monitor
holds one composition, like a painting. **The camera stays essentially still**:
all motion comes from the data itself (spinning rings, unwinding subtrees,
probes, falling flakes). This matches the film, keeps a single dataset on the
GPU, and is both peaceful and cheap.

### 5.1 Data → tower mapping

- **Ring = a directory's row of files.** The **segments** on a ring are its
  files. **Arc length ∝ log(size)**, with small gaps at file boundaries. A
  directory with many files wraps onto several stacked rings, forming a *band*.
- **Vertical order = depth-first path order**, so every subtree is a
  contiguous stretch of the tower. `~` is at the top.
- **Radius grows with depth**, like the ring chart of a disk-usage analyzer
  (Filelight, Baobab): `~`'s own files sit nearest the axis and each folder
  level down is a shell further out (`inner_radius` → `radius` over
  `depth_layers` shells; deeper folders share the outermost). From the side the
  tower becomes stepped shells with see-through parallax; the dial view down
  the axis reads as concentric rings. (Angular sectors per folder, as in a
  sunburst, are not done: a folder's rings still go all the way round.)
- Each ring spins slowly at its own rate (seeded by a hash, alternating
  direction) and speeds up gently when touched. Spin rates are kept slow and
  eased so the tower "breathes" rather than whirls.

### 5.2 Drum ↔ coil (activity = the hack)

- **Idle subtrees are "wound":** their rings sit packed and merge into the
  dense, glowing gold drum surface. (In the renderer this is effectively free;
  see §7.)
- **When activity hits a subtree it "unwinds":** its rings separate vertically,
  break into visible arc segments, spin up and take on event colours. White
  cards pop out carrying file names, and the probe rides out along the arcs to
  the touched file.
- After a quiet period the subtree **winds back** into the drum.
- The result: a calm machine looks like the intact gold drum, and a busy one
  looks like the breached, multicoloured coil from the wide shot.

### 5.3 Event colours and motions ✅ (defaults; all configurable)

| Event | Default look |
|-------|--------------|
| idle, by mtime age | gold (recent) → amber/orange → dim bronze (ancient) |
| read/open | segment brightens to **cream-white**; the probe glides to it; afterglow decays over minutes |
| write/modify | segment turns **crimson** ("altered"), and the ring spins up gently; decays back to gold over ~15 min |
| create | a new segment **grows along the arc** from zero length with a bright leading tip |
| delete | segment goes crimson; its "string is cut": it detaches, drifts down, and breaks into slowly falling gold flakes. The gap stays as a faint crimson ghost for a few hours |
| move/rename | segment slides along the tower to its new ring |
| symlink / aggregated remainder | dashed or dotted arcs |
| a burst in one subtree | white/red **arrowheads** drift toward it; a HUD line such as `SECURITY WALL 014 BREACHED` fades in and out |

Mapping *write* to crimson (not just delete) matters for the look: the film's
wide shot is full of red, so a busy work session should look like that, while
an idle machine stays gold.

**Peacefulness rules** (apply to every effect):
- Soft attack (≥150 ms fade-in) and long exponential release; no hard strobes.
- Global flash-rate limiter: no region brightens more than ~2×/s, which keeps
  it photosensitivity-safe even during a big build.
- Brightness ceiling and bloom are tuned so a crimson storm reads as "warm
  glow", never glare.

### 5.4 Compositions (fixed per monitor)

| Composition | From the film | Notes |
|-------------|---------------|-------|
| **Wide tower** | thumbnail frame | Tower centred in its chamber with the teal lattice and floor lights. Default for the 4K monitor |
| **Drum close-up** | panels 2, 5, 6 | Near the dense gold surface; micro file-name text. Default for the portrait monitor |
| **Dial** | panels 4, 7 | Looking down the axis, so the rings become concentric circles with names set along the arcs and a small event log in the security-system voice. Default for 1920×1200 |
| **Band** | panel 3 | Face-on crop of the segmented ring band of the most active directory |

Camera behaviour:
- **Default: completely still.** Optional *drift* (off by default, configurable)
  of at most a few degrees of orbit or parallax over many minutes. That is slow
  enough to be imperceptible from moment to moment, and it also avoids OLED
  burn-in.
- **No automatic cuts** by default. An optional slow rotation of compositions
  (e.g. every few hours, 10 s cross-dissolve) can be enabled in config.
- The film's **Vortex** fly-through is not used as a camera move. Its energy
  shows up in the data instead (probe and arrowheads travelling along the
  ribbons).

### 5.5 Activity levels (intensity only, always fades)

1. **Calm:** gold drum, gentle ring spin, a rare slow scan-line sweep.
2. **Activity:** subtrees unwind; cards, probe and HUD fragments appear.
3. **Surge** (mass delete, huge write burst): more crimson, arrowheads,
   security-voice HUD lines (`REBUILDING LEVEL 1 EXTERNAL SECURITY WALL`).

Levels rise and fall smoothly with a moving average of event rates. Nothing
latches and nothing needs acknowledging: once activity stops, the scene fades
back to the calm drum over a few minutes.

### 5.6 Background & texture

- **Zeami calligraphy columns** (生死去来 棚頭傀儡 一線断時 落落磊磊) as a dim,
  far, slightly defocused layer on the chamber walls. On by default, with
  configurable brightness.
- HUD/glyph layers animate "on twos" (12 fps), a nod to Innocence's
  step-printed ghosts. It also halves their cost.
- Fine film grain and gentle halation; no aggressive scanlines.

## 6. Scale

Measured on this machine: `$HOME` = **~134k directories, ~1.24M files**.

- inotify needs one watch per directory, but `max_user_watches` is **65536**.
  Default excludes (`.cache`, `node_modules`, `target`, `.git/objects`,
  `build`, `esp-idf`, snap/flatpak data, …) are required, plus an optional
  sysctl bump. The later fanotify helper removes the limit entirely.
- Ring budget: cold subtrees are aggregated into a single ring. Hot subtrees
  expand to real files. Target ≤16k rings and ≤1–2M segments in the drum
  texture, of which ≤2k rings are unwound as real geometry at once.

## 7. Rendering architecture (why this is cheap)

- **The drum is one shader, not geometry.** A few concentric cylinder meshes
  (one per depth step) sample a **ring atlas texture**: rows = rings, columns =
  angle, and each texel holds a segment id. The segment id looks up a per-file
  GPU record (size, event timestamps, flags). The fragment shader draws
  anti-aliased lines, gaps, dashes, colour and decay from a single `now`
  uniform, and per-ring spin is just an angle offset. Millions of segments cost
  a handful of draw calls. Pre-filtered atlas mips make distant rings merge into
  the glowing drum surface, which is exactly the film's look, instead of
  shimmering.
- **Unwound rings** (≤2k) are instanced ribbon geometry, with cards, probe,
  arrowheads and particles on top.
- **Post:** motion-smear feedback buffer (previous frame × decay), which
  produces the film's streaky arcs for one extra pass. Quarter-res dual-Kawase
  bloom, grain, vignette.
- **Text:** SDF/MSDF glyph atlas (Latin + CJK subset), only for cards, dial
  labels and HUD.
- **Static layers are cached.** Because the camera is still, the chamber, teal
  lattice, floor lights and calligraphy wall are rendered once per composition
  (or on resize) into a background texture. Each frame only redraws the tower
  and its effects on top.
- **One process, one GPU device, one surface per monitor**, with all buffers
  shared. Only composition and HUD differ per monitor.
- **CPU per frame ≈ 0:** the collector sends deltas at ≤10 Hz, and the renderer
  patches buffer and texture rows.

## 8. Efficiency plan

- 0 fps when a monitor is covered, a fullscreen app is running, the screen is
  locked, or the display is blanked.
- Frame rate follows activity, not the camera (the camera is still):
  ~6–12 fps when calm (slow ring spin and the motion-smear buffer hide it),
  up to 24 fps during activity. Lower caps on battery.
- Static background layers are cached (see §7), so a calm frame is mostly one
  cylinder shader plus bloom at reduced resolution.
- Internal resolution 0.5–0.75× on 4K, upscaled (glow hides it).
- Low-power adapter so the Intel iGPU is used and the NVIDIA dGPU stays asleep.
- Budgets: visible <2% of one core, hidden ≈0%, RSS <150 MB total. Measured
  in M3.

## 9. Configuration

One TOML file. [`config/default.toml`](../config/default.toml) is compiled into
the binary and is the single source of defaults and documentation. A user
file (`~/.config/gits-file-visualizer/config.toml` or `--config`) is deep-merged
over it, so it only needs the keys being changed. Unknown keys are rejected.
The renderer hot-reloads the file; `[synthetic]` changes need a restart.
Sections: `palette`, `events.{read,write,create,delete}`, `age`, `calm`,
`tower`, `background`, `camera`, `monitors."<connector>"`, `performance`,
`privacy`, `collector`, `synthetic`.

## 10. Components

```
┌───────────────────────┐   unix socket    ┌────────────────────────┐
│ gits-collector (Rust) │ ───────────────▶ │ gits-render (Rust)     │
│  user systemd service │  snapshot +      │  wgpu, 1 device,       │
│  statx scan, inotify  │  delta stream    │  1 surface per monitor │
└──────────┬────────────┘                  └──────────▲─────────────┘
           │ later                                    │ D-Bus: visibility,
┌──────────▼────────────┐                  ┌──────────┴─────────────┐
│ gits-fanotify-helper  │                  │ GNOME Shell extension  │
│  root, whole-FS + PID │                  │ (or layer-shell later) │
└───────────────────────┘                  └────────────────────────┘
```

- **Collector:** `statx` scan at idle I/O priority. Keeps a compact tree with a
  snapshot in `~/.cache` for instant restarts, and emits a stable depth-first
  ring layout with incremental updates.
- **Extension:** embeds the renderer windows, and reports visibility, lock
  state, overview state and screencast state.
- Note: stat atime is coarse because of `relatime`. Access visuals come from
  live events.

## 11. Progress notes

### M0 — done (2026-09-27)

- Workspace: `gits-core` (model, layout, config, synthetic data) and
  `gits-render` (wgpu 30 + winit 0.30). Rust toolchain pinned. Fonts
  bundled as subsets, defaults compiled in, reference frames in `docs/ref/`.
  Nothing depends on the host beyond the GPU driver and the display server.
- Implemented: drum shader over a segment-id atlas + LUT; unwinding driven by
  per-directory activity (capped at 5 ring pitches per directory and 35% of the
  tower height); event colours with soft attack and a flash-rate limit;
  unwound rings tilt slightly and show more cream "scanned" arcs; cards +
  labels, probe, falling segments → gold flakes; cached chamber (lattice, floor
  lights, dim Zeami calligraphy); motion smear, bloom, hue-preserving tonemap,
  grain and vignette; config hot reload; headless `snapshot` and `bench`.
- Look-dev findings that changed the design:
  - Files need long arcs (0.035–0.42 turns); short dashes read as noise.
  - The camera must look down about 23° so rings read as ellipses.
  - A per-channel tonemap bleached gold to yellow and crimson to pink. The
    tonemap is now applied to the brightest channel so hues are kept.
  - Unwound ribbons must be thin (22% of pitch) with a soft halo, otherwise
    they read as bricks.
- Measured on this laptop (Intel UHD 630 iGPU, Mesa Vulkan), per frame, with
  internal resolution capped at 1.6 MP:

  | Output | GPU+CPU | GPU busy at 24 fps / 8 fps |
  |--------|---------|----------------------------|
  | 1920×1080 | 8.1 ms | 19% / 6% |
  | 3840×2160 | 15.6 ms | 37% / 12% |
  | 1440×2560 portrait | 23.5 ms | 56% / 19% |

  CPU is about 1 ms/frame. The window holds 24 fps. Before the sample-count
  and resolution-cap fixes, 4K took 40 ms.

### After first hands-on session (2026-09-27)

- Defaults now follow Ilia's tuning: tower radius 3.75, ring spin 0.5 rev/min,
  up to 50 directories unwound, lattice brightness 0.15, synthetic seed 14 at 2×
  speed. The chamber (lattice, floor lights, floor ring) now scales with the tower
  radius.
- The wireframe probe is off by default (`events.read.probe`): too distracting.
- Synthetic mode loops seamlessly (`[synthetic] loop`, default 10 simulated
  minutes). Near the end of each loop:
  1. New events stop and all highlights fade out (60 s).
  2. Busy directories wind back.
  3. Files created during the loop shrink back along their arcs, deleted files
     grow back into their slots, and changed ages blend back (staggered over 15 s).
  4. Rings that only held created files ease out.
  5. The model and session reset while nothing on screen changes.
  The session runs on its own clock, so every loop replays the same story.
  Verified with frame-to-frame differences across the restart: no spikes at
  restore or reset. There is a brief drum shimmer while retiring rings ease out
  (see backlog).
- The window simulates in fixed 1/24 s steps, identical to headless snapshots.

### M1 — done (2026-09-27)

- `gits-collector` (new crate) is a separate process, installed as a systemd
  user service (`packaging/systemd`, `tools/install-user.sh`). It scans the
  root at idle CPU/IO priority, watches with inotify within a budget, and
  serves `$XDG_RUNTIME_DIR/gits-file-visualizer/collector.sock` (mode 0600).
  Extra tools: `scan` (counts + budget) and `monitor` (prints the live stream).
- Protocol (`gits-core::protocol`): length-prefixed binary frames — Hello,
  Status, one Snapshot, then Ops batches every 100 ms. Ops (MkDir, RmDir,
  Create, Delete, Read, Write) are applied identically on both sides, so ids
  never need to be sent on creation. Hand-written encoding, no new dependencies.
  The renderer decodes snapshots on a client thread, reconnects automatically,
  and gets a fresh snapshot after a collector restart. Directory keys are
  derived from paths, so ring animation state survives reconnects.
- Watch limit kept at the Ubuntu default and documented in README
  "Watch limit". The budget is half of `max_user_watches` (leaving at least
  4096 free). The top levels and the most recently changed folders are
  watched; the rest are crawled every `crawl_interval`, and a folder where the
  crawl finds changes is promoted to a watch.
- Default excludes grew after measuring this home: caches, build output,
  package stores (`~/.npm`, `~/.cargo/registry`, `~/.arduino15`, …),
  containers and app sandboxes. Result: 134k → 52k folders, 1.24M → 360k files.
  The scan takes 2.1–2.7 s warm, so the planned on-disk snapshot cache is deferred.
- Layout for large trees: the old depth-based collapse was replaced by greedy
  expansion within the ring budget, prioritised by current activity, then by
  the newest change anywhere below. Unexpanded folders are single dashed
  aggregate segments. Busy collapsed folders unfold at the next layout (at
  most every 5 s live). Real home: 15.7k rings, 86k segments, ~6.3k folders
  expanded, ~3k aggregates; a rebuild takes 25–70 ms. The segment atlas is now
  8-bit (index within the ring) instead of 32-bit.
- Live observations: steady background of ~40 ops/s on this machine (the
  `pstop_soak_8h` soak test writes ~15/s; Chrome IndexedDB). An ESP-IDF build
  peaks around 300 ops/s (header reads under `esp-idf-5.5`). Both render well,
  and the build unwinding into the coil is the look we want.
- Costs with the real tree (this laptop, iGPU): 14.3 ms/frame at 1080p and
  24.7 ms at 4K (vs 8 / 16 ms synthetic, since there are twice the rings).
  RSS: renderer 220 MB (170 MB even on synthetic data), collector ~100 MB.
  Both are over the 150 MB total budget; see the backlog.
- Hidden windows: GNOME stops frame callbacks while the window cannot be seen
  (locked screen, other workspace, and later the covered wallpaper). The
  renderer now keeps applying collector updates once a second without drawing,
  so nothing piles up and changes have faded by the time the window is visible
  again. Layout rebuilds wait for the next visible frame. Measured with the
  screen locked: renderer 0.05% of a core (2% before deferring rebuilds),
  collector ~0.7% at ~40 ops/s.
- Not yet: multiple roots (only the first is used), moved directories are
  handled as delete + create, and there is no process attribution (needs the
  later fanotify helper).

### M2 GNOME 45+ port — `extension/gnome-45` (2026-09-29)

- Same design as `extension/gnome-42`, as an ES-module extension (GNOME 45–46,
  tested on 46 in the sandbox). Differences found on Mutter 46:
  - `hide_from_window_list()` makes `can_minimize()` false, so a hidden window
    silently ignores `minimize()`. The extension hides the window at creation,
    then at map time shows it in the list, minimizes it and hides it again.
  - a minimized renderer window's actor stays visible and paints at its own
    size over the wallpaper. The extension hides the actor itself (and
    re-hides it if shown); the clones still paint a hidden source.
  - X11 sessions: `Meta.WaylandClient` does not exist there, so the extension
    launches the renderer as a plain subprocess and recognises its windows by
    pid. The renderer creates override-redirect windows on X11 (invisible to
    the window manager: no dock or alt-tab entry, no focus, cannot be
    minimized), physical-pixel sized; the extension hides their actors and shows
    them only through the background clones. Frames are paced by the renderer's
    timer (X11 has no frame callbacks; `pre_present_notify` is a no-op).
    Tested on a nested `gnome-shell --x11` running in a rootful Xwayland (needs
    DRI3 for Vulkan; Xephyr/Xvfb have none) inside the headless sandbox.
- `tools/sandbox-shell.sh` picks the extension and test helper by shell version.
- X11 hardening (after the wallpaper was seen drawn over windows): the
  extension hides any override-redirect window that looks like the renderer's
  (title prefix + WM_CLASS), whether or not the pid matches, on creation, on map,
  at enable and every 3 s, and it no longer skips hiding a window it cannot map
  to a monitor. The renderer also places its X11 windows at x = -30000, so a
  visible actor could never cover the desktop or catch input.
- An output that is halted (locked, or unplugged with `pause_on_battery`) still
  draws one frame after start, resize or a lost surface (`Output::drawn`), so a
  renderer restarted on battery (e.g. by unplugging a dock, which also changes
  the monitors) leaves the last frame up instead of an empty black window. The
  extension's own restarts (`monitors changed`) no longer count as failures.
  `GITS_FORCE_BATTERY=1|0` overrides the power supply for testing.
  A paused output first keeps drawing until the tower reflects the data
  (`World::is_settled`) plus 4 s for rings to grow in (30 s at most), otherwise a
  restart on battery froze on a frame drawn before the collector's snapshot had
  arrived: background only, no rings. Power detection ignores `*source*` USB-C
  supplies (a port powering a device) and `scope = Device` batteries (a mouse),
  and is polled every 2 s.
- Screensaver and lock screen (`extension/gnome-45` only). GNOME has no
  screensaver: at `idle-delay` it fades to black, locks, and gsd-power blanks the
  display the instant the shield reports `ActiveChanged(true)` (idle inhibitors
  do not matter once the shield is active, see `idle_configure`). A user lock
  (Super+L) also slides the shield in and fades to black (`_shortLightbox`), so
  the lock screen is normally only seen after the next keypress. The extension
  therefore (1) decorates the lock screen's `BackgroundManager` too and removes
  its heavy `BlurEffect` (restored on disable); (2) wraps the shield D-Bus
  object's `emit_signal` to hold `ActiveChanged(true)` until the core idle time
  reaches `idle-delay` (released by an idle watch if the lock came first); (3)
  wraps `ScreenShield._activateFade` to skip the short black fade before that,
  completing the lock via `_completeLockScreenShown`; (4) after `[screensaver]
  after` idle (core idle monitor; skipped while `InhibitedActions` has the idle
  bit) puts a black actor plus a clone of each monitor's renderer window at the
  top of `Main.uiGroup`, removed on input or when the shield activates. The
  renderer's `locked` control now means only "display blanked" (idle past
  `idle-delay`); the extension reports zero window coverage while the overview,
  screensaver or lock screen is up. The extension reads only `[screensaver]` from
  config.toml with a small line parser; the renderer just validates the section.
  Tested in the headless sandbox with a virtual pointer (`imports.gi.Clutter`
  seat) and `dbus-monitor` on `org.gnome.ScreenSaver`. ES modules stay cached
  across disable/enable, so extension edits need a shell restart to test.
- `[performance] integrated_native_window` and `pause_on_battery` (see README,
  quality knobs); the power source is checked every 5 s.

### M2 — done for GNOME 42–44 (2026-09-27)

- `extension/gnome-42` (legacy `imports`, GJS). The approach follows prior
  art (Hanabi, Ubuntu's DING), but the code is our own:
  - spawns `gits-render wallpaper --output INDEX:WxH:CONNECTOR …` through
    `Meta.WaylandClient`, so `owns_window()` identifies our windows reliably and
    `hide_from_window_list()` keeps them out of the dock and alt-tab;
  - keeps the renderer's windows sticky, fullscreen on their monitor and
    minimized (map and minimize animations suppressed with
    `Main.wm.skipNextEffect`); minimized Wayland windows still receive frame
    callbacks, as with Hanabi;
  - wraps `BackgroundManager._createBackgroundActor`: every background actor
    for a monitor (desktop, overview workspaces, workspace-switch animation)
    gets a `Clutter.Clone` of that monitor's renderer window, fading in over the
    normal wallpaper. Lock-screen backgrounds are skipped. On disable the hook
    is restored and backgrounds are rebuilt;
  - visibility: a monitor counts as covered when a 16×10 sample grid of its work
    area is inside normal, opaque, unminimized windows on the active workspace.
    It is recomputed on restack/move/resize/minimize/workspace/overview/session
    changes (debounced 150 ms) and polled every 3 s. It sends `visible N 0|1`,
    `locked 0|1` and `sharing 0|1` (from the remote-access controller) to the
    renderer's stdin;
  - respawns a crashed renderer (5 tries per 5 minutes) and restarts it on
    monitor changes. When the extension goes away, the renderer's stdin closes
    and it exits.
- Renderer: `Renderer` (shared) + `View` (per output). Wallpaper mode has one
  borderless window per monitor with independent frame pacing, pauses covered
  outputs, and keeps applying data when nothing draws. It probes every 10 s for
  a collector that starts after login and switches over. The window title
  `gits-file-visualizer|N` maps windows to monitors.
- `tools/sandbox-shell.sh`: a headless `gnome-shell --wayland --no-x11` with
  virtual monitors, an isolated HOME/XDG_RUNTIME_DIR/D-Bus/dconf, and a
  test-only helper extension enabling unsafe mode for Eval and Screenshot. Used
  to verify desktop, overview, workspace switch, covering, lock/unlock,
  renderer crash respawn, and disable/enable cleanup, without touching the
  real session.
- Measured in the sandbox with three monitors (3840×2160, 1920×1200,
  1440×2560) on the real collector stream: all visible, renderer 6.6% + shell
  compositing 4.9% of one core; all covered, renderer 1.6% + shell 0.2%.
  Renderer RSS 270 MB (three views).
- Live data polish found while testing: a file touched constantly (logs, soak
  tests) gets a new card at most every 2 minutes, and folder activity
  saturates so such folders do not dominate the layout.
- Known gaps: per-monitor compositions are still all "wide" (M4). No fanotify.
  Renderer logs go to gnome-shell's journal. Clicking the desktop goes to DING
  as before (the clone is not reactive), which is intended.

### M3 — efficiency (2026-09-27)

Measured with `gits-render loadtest` and `tools/measure.sh` (system CPU,
per-process CPU, and iGPU busy time from `rc6_residency_ms`):

- GPU selection: Vulkan backend only (GL is a fallback), and the Vulkan driver
  list is narrowed before loading (no lavapipe; no NVIDIA driver when an
  iGPU exists and `prefer_igpu`). Renderer RSS 171 → 112 MB, no llvmpipe
  threads, and the dGPU stays asleep.
- Tower shader: split into an exact per-pixel path for sparse (unwound) rings
  and a dense path that samples a per-ring colour texture (`ringcolor.wgsl`,
  256 angle bins per ring, rows redrawn only when their ring changed plus a
  rolling 2 s refresh). Each depth layer's cylinder is drawn only over the height
  bands holding its rings. Live portrait frame 55 → 24 ms at the old
  resolution; the drum shimmer is gone (frame-to-frame RMSE while retiring
  rings 0.0067 → 0.0056).
- Resolution: 0.5 scale, capped at 0.8 MP per monitor. Wallpaper windows are
  created at that size and GNOME scales the clone up, so neither we nor the
  compositor touch 4K pixels.
- Frame rate follows *fast motion* only (a deleted segment falling, the loop
  fade): 12 fps then, 6 fps otherwise, 4 on battery (`/sys/class/power_supply`).
  Unwinding, flashes, labels and flakes all look fine at 6 fps with the smear.
  Folder open/close uses hysteresis so steady activity does not keep rings
  easing forever. Live data steps once per frame instead of 24×/s.
- Result, three monitors, live data (~35 events/s): renderer 5.4% of one core +
  gnome-shell 2.8%, iGPU ~19% busy with all visible; 0.5% + 0.4%, iGPU ~1% with
  all covered. At the M2 settings (24 fps, 1.6 MP) the same test gave 12% CPU and
  a ~40% busy iGPU, which is too much on a laptop whose iGPU also runs the
  desktop.
- Lesson: `bench` and the sandbox shell saturate the shared iGPU and made the
  real desktop sluggish while testing. Builds now run under `nice -n 19`,
  `sandbox-shell.sh stop` kills everything started in the sandbox, and cost is
  measured with `loadtest` (real pacing) rather than `bench`.

### M4 — compositions (2026-09-27)

- `wide` (establishing shot), `drum` (beside the tower, the striations filling a
  tall frame), `dial` (down the axis; back faces at full weight). `auto`
  (default): portrait → drum, largest landscape → wide, others → dial. Per
  connector via `[monitors."DP-1"]`. Not done: `band`, security-voice HUD
  lines during surges, optional composition rotation.

### Comfort pass (2026-09-28)

First real use: the 6 fps timer-paced frames plus motion trails caused
nausea, and the drum looked like a noisy low-light photo. Guidance reviewed:
the VR comfort guides (Oculus/Meta/Qualcomm) call for a steady frame rate locked
to the display (≥60), no judder or dropped frames, no motion blur, and avoiding
high-contrast fine patterns and sudden accelerations. The W3C/web.dev guidance
agrees (and also lists flicker and rotation).

- Frame pacing: 60 fps (`fps`), each redraw released by the compositor's
  frame callback (vsync-locked), mailbox presentation, with all motion driven
  by real elapsed time. The synthetic session keeps fixed internal steps, so it
  stays deterministic.
- Removed defaults: motion smear (trails) and film grain. Bloom lowered.
- GPU: `gpu = "auto"` uses NVIDIA at native resolution on mains (the NVIDIA
  Vulkan driver can present to GNOME 42 Wayland windows in the real session,
  though not in the headless sandbox). Battery: integrated GPU at 0.6 scale and
  30 fps; a power change makes the renderer exit with code 75 and the extension
  restarts it.
- Noise: the dense path averages every ring in the pixel footprint (up to 16
  taps, box filter), and the ring colour texture has 1024 angle bins (from 256).
  The drum now reads as brushed-gold striations.
- Hitches: layout builds run on a worker thread against a shared, frozen
  `Arc<Model>` (collector updates wait in their channel for the ~20–40 ms).
  Rings keep their atlas and colour-texture row ("slot") across relayouts, so an
  install uploads and recolours only changed rings (typically 1 of 15.7k). Ghost
  expiry is incremental. The full sweep only runs while hidden. Live relayouts
  run at most every 20 s. Result: longest frame 16.9 ms over 75 s at 60 fps.
- Write spin boosts ease in and out (smoothstep over 12 s, one at a time), not
  an instant jump in angular speed; `spin_up` 0.35 → 0.15.
- CPU at 60 fps × 3 monitors: ~20% of one core (world step ~6%, encoding
  ~14%, of which the lookup-table/bands update is ~0.6 ms/step).

### Callouts and radius by depth (2026-09-28)

- Radius by depth (§5.1). With 8 shells, folders of different depths
  interleave finely along the tower, so several shells overlap in most pixels
  of the dense drum and the old dense path (up to 16 taps per shell, most
  hitting other shells' rings) dropped the frame rate to 48 fps. Each shell now
  visits only its own rings: `layer_index` lists each layer's rings in order
  with a per-ring lookup table, the shader box-filters those in the pixel's
  footprint (weighted by coverage, every `stride`-th counted from the start of
  the list when there are more than the tap budget, so the chosen rings do not
  change as rings move), and discards pixels where its shell has no rings.
  Back at 60 fps with the GPU less busy than before the change.
- Callouts (`callouts.rs`, `[labels]`): up to 36 at once for creates, edits,
  reads and deletes (tagged NEW / EDIT / READ / DELETED with size and folder).
  Labels sit in two columns just outside the tower's silhouette with leader
  lines to the file, like the outside labels of a pie chart. Each monitor
  gives a new callout the free row nearest its file and keeps it there until
  it fades, so labels never slide; the leader line follows the turning ring.
  A new file's line points at its folder's tick until the file appears, then
  glides over. Deleted files keep their callout, anchored at the ghost. Rate
  limited (6/s, bursts of 4, reads leave room for other kinds), a file gets a
  new callout at most every 30 s, and when full the oldest one fades out.
- Live relayouts every 5 s (was 20 s): new files appear sooner. Install costs
  ~4 ms on the render thread (the build is on the worker).
- The collector watches folders found inside a newly created folder straight
  away (`mkdir -p`, `git clone`, unpacking archives), which were previously
  only picked up by the crawl.
- `tools/stress.py` drives real file activity in `~/gits-stress`.
- One copy of the verse on the wall is the English translation.

### Upscaling, whole-tower framing, system monitor (2026-09-28)

- Wide shot: the camera distance and height are fitted so the whole tower and
  the floor lights fill the frame, centred, for any screen shape (was: the top
  left the frame). The calligraphy was rearranged around the smaller tower.
- Upscaling. Temporal upscalers (TAA-U, FSR 2, DLSS) were ruled out: they need
  motion vectors for the spinning rings and ghost where those are wrong, which
  is the blur/trail problem from the comfort pass (and DLSS is NVIDIA-only).
  Used instead, the spatial approach plus HUD-at-native: on the discrete GPU
  the wallpaper window stays at native size, the scene (tower, chamber, flakes)
  is drawn at `render_scale` (0.58) and the final pass upscales it with
  Catmull-Rom from 5 bilinear taps, a little unsharp masking
  (`upscale_sharpness`), clamped to the 2x2 nearest texels (no ringing on
  bright thin lines). Callout text, leader lines, notices and the system
  monitor are then drawn at native resolution onto the finished frame
  (`fs_hud`: same tonemap, additive). The integrated GPU keeps the small
  window that GNOME scales up (least memory bandwidth), text included.
- Tower mesh 256 → 96 slices per band (silhouette within 0.2 px of a circle at
  4K): the vertex/raster work did not scale with resolution.
- Measured (NVIDIA board power, 3 monitors, 60 fps; the live wallpaper paused
  so it does not share the GPU): native 63 W; 0.67 ~54 W; 0.58 ~36 W; 0.5 ~35 W;
  without the tower pass ~14 W, so the tower's fragment shader is still
  nearly all of it. The driver keeps top clocks until the load falls under a
  threshold, hence the jump between 0.67 and 0.58.
- System monitor (`sysmon.rs`): CPU (/proc/stat), memory (/proc/meminfo), GPU
  busy (NVML via dlopen on NVIDIA; RC6 residency on Intel; gpu_busy_percent on
  AMD), power (battery discharge watts; on mains the GPU's watts from NVML/AMD
  hwmon; RAPL needs root), network (/proc/net/dev, virtual interfaces skipped).
  Sampled once a second on a thread, only while visible; drawn in the lower
  right of the main monitor with one-minute sparklines scrolling smoothly
  (~2 px/s), callout rows avoid it.

### Balance: excludes and per-tree cap (2026-09-28)

`gits-render --source collector stats --shells` showed the outermost shell
(depth 7+) holding 10.5k of 15.7k rings and 67% of the tower's height, mostly
generated trees: opencode's snapshot store, Chrome extensions and Service Worker
caches, ESP-IDF build trees, uv's Pythons, pnpm's store. Changes:

- Default excludes for stores programs manage for themselves (opencode
  snapshots, uv, pnpm, browser and editor extensions, Service Worker caches).
  Build folders stay: build activity is worth seeing.
- `[tower] max_tree_share = 0.1`: no directory's subtree may take more than 10%
  of the ring budget (checked for the subtree and every enclosing one below ~;
  folders directly in ~ are always shown). Beyond it the least recently
  changed subfolders stay collapsed as dashed aggregates, which still flash on
  activity; active folders are expanded first.
- Result on this home: outermost shell 51% of the height, no single tree above
  ~1.6k rings. Deep folders are simply the majority of any home, so the
  outermost shell stays the largest.

### Look pass: folder slices, gold resting colours, portrait framing (2026-09-28)

- Folder slices (`[tower] sectors`): each expanded folder gets an angular
  sector inside its parent's (∝ content^`sector_weight`, at least
  `min_sector`), like a sunburst. Only one folder shows at any height (the
  height→ring lookup), so strict slices leave the tower mostly dark; each
  folder's rings therefore cover at least `sector_fill` (0.6) of the circle,
  centred on its slice. A true sunburst needs siblings side by side at the
  same height (several rings per height per shell), which the renderer can't
  draw yet. With sectors the tower turns as one (sectors must stay aligned);
  rings ease to a moved sector over 12 s; write spin boosts are off.
- Resting colours: old = deep gold, stale = dim bronze (was orange / brown).
- Portrait screens get the whole tower (`auto`); `[camera] wide_fill`,
  `wide_fill_portrait` set how much of the screen it fills.
- `shell_dim`: an outer shell's near side is dimmed where inner shells are
  behind it (per-height shell mask appended to the LUT buffer).
- Optional scan band (`scan_every`, off by default).
- Pausing: the extension reports each monitor's covered share; the renderer
  pauses at `[performance] pause_when_covered` (0.8).
- From the GPU review: the dense path reads a precomputed per-shell ring list
  (phases in f64), and the upscale uses textureGather: about -27% GPU time.

### Shell cache, full-resolution small screens, dithering, display layout (2026-09-28)

- Shell image pyramid (`pyramid.wgsl`, `[performance] shell_cache`): with
  sectors the whole tower turns as one, so each shell's packed drum is a
  fixed image in the shell's frame. Level L of the pyramid averages 2^L
  consecutive rings of a shell (lists padded to 256-ring blocks); rings start
  at whole multiples of 1/256 turn, so level 1 is an exact copy of the ring
  colour rows. Only rows whose rings changed (ring colour rows redrawn,
  rings easing to a new sector) are rebuilt, about 0.05 ms per frame. The
  dense path reads one level in at most 4 blocks per pixel instead of up to
  16 rings, and still visits rings one by one where a pixel covers 16 or fewer.
  An exact aligned-block decomposition was tried first and was slower (up to
  24 dependent reads). Measured: tower pass -20..35% at 4K; loadtest power
  34 -> 27 W at P0/P3; output PSNR 43-46 dB against the per-ring path, same mean.
- `min_scene_megapixels = 2.1`: `render_scale` no longer lowers screens of
  about 1080p (the portrait screen was drawn at 626x1113 and looked pixelated);
  only the 4K screen is upscaled. Net power is about the same as before the
  pyramid.
- Dithering at the output (Gjøl, "Banding in Games"): triangular noise of
  +-1 step per channel in the output's encoding (+-0.5 next to black and
  white), fixed per pixel. The dark corner of a 4K frame went from 250 to 689
  distinct colours; no visible grain.
- Shells: `inner_radius` 0.3 and `shell_curve` 2 (radius ~ depth^2) fill the
  centre of the dial view.
- `tools/display-layout.py`: GNOME matches stored monitor layouts by connector
  name, and the monitors come up on different connectors (Intel vs NVIDIA
  outputs) or late or without EDID; it then fell back to a default layout
  (portrait screen at 1024x768). The tool saves the layout by monitor
  identity and, from a login autostart entry, applies it and re-checks every
  3 s for 3 minutes (ApplyMonitorsConfig, persistent).
- `GITS_GPUPROF=1 gits-render bench ...` prints GPU time per pass (from the
  GPU review).

### Far side, no combine pass, time base, quality knobs (2026-09-28)

- Far side of the tower: `back_face_detail` (0.25) of the dense samples and
  cache blocks (not in the dial, which is made of the shells' inside).
- No combine pass while motion trails are off: the scene pass starts from a
  copy of the cached background, and bloom and the final pass read the scene
  (identical output, PSNR 57-66 dB against the old path).
- GPU time base: shader times were `world.t as f32`, which after a day of
  uptime resolves only 7.8 ms (31 ms after three days), so fades and ring
  rotation would step unevenly. All GPU times are now relative to `t_base`,
  moved forward every 4 h (`World::rebase` shifts every stored event time
  and ring phase by the same amount; tested to change nothing on screen).
- Dithering rewritten with a PCG hash and a gamma-2 estimate of the output
  step: +0.4 ms -> +0.07 ms at 4K, same effect (915 vs 250 distinct colours
  in a dark corner).
- Quality knobs (`[performance]` dense_samples, cache_blocks,
  back_face_detail, ring_bins; `[monitors."<connector>"] render_scale`),
  applied live. With the shell cache the tower costs 0.6-0.8 ms at 4K, so
  each knob now saves at most ~0.1 ms; see README "Quality knobs".
- Measured (3 monitors, 60 fps, headless, real data): 22-33 W, renderer
  11-12% of one core. Was 28-36 W.

### Efficiency backlog

- Tower fragment shader is ~65% of GPU time: cheaper samples in the dense drum,
  scissor to the tower's screen rectangle, fewer layers for back faces.
- Full-output passes are bandwidth-bound on iGPUs: skip `combine` when smear is
  off, use `Rg11b10Ufloat` targets where supported, merge passes.
- Calm policy: after a few idle minutes, slow the ring spin and drop to 2–4 fps.
  The motion smear hides the low rate.
- Per-pass GPU timestamps in `bench` (the `GITS_SKIP` env var is a stopgap).
- Stable drum filtering: where many rings share a pixel, box-filter per-ring
  average colours via a prefix-sum buffer instead of 2–3 point samples. This
  removes the sparkle when ring spacing animates (unwinding, loop retire), and is
  cheaper. Keep one point sample for event highlights.
- Portrait: the tower covers more pixels; consider a composition that pulls
  back further.
- Memory: wgpu enumerates every Vulkan driver, so lavapipe (software
  Vulkan) is loaded and spawns 16 idle llvmpipe threads; skip it. Intern file
  names into one arena (both processes), drop the
  collector's per-directory `PathBuf`s (rebuild paths from parents), and find
  what makes up the renderer's 170 MB baseline (Vulkan driver, wgpu, staging).
- Relayout cost on live trees (25–70 ms on the render thread): move the layout
  build to a worker thread and swap it in, or rebuild only the changed bands.

## 12. Milestones

1. **M0 look-dev:** windowed renderer on synthetic data (a scripted "work
   session" event generator): drum shader, coil unwinding, event colours,
   bloom/smear/grain, cached chamber + calligraphy background, Wide tower
   composition, config file with hot reload. Iterate against `docs/ref/`.
2. **M1 collector:** `$HOME` scan with excludes, inotify, snapshot, IPC; real
   data drives M0.
3. **M2 embed:** `SurfaceBackend` interface + GNOME 42 extension on this machine
   (Wayland, 3 monitors incl. portrait).
4. **M3 efficiency:** visibility pause, activity-driven fps, iGPU, budgets
   measured, screencast privacy switch.
5. **M4 compositions:** Drum close-up, Dial, Band; per-monitor assignment;
   activity levels + security-voice HUD; optional drift/rotation.
6. **M5 portability:** GNOME 45+ ESM extension, 24.04/26.04 VM tests, install
   script or `.deb`, user systemd units.
7. **Later / optional:** layer-shell backend, root fanotify helper (whole
   system + process names), still-frame fallback for other desktops.
