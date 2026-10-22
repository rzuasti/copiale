"""Writes emoji_shortcodes.tsv from emoji-datasource's emoji.json; see README.md."""

import json
import sys


def main(source, target):
    emoji = json.load(open(source, encoding="utf-8"))
    emoji = [e for e in emoji if not e.get("obsoleted_by")]
    emoji.sort(key=lambda e: e["sort_order"])
    with open(target, "w", encoding="utf-8") as out:
        for e in emoji:
            text = "".join(chr(int(cp, 16)) for cp in e["unified"].split("-"))
            for name in e["short_names"]:
                out.write(f"{name}\t{text}\t{e['category']}\n")


if __name__ == "__main__":
    main(*sys.argv[1:])
