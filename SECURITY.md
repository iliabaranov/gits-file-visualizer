# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub's
[private vulnerability reporting](https://github.com/iliabaranov/gits-file-visualizer/security/advisories/new)
rather than a public issue.

## What to know about this project

- `gits-collector` reads your home folder (names, sizes and modification times,
  not file contents) and serves them on a Unix socket in `$XDG_RUNTIME_DIR` that
  only your user can open. It makes no network connections.
- `gits-render` draws that data. It also reads `/proc` and `/sys` for the system
  readout, and for the disk chart measures folder sizes in your home folder
  (metadata only, at idle priority), caching folder names and sizes in
  `~/.cache/gits-file-visualizer/disk-usage.txt`. File and folder names can be
  hidden or replaced by codes (`[privacy]`); on the lock screen they are codes
  unless `[lock_screen] show_names = true`.
- The GNOME extension runs inside GNOME Shell with the shell's privileges.
- Install and uninstall act only on your home directory and need no `sudo`.

Only the latest commit on `main` is supported.
