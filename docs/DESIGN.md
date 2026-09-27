# gits-file-visualizer — Design (DRAFT v2)

Status: planning. Decided items are marked ✅; open items are marked ❓.

## 1. Goal

A live desktop wallpaper for Ubuntu 22.04 / 24.04 / 26.04 that renders the
filesystem as the glowing security-system tower from the LOCUS SOLUS hack in
*Ghost in the Shell 2: Innocence* (2004). Files are arc segments of stacked
rings; reads, writes, creation and deletion drive the animation in real time.

Priority order: **looks like the film > efficient > informative.**

## 2. Decisions so far

| Topic | Decision |
|-------|----------|
| Reference | ✅ Togusa hacking the LOCUS SOLUS ship (YouTube `idEMI562YM0`, "Ghost in the Shell [Hacking Scene]") |
| Scope | ✅ `$HOME` first (unprivileged inotify); pipeline designed so a root fanotify helper can be added later |
| Layout | ✅ The film's language wins over strict usability; multiple looks. ❓ Details in §5 |
| Monitors | ✅ One view per monitor: shared data, independent camera ("director") per monitor |
| Text | ✅ Real paths; auto-obfuscated while screen sharing |
| Performance | ✅ Ambient 24 fps (film rate), ≈0 when hidden/idle, iGPU preferred |
| Interaction | ✅ Passive only |
| Desktops | ❓ See §4.1 |
| Stack | ✅ Rust + wgpu |

## 3. Visual reference: what the scene actually shows

Reference frames are kept locally in `docs/ref/` (gitignored; film stills are
not committed). They come from YouTube storyboard sprites of the clip plus one
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
   生死去来 棚頭傀儡 一線断時 落落磊磊, "…puppets on a shelf; when the one string
   is cut, they collapse and scatter". Possible optional backdrop.

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

### 4.1 Desktop support ❓ (discussion)

| | GNOME extension backend | layer-shell backend |
|---|---|---|
| Covers | Ubuntu default on 22.04/24.04/26.04 | Kubuntu (Plasma 6), COSMIC, Sway/Hyprland/niri |
| Mechanism | Extension moves our window's actor into the background group | Standard protocol: `background` layer surface per output |
| Robustness | API breaks between GNOME versions (42–44 legacy, 45+ ESM); runs inside gnome-shell | Stable protocol, no compositor code |
| Occlusion / lock / overview signals | Full (extension sees window geometry) | Weak: frame-callback throttling, varies per compositor |
| Screen-share detection (privacy) | Yes | Per-DE, often unavailable |
| Rust plumbing | `winit` toplevel window | `smithay-client-toolkit` + raw wgpu surface |
| Extra test burden | 3 GNOME versions (pinned by LTS) | 1–2 more environments |

Proposal: a small surface-backend interface from day one, GNOME first, and
layer-shell as an optional later milestone. Universal fallback for any desktop:
render a still frame every N seconds and set it as the normal wallpaper.

## 5. Visual concept: one tower, many shots

Instead of separate modes with separate geometry there is **one scene**: the
tower in its chamber. It has **two states** (drum ↔ coil) and **several camera
"shots"** taken from the film's editing. A per-monitor *director* chooses and
holds shots. This matches the film, keeps a single dataset on the GPU, and
makes every shot cheap.

### 5.1 Data → tower mapping

- **Ring = a directory's row of files.** The **segments** on a ring are its
  files. **Arc length ∝ log(size)**, with small gaps at file boundaries. A
  directory with many files wraps onto several stacked rings, forming a *band*.
- **Vertical order = depth-first path order**, so every subtree is a
  contiguous stretch of the tower. `~` is at the top.
- **Radius steps inward slightly with depth**, which gives the layered
  see-through parallax visible in the wide shot.
- Each ring spins slowly at its own rate (seeded by a hash, alternating
  direction) and speeds up briefly when touched.

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

### 5.3 Event colours and motions (proposal ❓)

