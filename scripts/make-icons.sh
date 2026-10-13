#!/usr/bin/env bash
# Makes every app icon from one square picture (ImageMagick 7):
#   scripts/make-icons.sh painting.webp
# The picture should be opaque (iOS shows transparency as black).
set -euo pipefail
SRC="${1:?usage: $0 <square image>}"
cd "$(dirname "$0")/.."
r() { magick "$SRC" -filter Lanczos -resize "$1x$1" -strip "$2"; }

mkdir -p assets/icon
r 1024 assets/icon/icon-1024.png
# The window and taskbar icon (src/lib.rs).
r 256 assets/icon/icon-256.png
# The .exe's icon (build.rs).
magick assets/icon/icon-1024.png -filter Lanczos -define icon:auto-resize=256,64,48,32,24,16 assets/icon/icon.ico

# Android launcher icons (Cargo.toml's `resources`), one per density.
for d in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
  mkdir -p "android/res/mipmap-${d%%:*}"
  r "${d##*:}" "android/res/mipmap-${d%%:*}/ic_launcher.png"
done

# Android 8+ adaptive icon (mipmap-anydpi-v26/ic_launcher.xml): the picture
# fills the 72 dp the launcher's mask shows of the 108 dp layer, its edge
# pixels stretched into the margin that only parallax and odd mask shapes reveal.
for d in mdpi:108 hdpi:162 xhdpi:216 xxhdpi:324 xxxhdpi:432; do
  s="${d##*:}"; m=$((s / 6))
  magick "$SRC" -filter Lanczos -resize "$((s - 2 * m))x$((s - 2 * m))" -virtual-pixel edge \
    -set option:distort:viewport "${s}x${s}-${m}-${m}" -distort SRT 0 +repage -strip \
    "android/res/mipmap-${d%%:*}/ic_launcher_background.png"
done

# iPad home screen (Info.plist's CFBundleIcons~ipad, copied by build-ios.sh).
r 152 ios/AppIcon76x76@2x~ipad.png
r 167 ios/AppIcon83.5x83.5@2x~ipad.png
