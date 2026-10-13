#!/usr/bin/env bash
# Cross-builds ffmpeg.exe for Windows x86_64 from Linux, for
# the release zip: only what the app uses (H.264 and VP9 out, ffmpeg's own
# decoders and dav1d in), linked statically, so it runs on a fresh Windows.
# The same slim build as the Flatpak's (flatpak/io.vectra.rustypainter.yml).
# Needs (Ubuntu): mingw-w64 nasm meson ninja-build pkg-config git curl make
# Writes target/ffmpeg-windows/ffmpeg.exe.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="$PWD/target/ffmpeg-windows"
WORK="$PWD/target/ffmpeg-windows-build"
P="$WORK/prefix"
HOST=x86_64-w64-mingw32
FFMPEG=9.0.2
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
X264=b35605ace3ddf7c1a5d67a2eb553f034aef41d55
VPX=v1.17.0
DAV1D=1.5.4
JOBS="$(nproc)"

rm -rf "$WORK"
mkdir -p "$WORK" "$OUT"
cd "$WORK"
export PKG_CONFIG_LIBDIR="$P/lib/pkgconfig"

git clone -q https://code.videolan.org/videolan/x264.git
git -C x264 checkout -q "$X264"
(cd x264 && ./configure --host="$HOST" --cross-prefix="$HOST-" --prefix="$P" \
  --enable-static --disable-cli --enable-pic && make -j"$JOBS" && make install)

git clone -q --depth 1 --branch "$VPX" https://chromium.googlesource.com/webm/libvpx
(cd libvpx && CROSS="$HOST-" ./configure --target=x86_64-win64-gcc --prefix="$P" \
  --enable-static --disable-shared --disable-examples --disable-tools --disable-docs \
  --disable-unit-tests && make -j"$JOBS" && make install)

git clone -q --depth 1 --branch "$DAV1D" https://code.videolan.org/videolan/dav1d.git
cat > cross.txt <<EOF
[binaries]
c = '$HOST-gcc'
ar = '$HOST-ar'
strip = '$HOST-strip'
windres = '$HOST-windres'
[host_machine]
system = 'windows'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF
meson setup dav1d/build dav1d --cross-file cross.txt --prefix="$P" --libdir=lib \
  --default-library=static --buildtype=release -Denable_tools=false -Denable_tests=false
ninja -C dav1d/build install

curl -fsSL -o ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-${FFMPEG}.tar.xz"
echo "${FFMPEG_SHA256}  ffmpeg.tar.xz" | sha256sum -c -
tar xf ffmpeg.tar.xz
(cd "ffmpeg-${FFMPEG}" && ./configure --prefix="$P" --arch=x86_64 --target-os=mingw32 \
  --cross-prefix="$HOST-" --pkg-config=pkg-config --pkg-config-flags=--static \
  --extra-ldflags=-static --enable-gpl --enable-libx264 --enable-libvpx --enable-libdav1d \
  --disable-ffplay --disable-ffprobe --disable-doc --disable-debug && make -j"$JOBS")
cp "ffmpeg-${FFMPEG}/ffmpeg.exe" "$OUT/"
echo "Built $OUT/ffmpeg.exe"
