#!/usr/bin/env bash
# Regenerates assets/ from the Linux icon, unmargined. The unplated sizes are
# what the taskbar and Start show without a coloured plate.
#
#   nix shell nixpkgs#librsvg -c packaging/windows/icons.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
svg="$here/../linux/page.codeberg.korba.Copiale.svg"

render() { rsvg-convert -w "$1" -h "$1" -o "$here/assets/$2.png" "$svg"; }

for scale in 100 200; do
  render $((44 * scale / 100)) "Square44x44Logo.scale-$scale"
  render $((150 * scale / 100)) "Square150x150Logo.scale-$scale"
done
for size in 16 24 32 48 256; do
  render "$size" "Square44x44Logo.targetsize-${size}_altform-unplated"
done
render 50 StoreLogo.scale-100
