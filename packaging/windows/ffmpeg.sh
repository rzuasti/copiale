#!/usr/bin/env bash
# Builds the static FFmpeg the video helper links on Windows (x86_64, MSVC)
# into target/ffmpeg/x86_64-pc-windows-msvc; prints that directory. Kept if
# already built. Same version and options as packaging/macos/ffmpeg.sh:
# only the MOV demuxer, the H.264, HEVC, VP9 and AAC decoders, scaling and
# resampling; no network, devices, filters, programs or system libraries.
#
# Runs in MSYS2 (make, diffutils), with MSVC's environment (vcvars64.bat)
# inherited. Usage: packaging/windows/ffmpeg.sh
set -euo pipefail

version=9.0.1
# The release tarball's, checked against its signature by FFmpeg's release
# key (FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8).
sha256=cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635
libs=(avcodec avformat avutil swresample swscale)

root=$(cd "$(dirname "$0")/../.." && pwd)
prefix="$root/target/ffmpeg/x86_64-pc-windows-msvc"
if [ -f "$prefix/lib/avcodec.lib" ] && [ -f "$prefix/lib/swresample.lib" ]; then
  cygpath -m "$prefix"
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/ffmpeg.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz"
echo "$sha256  $work/ffmpeg.tar.xz" | sha256sum -c - >&2
tar -xJf "$work/ffmpeg.tar.xz" -C "$work"

(
  cd "$work/ffmpeg-$version"
  # -MT: the static C runtime, as Copiale links on Windows.
  ./configure \
    --prefix="$prefix" \
    --toolchain=msvc --target-os=win64 --arch=x86_64 \
    --extra-cflags=-MT \
    --enable-static --disable-shared \
    --disable-everything --disable-autodetect --disable-network \
    --disable-programs --disable-doc \
    --disable-avdevice --disable-avfilter \
    --enable-avcodec --enable-avformat --enable-swscale --enable-swresample \
    --enable-demuxer=mov \
    --enable-decoder=h264,hevc,vp9,aac \
    --enable-parser=h264,hevc,vp9,aac \
    --disable-x86asm >&2
  make -j"$(nproc)" >&2
  make install >&2
)
# MSVC's linker looks for avcodec.lib; FFmpeg names it libavcodec.a.
for lib in "${libs[@]}"; do
  if [ ! -f "$prefix/lib/$lib.lib" ]; then
    cp "$prefix/lib/lib$lib.a" "$prefix/lib/$lib.lib"
  fi
done
cygpath -m "$prefix"
