# Bundled fonts

Small subsets, embedded into the binary at compile time, so the renderer never
depends on fonts installed on the host. Regenerate with `tools/make-fonts.sh`.

| File | Source | License |
|------|--------|---------|
| `hud-mono.ttf` | DejaVu Sans Mono (Latin-1, arrows, box drawing, shapes) | Bitstream Vera / public domain changes, see `LICENSE-DejaVu.txt` |
| `calligraphy-cjk.otf` | Noto Serif CJK JP Bold (Zeami verse + a few HUD words) | SIL OFL 1.1, see `LICENSE-NotoSerifCJK.txt` |
