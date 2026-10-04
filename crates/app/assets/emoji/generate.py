#!/usr/bin/env python3
"""Builds the emoji data the application embeds (see ../ASSETS.md).

    python3 generate.py <dir with the downloaded sources>

Sources, all fetched by hand into one directory (nothing is downloaded at
build or run time):

    emoji-test.txt            https://www.unicode.org/Public/emoji/latest/emoji-test.txt
    ann_<locale>.xml          https://github.com/unicode-org/cldr  common/annotations/<locale>.xml
    der_<locale>.xml          https://github.com/unicode-org/cldr  common/annotationsDerived/<locale>.xml
    gemoji.json               https://github.com/github/gemoji     db/emoji.json

Output, next to this script:

    base.txt.gz     every emoji in keyboard order, by group, with its English
                    name, shortcodes and English keywords; skin-tone variants
                    follow the emoji they belong to
    <lang>.txt.gz   one line per emoji of base.txt, in the same order: its
                    name, a regional name when that differs, and keywords

The files are gzip with no timestamp, so the same sources give the same
bytes.
"""

import gzip
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
TONES = {0x1F3FB, 0x1F3FC, 0x1F3FD, 0x1F3FE, 0x1F3FF}
VS16 = 0xFE0F

# Language packs: file name, main CLDR locale, regional CLDR locale whose
# names are kept beside the main ones and whose keywords are merged in.
PACKS = [
    ("es", "es", "es_419"),
    ("pt", "pt", "pt_PT"),
    ("hi", "hi", None),
    ("fr", "fr", None),
    ("de", "de", None),
    ("it", "it", None),
    ("id", "id", None),
    ("ar", "ar", None),
    ("ru", "ru", None),
    ("tr", "tr", None),
]


# Mixed skin tones are written with other characters than the emoji they
# belong to: two hands for the handshake, two people for the couples.
BELONGS_TO = {
    "\U0001FAF1\u200D\U0001FAF2": "\U0001F91D",
    "\U0001F9D1\u200D\U0001F430\u200D\U0001F9D1": "\U0001F46F",
    "\U0001F468\u200D\U0001F430\u200D\U0001F468": "\U0001F46F\u200D\u2642",
    "\U0001F469\u200D\U0001F430\u200D\U0001F469": "\U0001F46F\u200D\u2640",
    "\U0001F9D1\u200D\U0001FAEF\u200D\U0001F9D1": "\U0001F93C",
    "\U0001F468\u200D\U0001FAEF\u200D\U0001F468": "\U0001F93C\u200D\u2642",
    "\U0001F469\u200D\U0001FAEF\u200D\U0001F469": "\U0001F93C\u200D\u2640",
    "\U0001F469\u200D\U0001F91D\u200D\U0001F469": "\U0001F46D",
    "\U0001F469\u200D\U0001F91D\u200D\U0001F468": "\U0001F46B",
    "\U0001F468\u200D\U0001F91D\u200D\U0001F468": "\U0001F46C",
    "\U0001F9D1\u200D\u2764\u200D\U0001F48B\u200D\U0001F9D1": "\U0001F48F",
    "\U0001F9D1\u200D\u2764\u200D\U0001F9D1": "\U0001F491",
}


def key(text):
    """An emoji as CLDR writes it: without variation selectors."""
    return "".join(ch for ch in text if ord(ch) != VS16)


def without_tones(text):
    return "".join(ch for ch in text if ord(ch) not in TONES)


def read_test(path):
    """[(group, [(emoji, name, version)])], fully-qualified only."""
    groups = []
    line_re = re.compile(r"^([0-9A-F ]+?)\s*;\s*fully-qualified\s*#\s*(\S+)\s+E(\d+\.\d+)\s+(.+)$")
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("# group:"):
            groups.append((line.split(":", 1)[1].strip(), []))
            continue
        found = line_re.match(line)
        if not found:
            continue
        emoji = "".join(chr(int(cp, 16)) for cp in found.group(1).split())
        assert emoji == found.group(2), line
        groups[-1][1].append((emoji, found.group(4).strip(), found.group(3)))
    return [(name, rows) for name, rows in groups if name != "Component"]


