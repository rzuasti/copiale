# Bundled fonts

## Copiale Emoji (`CopialeEmoji.ttf`)

Colour emoji fallback on Linux (`theme::emoji`); macOS and Windows use the
system's emoji font.

- **Base:** [Noto Color Emoji](https://github.com/googlefonts/noto-emoji)
  `2D/fonts/NotoColorEmoji.ttf` at tag `v2026-09-24-unicode18_0`
  (v2.057, Unicode 18.0). License: `OFL.txt`.
- **Changes** (`patch_emoji.py`):
  - Family renamed to "Copiale Emoji", so it never matches an older system
    Noto. The PostScript name stays `NotoColorEmoji`, which GPUI requires
    to draw it in colour.
  - `m` mapped to the blank space glyph: GPUI only loads families with an
    `m`.
- **Update:**

  ```sh
  nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (p: [p.fonttools])' \
    -c python3 patch_emoji.py NotoColorEmoji.ttf CopialeEmoji.ttf
  ```

- **Limits:**
  - Only for characters the configured font lacks: fonts with their own
    symbol glyphs (DejaVu Sans, Liberation) keep drawing ❤ ☺ ☀ in
    monochrome.
  - Text-default characters the configured font lacks (↔, ▶) show as emoji
    even without U+FE0F.
  - Keycaps (1️⃣, #️⃣) stay text: GPUI draws ASCII-led clusters with the
    primary font.
