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