| Event | Look |
|-------|------|
| idle, by mtime age | gold (recent) → amber/orange → dim bronze (ancient) |
| read/open | segment flashes **cream-white**; the probe visits; afterglow decays over minutes |
| write/modify | segment turns **crimson** ("altered"), and the ring spins up; decays back to gold over ~10–30 min |
| create | a new segment **grows along the arc** from zero length with a white-hot leading tip |
| delete | segment goes bright crimson; its "string is cut": it detaches, falls, and breaks into drifting gold flakes. The gap stays as a faint crimson ghost for hours |
| move/rename | segment slides along the tower to its new ring |
| symlink / aggregated remainder | dashed or dotted arcs |
| a burst in one subtree | white/red **arrowhead swarm** + probe fly-in; HUD lines such as `SECURITY WALL 014 BREACHED` |

Mapping *write* to crimson (not just delete) matters for the look: the film's
wide shot is full of red, so a busy work session should look like that, while
an idle machine stays gold.

### 5.4 Shots (camera presets + HUD layouts)

| Shot | From the film | Use |
|------|---------------|-----|
| **Wide tower** | thumbnail frame | Default "establishing" shot. Slow orbit, teal lattice and floor lights visible |
| **Drum close-up** | panels 2, 5, 6 | Camera near the surface, slow vertical pan along the dense gold striations; micro file-name text. Great for the portrait monitor |
| **Dial** | panels 4, 7 | Looking straight down the axis: the rings become concentric circles. Names are set along the arcs, leader lines point to the hottest files, and a centre box shows an event log in the security-system voice |
| **Band** | panel 3 | Face-on crop of one segmented ring band (the active directory) with bar-length size readouts |
| **Vortex** | panels 10–12 | Fly-through following the probe down the ribbons to a burst. Only used during "intrusion" |

**Director:** holds calm shots for long periods (30–120 s, slow drifts,
cross-dissolves), goes to Dial/Band when activity is sustained, and to Vortex
only on bursts. Each monitor can have its own shot preferences, e.g. 4K = wide
tower, portrait = drum close-up, 1920×1200 = dial.

### 5.5 Alert escalation

1. **Calm:** gold drum, slow drift, occasional scan-line sweep.
2. **Activity:** subtrees unwind, cards and probe appear, HUD fragments.
3. **Intrusion** (mass delete, huge write burst): crimson dominates, arrowhead
   swarm, Vortex shot allowed, plus security-voice HUD (`REBUILDING LEVEL 1
   EXTERNAL SECURITY WALL`). Dramatic, and doubles as a "something is deleting
   lots of files" alarm.

### 5.6 Optional flourishes

- Zeami calligraphy columns as a dim, far background layer in the chamber.
- HUD/glyph layers animated "on twos" (12 fps), a nod to Innocence's
  step-printed ghosts. It also halves their cost.

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
- **One process, one GPU device, one surface per monitor**, with all buffers
  shared. Only camera and HUD differ per monitor.
- **CPU per frame ≈ 0:** the collector sends deltas at ≤10 Hz, and the renderer
  patches buffer and texture rows.

## 8. Efficiency plan

- 0 fps when a monitor is covered, a fullscreen app is running, the screen is
  locked, or the display is blanked.
- 24 fps while visible; drop to ~6–12 fps in calm shots (slow drifts hide it,
  and the motion-smear buffer smooths it). Lower cap on battery.
- Internal resolution 0.5–0.75× on 4K, upscaled (glow hides it).
- Low-power adapter so the Intel iGPU is used and the NVIDIA dGPU stays asleep.
- Budgets: visible <2% of one core, hidden ≈0%, RSS <150 MB total. Measured
  in M3.

## 9. Components

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

## 10. Milestones

1. **M0 look-dev:** windowed renderer on synthetic data: drum shader + coil
   unwinding + post FX + Wide tower shot. Iterate against `docs/ref/` frames.
2. **M1 collector:** `$HOME` scan with excludes, inotify, snapshot, IPC; real
   data drives M0.
3. **M2 embed:** GNOME 42 extension on this machine (Wayland, 3 monitors incl.
   portrait).
4. **M3 efficiency:** visibility pause, adaptive fps, iGPU, budgets measured,
   screencast privacy switch.
5. **M4 shots & director:** Drum close-up, Dial, Band, Vortex, alert escalation,
   per-monitor preferences.
6. **M5 portability:** GNOME 45+ ESM extension, 24.04/26.04 VM tests, install
   script or `.deb`. Optional: layer-shell backend, fanotify helper.
