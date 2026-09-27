# gits-file-visualizer

A live GNOME desktop wallpaper that renders your filesystem as the glowing
security-system tower from the LOCUS SOLUS hack in *Ghost in the Shell 2:
Innocence*. Directories are bands of stacked rings and files are arc segments.
Reads flash cream, writes turn crimson, new files grow along their ring, and
deleted files snap loose and fall apart into gold flakes.

Targets Ubuntu 22.04, 24.04 and 26.04 (GNOME 42 / 46 / 50).
Design: [docs/DESIGN.md](docs/DESIGN.md).

## Status

- **M0 (look-dev) — done.** Synthetic home directory and a looping scripted
  work session, used for look-dev and reproducible snapshots.
- **M1 (real data) — done.** `gits-collector` scans your home folder, watches
  it for changes and streams them to the renderer.
- **M2 (wallpaper) — done for GNOME 42–44** (Ubuntu 22.04). A GNOME Shell
  extension starts the renderer and shows it as the background on every
  monitor, including the overview and workspace switching, and pauses monitors
  that are covered. GNOME 45+ (Ubuntu 24.04/26.04) follows in M5.

## Build and run

Only [rustup](https://rustup.rs) is needed; `rust-toolchain.toml` pins the
compiler, and rustup installs it automatically. No system `-dev` packages are
required. At runtime the binary loads the GPU driver (Vulkan) and
Wayland/X11 libraries that every Ubuntu desktop already has.

### As your wallpaper (Ubuntu 22.04 / GNOME 42)

```sh
tools/install-user.sh              # build; install to ~/.local; start the collector service
gnome-extensions enable gits-file-visualizer@iliabaranov.github.io     # wallpaper on
gnome-extensions disable gits-file-visualizer@iliabaranov.github.io    # wallpaper off
tools/install-user.sh uninstall    # remove everything again
```

On Wayland, GNOME only notices a newly installed extension after logging out
and back in once; the script says when that is needed. The extension:

- starts `gits-render wallpaper` (from `~/.local/bin`, `PATH`, or
  `GITS_RENDER_BIN`) and shows its output as the background of every monitor,
  in the overview and during workspace switches; the lock screen keeps the normal
  wallpaper;
- keeps the renderer's windows out of the dock, alt-tab and the overview;
- pauses a monitor while windows cover it, and everything while the screen is
  locked (with every monitor covered the renderer uses ~1.6% of one core, from
  applying live file changes);
- switches file names to obfuscated glyphs while a screencast or screen share is
  running (`[privacy] obfuscate_when_sharing`);
- restarts the renderer if it crashes (at most 5 times in 5 minutes) and when
  monitors change; if the renderer is missing, the normal wallpaper stays.

Logs: `journalctl --user -f -o cat /usr/bin/gnome-shell | grep gits`.

### With your real files, in a window

```sh
gits-render window                 # uses the collector if running (Esc or q quits)
```

Or without installing anything, in two terminals:

```sh
cargo run --release -p gits-collector                  # scan + watch + serve
cargo run --release -p gits-render -- window           # connects automatically
cargo run --release -p gits-collector -- monitor       # print changes as they stream (debugging)
cargo run --release -p gits-collector -- scan          # one-off scan: counts + watch budget
```

The collector runs at idle CPU and I/O priority and serves a Unix socket at
`$XDG_RUNTIME_DIR/gits-file-visualizer/collector.sock` that only your user can
open. `[data] source = "auto"` (default) makes the renderer use the collector
when it is running and fall back to synthetic data otherwise; `--source
collector|synthetic` forces one.

### Look-dev with synthetic data

```sh
cargo run --release -p gits-render -- window --warmup 150   # window; Esc or q quits
cargo run --release -p gits-render -- snapshot --out out/shot.png --at 160
cargo run --release -p gits-render -- snapshot --out out/seq/s.png --at 60 --frames 12 --every 25
cargo run --release -p gits-render -- bench --width 3840 --height 2160
cargo run --release -p gits-render -- stats --minutes 15
cargo run --release -p gits-render -- default-config > my-config.toml
```

### Testing the extension safely

`tools/sandbox-shell.sh` runs a separate, headless GNOME Shell with its own
home, runtime directory, D-Bus and three virtual monitors; nothing touches the
real session. `start`, `eval 'JS'`, `shot file.png`, `run gnome-calculator`,
`log`, `stop`. `GITS_COLLECTOR_SOCKET=$XDG_RUNTIME_DIR/gits-file-visualizer/collector.sock`
connects it to the real collector.

`--warmup` simulates that many seconds of activity before the first frame.
`--seed` and `--scale` change the synthetic home. `snapshot`, `bench` and
`stats` use synthetic data unless given `--source collector`. Snapshots and
other scratch output go to `out/`, which is ignored by git.

## Watch limit

Live change detection uses Linux inotify, which needs one *watch* per folder.
Ubuntu allows each user 65,536 watches (`fs.inotify.max_user_watches`), shared
with every other program that watches files: editors, sync clients such as
Nextcloud, browsers, and so on.

On a large home folder that is not enough for every folder. The collector
therefore takes at most half the limit (and always leaves 4,096 free); see
`[collector] max_watches`. It watches the top-level folders and the most
recently changed folders live. Everything else is re-checked by a slow
background crawl (`crawl_interval`, default 2 minutes), with this effect:

| | Live-watched folders | Crawled folders |
|---|---|---|
| New, deleted, renamed files | within ~0.1 s | on the next crawl pass (up to `crawl_interval`) |
| Writes to existing files | within ~0.1 s | not seen |
| Reads | within ~0.1 s | not seen |

A crawled folder in which the crawl finds changes is promoted to a live watch,
replacing the least recently active one, so busy folders end up watched.
`gits-collector scan` prints how many folders you have and how many fit.
Excluding big folders you do not care about (`[collector] excludes`) frees
watches.

On the development machine: 52k folders and 360k files after the default
excludes; 32,768 are watched live, and about 19k fall back to the crawl.

The limit can be raised system-wide if you want every folder watched (not done
by default; needs sudo; each watch costs about 1 KB of kernel memory):

```sh
echo fs.inotify.max_user_watches=262144 | sudo tee /etc/sysctl.d/60-gits-inotify.conf
sudo sysctl --system
```

## Configuration

Every option and its default lives in [`config/default.toml`](config/default.toml),
which is compiled into the binary. To customise, put only the keys you want to
change in `~/.config/gits-file-visualizer/config.toml`, or pass
`--config file.toml`. The window reloads the file while running. Unknown keys
are rejected, so typos surface immediately.

## Repository layout

| Path | Contents |
|------|----------|
| `crates/gits-core` | Filesystem model, tower layout, config, collector protocol, synthetic data |
| `crates/gits-collector` | Scanner + inotify watcher + socket server (`gits-collector`) |
| `crates/gits-render` | wgpu renderer, window mode, headless snapshots, benchmark |
| `crates/gits-render/shaders` | WGSL: tower, sprites, post-processing |
| `config/default.toml` | All defaults (compiled in) |
| `extension/gnome-42` | GNOME Shell 42–44 extension (wallpaper embedding) |
| `packaging/systemd` | User service for the collector |
| `tools/` | `install-user.sh` (per-user install), `sandbox-shell.sh` (isolated test shell), `make-fonts.sh` |
| `assets/fonts` | Bundled font subsets + licenses (compiled in; see `tools/make-fonts.sh`) |
| `docs/DESIGN.md` | Design, decisions, milestones |
| `docs/ref` | Low-res reference frames from the film (look-dev only) |

## Portability

Everything the project needs is in this repository: fonts are bundled
subsets, defaults are compiled in, the toolchain is pinned, `Cargo.lock` is
committed, and look-dev uses synthetic data, never the host filesystem. The only
things fetched from outside are crates from crates.io on the first build (cached
in `~/.cargo`).

Committing from a new machine: this GitHub account rejects pushes that expose a
private email address, so set the repo-local no-reply address once:
`git config user.email "90713890+iliabaranov@users.noreply.github.com"`.
