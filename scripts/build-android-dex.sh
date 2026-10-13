#!/usr/bin/env bash
# Rebuild android/classes.dex (embedded in the library and loaded at run
# time, see src/android.rs) from android/java. Run after editing the Java;
# the dex is committed so normal builds need no Java toolchain.
set -euo pipefail

SDK="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Android/Sdk}}"
PLATFORM="$(find "$SDK/platforms" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
BUILD_TOOLS="$(find "$SDK/build-tools" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
JAVAC="${JAVAC:-$(command -v javac || echo "$HOME/android-studio/jbr/bin/javac")}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

"$JAVAC" --release 11 -Xlint:-deprecation -classpath "$PLATFORM/android.jar" -d "$OUT/classes" \
  $(find "$ROOT/android/java" -name '*.java')
"$BUILD_TOOLS/d8" --release --min-api 29 --lib "$PLATFORM/android.jar" --output "$OUT" \
  $(find "$OUT/classes" -name '*.class')
cp "$OUT/classes.dex" "$ROOT/android/classes.dex"
echo "Wrote android/classes.dex"
