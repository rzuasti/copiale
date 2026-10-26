#!/usr/bin/env bash
# Builds an unsigned, universal target/macos/Copiale.app. macOS only; needs
# the Rust targets below, Xcode's command line tools and rsvg-convert. Each
# target links its own static FFmpeg (ffmpeg.sh).
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
here="$root/packaging/macos"
app="$root/target/macos/Copiale.app"
targets=(aarch64-apple-darwin x86_64-apple-darwin)

version=$(cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "copiale") | .version')
MACOSX_DEPLOYMENT_TARGET=$(/usr/libexec/PlistBuddy -c "Print :LSMinimumSystemVersion" "$here/Info.plist")
export MACOSX_DEPLOYMENT_TARGET

# Bindings to FFmpeg need libclang.
LIBCLANG_PATH="${LIBCLANG_PATH:-$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib}"
export LIBCLANG_PATH

binaries=()
for target in "${targets[@]}"; do
  FFMPEG_DIR=$("$here/ffmpeg.sh" "$target") \
    cargo build --manifest-path "$root/Cargo.toml" --release --locked --package copiale --target "$target"
  binaries+=("$root/target/$target/release/copiale")
done

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/copiale" "${binaries[@]}"
# FFmpeg is linked in: nothing outside the system's libraries.
if otool -L "$app/Contents/MacOS/copiale" | tail -n +2 | grep -Ev '^\s*(/usr/lib/|/System/Library/)'; then
  echo "linked to libraries outside the system" >&2
  exit 1
fi
sed "s/@VERSION@/$version/g" "$here/Info.plist" >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"

iconset="$(mktemp -d)/Copiale.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
  rsvg-convert -w "$size" -h "$size" -o "$iconset/icon_${size}x${size}.png" "$here/Copiale.svg"
  rsvg-convert -w $((size * 2)) -h $((size * 2)) -o "$iconset/icon_${size}x${size}@2x.png" "$here/Copiale.svg"
done
iconutil --convert icns --output "$app/Contents/Resources/Copiale.icns" "$iconset"
rm -r "$(dirname "$iconset")"

echo "Built $app ($version)"
