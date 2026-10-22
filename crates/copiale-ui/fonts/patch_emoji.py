"""Builds Copiale Emoji from upstream Noto Color Emoji; see README.md."""

import sys

from fontTools.ttLib import TTFont

FAMILY = "Copiale Emoji"
# GPUI draws only this PostScript name in colour on Linux.
POSTSCRIPT = "NotoColorEmoji"


def main(source, target):
    font = TTFont(source)

    # GPUI only loads a family that maps 'm'; point it at the blank space glyph.
    for table in font["cmap"].tables:
        if table.isUnicode() and 0x20 in table.cmap:
            table.cmap[ord("m")] = table.cmap[0x20]

    # A family of its own, so an older system Noto never matches it.
    name = font["name"]
    for record in list(name.names):
        if record.nameID in (1, 4, 16):
            record.string = FAMILY
        elif record.nameID == 3:
            record.string = f"{FAMILY};{record.toUnicode()}"
        elif record.nameID == 6:
            record.string = POSTSCRIPT

    font.save(target)


if __name__ == "__main__":
    main(*sys.argv[1:])
