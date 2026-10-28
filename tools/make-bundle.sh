#!/usr/bin/env bash
# Builds a prebuilt bundle to copy to another Linux machine (x86_64, glibc 2.35+,
# e.g. Ubuntu 22.04 or newer): no Rust toolchain needed there.
#
#   tools/make-bundle.sh            -> out/gits-file-visualizer-linux-x86_64.tar.gz
#
# On the other machine:
#   tar xzf gits-file-visualizer-linux-x86_64.tar.gz
#   gits-file-visualizer/install.sh
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=gits-file-visualizer
STAGE=out/bundle/$NAME
nice -n 19 cargo build --release --locked -p gits-collector -p gits-render
rm -rf out/bundle && mkdir -p "$STAGE/bin" "$STAGE/tools"
install -m 755 target/release/gits-collector target/release/gits-render "$STAGE/bin/"
strip "$STAGE/bin/"*                        # drop debug info: ~110 MB -> ~25 MB
cp -r extension packaging "$STAGE/"
install -m 755 tools/install-user.sh "$STAGE/install.sh"
install -m 755 tools/stress.py tools/display-layout.py tools/restart.sh tools/install-shortcut.sh "$STAGE/tools/"
cp README.md LICENSE* "$STAGE/" 2>/dev/null || true
cp config/default.toml "$STAGE/config-defaults.toml"
tar -C out/bundle -czf "out/$NAME-linux-x86_64.tar.gz" "$NAME"
ls -lh "out/$NAME-linux-x86_64.tar.gz"
