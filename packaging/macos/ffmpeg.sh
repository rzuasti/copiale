#!/usr/bin/env bash
# Builds the static FFmpeg the video helper links on macOS, for one Rust
# target, into target/ffmpeg/<target>; prints that directory. Kept if
# already built. Only the MOV demuxer, the H.264, HEVC, VP9 and AAC
# decoders, scaling and resampling: no network, devices, filters, programs
# or system libraries.
#
# Usage: packaging/macos/ffmpeg.sh aarch64-apple-darwin|x86_64-apple-darwin
set -euo pipefail

version=9.0.1
# The release tarball's, checked against its signature by FFmpeg's release
# key (FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8).
sha256=cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635

target=${1:?usage: ffmpeg.sh <rust target>}
case "$target" in
  aarch64-apple-darwin) arch=arm64 ;;
  x86_64-apple-darwin) arch=x86_64 ;;
  *) echo "unsupported target: $target" >&2; exit 2 ;;
esac

root=$(cd "$(dirname "$0")/../.." && pwd)
prefix="$root/target/ffmpeg/$target"
if [ -f "$prefix/lib/libavcodec.a" ] && [ -f "$prefix/lib/libswresample.a" ]; then
  echo "$prefix"
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/ffmpeg.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz"
echo "$sha256  $work/ffmpeg.tar.xz" | shasum -a 256 -c - >&2
tar -xJf "$work/ffmpeg.tar.xz" -C "$work"

minimum=$(/usr/libexec/PlistBuddy -c "Print :LSMinimumSystemVersion" "$root/packaging/macos/Info.plist")
(
  cd "$work/ffmpeg-$version"
  ./configure \
    --prefix="$prefix" \
    --enable-static --disable-shared \
    --enable-cross-compile --target-os=darwin --arch="$arch" \
    --cc="clang -arch $arch" \
    --extra-cflags="-mmacosx-version-min=$minimum" \
    --extra-ldflags="-mmacosx-version-min=$minimum" \
    --disable-everything --disable-autodetect --disable-network \
    --disable-programs --disable-doc \
    --disable-avdevice --disable-avfilter \
    --enable-avcodec --enable-avformat --enable-swscale --enable-swresample \
    --enable-demuxer=mov \
    --enable-decoder=h264,hevc,vp9,aac \
    --enable-parser=h264,hevc,vp9,aac \
    --disable-x86asm >&2
  make -j"$(sysctl -n hw.ncpu)" >&2
  make install >&2
)
echo "$prefix"
