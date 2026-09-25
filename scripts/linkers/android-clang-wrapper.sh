#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <ndk-tool> [linker-args...]" >&2
  exit 1
fi

TOOL="$1"
shift

find_ndk_root() {
  if [[ -n "${ANDROID_NDK_ROOT:-}" ]]; then
    printf '%s\n' "${ANDROID_NDK_ROOT}"
    return 0
  fi

  if [[ -n "${ANDROID_NDK_HOME:-}" ]]; then
    printf '%s\n' "${ANDROID_NDK_HOME}"
    return 0
  fi

  local sdk_root="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
  if [[ -n "${sdk_root}" && -d "${sdk_root}/ndk" ]]; then
    ls -d "${sdk_root}"/ndk/* 2>/dev/null | sort -V | tail -n 1
    return 0
  fi

  return 1
}

NDK_ROOT="$(find_ndk_root || true)"
if [[ -z "${NDK_ROOT}" || ! -d "${NDK_ROOT}" ]]; then
  echo "Android NDK not found. Set ANDROID_NDK_ROOT, ANDROID_NDK_HOME, ANDROID_SDK_ROOT, or ANDROID_HOME." >&2
  exit 1
fi

case "$(uname -s)" in
  Linux) HOST_TAG="linux-x86_64" ;;
  Darwin)
    if [[ "$(uname -m)" == "arm64" ]]; then
      HOST_TAG="darwin-arm64"
    else
      HOST_TAG="darwin-x86_64"
    fi
    ;;
  MINGW*|MSYS*|CYGWIN*) HOST_TAG="windows-x86_64" ;;
  *)
    echo "Unsupported host platform for Android NDK toolchain: $(uname -s)" >&2
    exit 1
    ;;
esac

TOOL_PATH="${NDK_ROOT}/toolchains/llvm/prebuilt/${HOST_TAG}/bin/${TOOL}"
if [[ ! -x "${TOOL_PATH}" ]]; then
  echo "Android NDK tool not found: ${TOOL_PATH}" >&2
  exit 1
fi

exec "${TOOL_PATH}" "$@"