def read_annotations(source, locale):
    """{emoji key: (name, [keywords])} from the two CLDR files of a locale."""
    names, words = {}, {}
    for kind in ("ann", "der"):
        path = source / f"{kind}_{locale}.xml"
        for node in ET.parse(path).getroot().iter("annotation"):
            cp = key(node.get("cp"))
            text = (node.text or "").strip()
            if not text or text == "↑↑↑":
                continue
            if node.get("type") == "tts":
                names[cp] = text
            else:
                words[cp] = [word.strip() for word in text.split("|") if word.strip()]
    return {cp: (names.get(cp, ""), words.get(cp, [])) for cp in set(names) | set(words)}


def clean(text):
    return text.replace("\t", " ").replace("\n", " ").replace("|", " ").strip()


def slug(name):
    return re.sub(r"_+", "_", re.sub(r"[^a-z0-9]+", "_", name.lower())).strip("_")


def write(name, lines):
    data = ("\n".join(lines) + "\n").encode("utf-8")
    packed = gzip.compress(data, compresslevel=9, mtime=0)
    (HERE / name).write_bytes(packed)
    print(f"{name}: {len(lines)} lines, {len(data)} bytes, {len(packed)} packed")


def main():
    source = Path(sys.argv[1])
    groups = read_test(source / "emoji-test.txt")
    english = read_annotations(source, "en")

    aliases = {}
    for entry in json.loads((source / "gemoji.json").read_text(encoding="utf-8")):
        aliases[key(entry["emoji"])] = entry["aliases"]

    # Bases and their skin-tone variants.
    every = {key(emoji) for _, rows in groups for emoji, _, _ in rows}

    def owner(emoji):
        """The emoji a skin-tone variant belongs to; None for a base."""
        plain = key(without_tones(emoji))
        if plain == key(emoji):
            return None
        plain = plain if plain in every else BELONGS_TO.get(plain)
        assert plain in every, f"{emoji}: a skin-tone variant of nothing known"
        return plain

    bases = []  # (group index, emoji, test name, [variants])
    by_key = {}
    for at, (_, rows) in enumerate(groups):
        for emoji, name, _ in rows:
            if owner(emoji) is None:
                by_key[key(emoji)] = len(bases)
                bases.append((at, emoji, name, []))
    for _, rows in groups:
        for emoji, _, _ in rows:
            if owner(emoji) is not None:
                bases[by_key[owner(emoji)]][3].append(emoji)

    taken = set()
    lines = ["# Generated by generate.py from Unicode emoji-test.txt, CLDR annotations and gemoji."]
    group = None
    codes_of = []
    for at, emoji, name, _ in bases:
        codes = [code for code in aliases.get(key(emoji), []) if code not in taken]
        taken.update(codes)
        codes_of.append(codes)
    for index, (at, emoji, name, variants) in enumerate(bases):
        if at != group:
            group = at
            lines.append(f"G\t{groups[at][0]}")
        cldr_name, words = english.get(key(emoji), ("", []))
        shown = clean(cldr_name) or clean(name)
        codes = codes_of[index]
        if not codes:
            made = slug(name)
            if made and made not in taken:
                taken.add(made)
                codes = [made]
        words = [clean(word) for word in words if clean(word) and clean(word) != shown]
        # The name of the data file, when CLDR calls the emoji something else.
        if clean(name).lower() != shown.lower() and clean(name) not in words:
            words.append(clean(name))
        lines.append("\t".join(["E", emoji, shown, ",".join(codes), "|".join(words)]))
        for variant in variants:
            lines.append(f"V\t{variant}")
    write("base.txt.gz", lines)

    for file, main_locale, regional in PACKS:
        main_data = read_annotations(source, main_locale)
        regional_data = read_annotations(source, regional) if regional else {}
        lines = []
        for _, emoji, _, _ in bases:
            name, words = main_data.get(key(emoji), ("", []))
            other, more = regional_data.get(key(emoji), ("", []))
            name, other = clean(name), clean(other)
            if other == name:
                other = ""
            merged = []
            for word in list(words) + list(more):
                word = clean(word)
                if word and word not in merged and word not in (name, other):
                    merged.append(word)
            lines.append("\t".join([name, other, "|".join(merged)]).rstrip("\t"))
        write(f"{file}.txt.gz", lines)


if __name__ == "__main__":
    main()
