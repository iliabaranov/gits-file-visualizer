#!/usr/bin/env bash
# Regenerates the bundled font subsets in assets/fonts/.
# Only needed when changing which glyphs are bundled; the outputs are committed,
# so building and running the project never needs system fonts.
#
# Requires: python3 with fontTools (Ubuntu: python3-fonttools),
#           fonts-dejavu-core and fonts-noto-cjk-extra (or any files passed via env).
set -euo pipefail
cd "$(dirname "$0")/.."

MONO_SRC="${MONO_SRC:-/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf}"
CJK_SRC="${CJK_SRC:-/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc}"
CJK_INDEX="${CJK_INDEX:-0}" # face 0 of the Noto Serif CJK collection is JP

# HUD text: printable Latin-1 plus arrows, box drawing and geometric shapes.
python3 -m fontTools.subset "$MONO_SRC" \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2190-21FF,U+2500-259F,U+25A0-25FF" \
  --layout-features='' --no-hinting --desubroutinize \
  --output-file=assets/fonts/hud-mono.ttf

# Calligraphy: Zeami's verse from the film plus a few security-system words.
CJK_TEXT="生死去来棚頭傀儡一線断時落磊防壁侵入警告攻性再構築完了第二三四五六七八九十〇層外部内接続拒否電脳"
python3 -m fontTools.subset "$CJK_SRC" --font-number="$CJK_INDEX" \
  --text="$CJK_TEXT" \
  --layout-features='' --no-hinting --desubroutinize \
  --output-file=assets/fonts/calligraphy-cjk.otf

ls -l assets/fonts/
