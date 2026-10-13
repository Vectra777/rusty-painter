#!/usr/bin/env bash
# A flamegraph of one brush preset painting, without the app's window: the
# preset's stroke through the real app (stroke worker, undo, wet paint
# drying), repeated, recorded with perf.
#
#   scripts/flamegraph_brush.sh "Wet Smear"                # -> flamegraph-Wet_Smear.svg
#   scripts/flamegraph_brush.sh "Wet Round" 10 300         # 10 strokes at 300 px
#   RP_PEN_MS=0 scripts/flamegraph_brush.sh "Blender Rake" # samples as fast as painted
#   OUT=x.svg FREQ=1999 scripts/flamegraph_brush.sh ...
#
# Every preset's lag, without a graph:
#   cargo run --release --features bench --example profile_presets
#
# Built with frame pointers in its own target dir (see scripts/flamegraph.sh).
set -euo pipefail

NAME="${1:?usage: $0 \"Preset name\" [strokes] [size]}"
STROKES="${2:-6}"
SIZE="${3:-}"
OUT="${OUT:-flamegraph-${NAME// /_}.svg}"
FREQ="${FREQ:-999}"

export CARGO_TARGET_DIR=target/profiling
export CARGO_PROFILE_RELEASE_DEBUG=true
export RUSTFLAGS="${RUSTFLAGS:-} -C force-frame-pointers=yes"
cargo build --release --features bench --example profile_presets

FG="$(command -v flamegraph || true)"
for candidate in "${CARGO_HOME:-}/bin/flamegraph" "$HOME/.cargo/bin/flamegraph" \
    "$HOME/.local/share/cargo/bin/flamegraph"; do
    if [[ -z "$FG" && -x "$candidate" ]]; then FG="$candidate"; fi
done
if [[ -z "$FG" ]]; then
    echo "flamegraph not found; install it with: cargo install flamegraph" >&2
    exit 1
fi

# (--no-inline: see scripts/flamegraph.sh.)
"$FG" --no-inline --output "$OUT" --cmd "record -F ${FREQ} --call-graph fp" -- \
    "$CARGO_TARGET_DIR/release/examples/profile_presets" "$NAME" "$STROKES" $SIZE
echo "wrote $OUT"
