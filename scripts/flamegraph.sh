#!/usr/bin/env bash
# Profile the app and write a flamegraph. Use the app (paint, pan...) while it
# runs, then close its window: the SVG is written when it exits.
#
#   scripts/flamegraph.sh                 # -> flamegraph.svg
#   scripts/flamegraph.sh out.svg         # custom output
#   FREQ=199 scripts/flamegraph.sh        # fewer samples per second
#
# Why not plain `cargo flamegraph`: it records call stacks with
# `--call-graph dwarf`, which copies a chunk of stack memory for every sample
# on every thread (the brush thread pool has dozens), writing gigabytes to
# perf.data and overloading CPU and disk. Building with frame pointers lets
# perf walk stacks cheaply (`--call-graph fp`): ~1 MB for 10 s here.
set -euo pipefail

OUT="${1:-flamegraph.svg}"
FREQ="${FREQ:-499}"

# Separate target dir so the normal release build cache isn't invalidated.
export CARGO_TARGET_DIR=target/profiling
export CARGO_PROFILE_RELEASE_DEBUG=true
export RUSTFLAGS="${RUSTFLAGS:-} -C force-frame-pointers=yes"

# `cargo flamegraph` only works if cargo-flamegraph is in the cargo home cargo
# searches; look for the binary directly so either install location works.
CF="$(command -v cargo-flamegraph || true)"
for candidate in "${CARGO_HOME:-}/bin/cargo-flamegraph" "$HOME/.cargo/bin/cargo-flamegraph" \
    "$HOME/.local/share/cargo/bin/cargo-flamegraph"; do
    if [[ -z "$CF" && -x "$candidate" ]]; then CF="$candidate"; fi
done
if [[ -z "$CF" ]]; then
    echo "cargo-flamegraph not found; install it with: cargo install flamegraph" >&2
    exit 1
fi

# --no-inline: without it `perf script` resolves inlined frames by running
# addr2line per address, which took ~7 minutes (at 100% CPU) for a 10 s
# recording here, versus a fraction of a second.
"$CF" flamegraph --release --bin rusty-painter --output "$OUT" --no-inline \
    --cmd "record -F ${FREQ} --call-graph fp"
