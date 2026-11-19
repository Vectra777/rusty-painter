#!/usr/bin/env bash
set -euo pipefail

TARGET="${1:-aarch64-linux-android}"
MODE="${2:-debug}"

if ! command -v cargo-apk >/dev/null 2>&1; then
  echo "cargo-apk is required. Install it with: cargo install cargo-apk" >&2
  exit 1
fi

if [[ -z "${ANDROID_SDK_ROOT:-}" && -z "${ANDROID_HOME:-}" ]]; then
  echo "Set ANDROID_SDK_ROOT (or ANDROID_HOME) before building." >&2
  exit 1
fi

if [[ -z "${ANDROID_NDK_ROOT:-}" && -z "${ANDROID_NDK_HOME:-}" ]]; then
  SDK_DIR="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
  if [[ -n "${SDK_DIR}" && -d "${SDK_DIR}/ndk" ]]; then
    NDK_DIR="$(find "${SDK_DIR}/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
    if [[ -n "${NDK_DIR}" ]]; then
      export ANDROID_NDK_ROOT="${NDK_DIR}"
    fi
  elif [[ -n "${SDK_DIR}" && -d "${SDK_DIR}/ndk-bundle" ]]; then
    export ANDROID_NDK_ROOT="${SDK_DIR}/ndk-bundle"
  fi
fi

if [[ -z "${ANDROID_NDK_ROOT:-}" && -z "${ANDROID_NDK_HOME:-}" ]]; then
  echo "Set ANDROID_NDK_ROOT (or ANDROID_NDK_HOME) before building." >&2
  exit 1
fi

# The APK's assets: the repo's default brush pack (when Git LFS has pulled
# it, not just its pointer), so a local build never ships a stale copy; a
# build without it simply has no default brushes.
ROOT="$(dirname "$0")/.."
mkdir -p "$ROOT/target/android-assets"
if [[ -f "$ROOT/assets/brush-pack/default-v1/brushes.rpbrush" ]] &&
  ! head -c 40 "$ROOT/assets/brush-pack/default-v1/brushes.rpbrush" | grep -q '^version https://git-lfs'; then
  rm -rf "$ROOT/target/android-assets/default-v1"
  cp -R "$ROOT/assets/brush-pack/." "$ROOT/target/android-assets/"
fi
# The realistic figures likewise (as figures-v1/, the app's name for them).
if [[ -f "$ROOT/assets/figure-pack/v1/male.glb" ]] &&
  [[ "$(head -c 4 "$ROOT/assets/figure-pack/v1/male.glb")" == "glTF" ]]; then
  rm -rf "$ROOT/target/android-assets/figures-v1"
  cp -R "$ROOT/assets/figure-pack/v1" "$ROOT/target/android-assets/figures-v1"
fi
# The hand and body tracking models likewise (as pose-models-v1/);
# NO_POSE_MODELS=1 leaves them out (~24 MB): the app downloads them the
# first time hand or body tracking is used.
rm -rf "$ROOT/target/android-assets/pose-models-v1"
if [[ -z "${NO_POSE_MODELS:-}" ]] && [[ -f "$ROOT/assets/pose-models/v1/hand_landmark.onnx" ]] &&
  ! head -c 40 "$ROOT/assets/pose-models/v1/hand_landmark.onnx" | grep -q '^version https://git-lfs'; then
  cp -R "$ROOT/assets/pose-models/v1" "$ROOT/target/android-assets/pose-models-v1"
fi

# 16 KB pages (see .cargo/config.toml, whose flags cargo-apk replaces).
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-Wl,-z,max-page-size=16384"

case "${MODE}" in
  release)
    cargo apk build --lib --target "${TARGET}" --release
    ;;
  debug)
    cargo apk build --lib --target "${TARGET}"
    ;;
  *)
    echo "Usage: $0 [target] [debug|release]" >&2
    exit 1
    ;;
esac
