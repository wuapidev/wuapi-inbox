# Assets

Everything here is compiled into the binary (`src/brand.rs`, and `src/emoji/data.rs` for the emoji tables). Nothing is read from outside this repository at build or run time.

## wuapi brand

| File | Origin | Licence |
|---|---|---|
| `brand/mark-color.svg` | wuapi brand kit, `svg/mark-color.svg` | wuapi trademark, see below |
| `brand/app-icon-rounded-color.svg` | wuapi brand kit, `svg/app-icon-rounded-color.svg` | wuapi trademark, see below |
| `icons/app-icon-{32,48,64,128,256,512}.png` | wuapi brand kit, `png/app-icon-rounded/color/` | wuapi trademark, see below |
| `brand/BRAND-KIT-README.txt` | wuapi brand kit, `README.txt` | usage rules of the kit |
| `brand/wallpaper-lines.svg`, `brand/wallpaper-marks.svg` | ours: the two layers of the conversation's wallpaper tile (`src/ui/wallpaper.rs`). The first is crosshairs, ticks, brackets and dots (the ticks are Lucide's `check-check`, ISC); the second is the outline of the mark | wuapi trademark for the mark's outline, see below |

The kit is published at <https://wuapi.dev/brand>; these copies were taken unchanged from `apps/wuapi/public/brand` of the wuapi repository on 2026-09-30.

The logo and the icon are **not** covered by this repository's Apache-2.0 licence. They are wuapi's trademark and are used here by its owner. A fork that is not a wuapi product has to replace them (two SVGs, six PNGs, and the constants in `src/brand.rs`). The kit's rules apply: keep the clear space, 16 px minimum height, and do not stretch, rotate, recolour or add effects.

Only the lime variant is used: the bare mark on dark surfaces, and the mark on its ink tile (the application icon) on light ones.

## Fonts

| Files | Origin | Licence |
|---|---|---|
| `fonts/Inter-{Regular,Italic,Medium,SemiBold}.ttf` | Inter 4.1, `extras/ttf/` of <https://github.com/rsms/inter/releases/tag/v4.1> | SIL Open Font License 1.1, `fonts/Inter-LICENSE.txt` |
| `fonts/JetBrainsMono-{Regular,Medium}.ttf` | JetBrains Mono 2.304, `fonts/ttf/` of <https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304> | SIL Open Font License 1.1, `fonts/JetBrainsMono-OFL.txt` |

Both files are unmodified. The OFL allows bundling and redistributing them with software under any licence, Apache-2.0 included, as long as the licence text travels with them and they are not sold on their own; neither font declares a Reserved Font Name. They are the two families of the wuapi design system. The wuapi site ships them as Latin-subset WOFF files, which GPUI cannot load, so these are the full TrueType files from the upstream releases.

## Emoji

| Files | Origin | Licence |
|---|---|---|
| `emoji/base.txt.gz` | Every fully-qualified emoji of `emoji-test.txt`, Emoji 18.0 (<https://www.unicode.org/Public/emoji/latest/emoji-test.txt>, dated 2026-04-30), in its order and groups; English names and keywords from CLDR 48.2 (`common/annotations/en.xml`, `common/annotationsDerived/en.xml` of <https://github.com/unicode-org/cldr>, tag `release-48-2`); shortcodes from gemoji (`db/emoji.json` of <https://github.com/github/gemoji>) | Unicode License v3, `emoji/UNICODE-LICENSE.txt`; gemoji's data MIT, `emoji/GEMOJI-LICENSE.txt` |
| `emoji/{es,pt,hi,fr,de,it,id,ar,ru,tr}.txt.gz` | Names and keywords from CLDR 48.2, the same two files per locale; `es` also carries `es_419` and `pt` carries `pt_PT` | Unicode License v3, `emoji/UNICODE-LICENSE.txt` |
| `emoji/generate.py` | ours (Apache-2.0): builds the files above from the downloaded sources | |

The tables are derived data, not copies: `generate.py` keeps what the picker needs (the sequences, one name, the keywords) and writes it as gzipped text, one line per emoji. Nothing is downloaded at build or run time; to take a newer Unicode or CLDR release, download the sources named at the top of the script and run it again. The Unicode licence allows using, modifying and redistributing the data files with software under any licence, Apache-2.0 included, as long as its notice travels with them, which is what `emoji/UNICODE-LICENSE.txt` is; gemoji's MIT licence asks the same of its notice.

CLDR 48.2 is one release behind Emoji 18.0: the nine or so newest emoji have an English name (the data file's own) and none in the other languages yet, so they are found in English only.

| File | Bytes in the binary |
|---|---|
| `base.txt.gz` (every emoji, English) | 51,949 |
| `es.txt.gz` | 41,852 |
| `pt.txt.gz` | 48,623 |
| `hi.txt.gz` | 42,358 |
| `fr.txt.gz` | 35,313 |
| `de.txt.gz` | 34,823 |
| `it.txt.gz` | 41,366 |
| `id.txt.gz` | 31,676 |
| `ar.txt.gz` | 44,601 |
| `ru.txt.gz` | 43,453 |
| `tr.txt.gz` | 33,675 |
| all of them | 449,689 |

No emoji font is bundled: the glyphs are the system's (see "Emoji" in `docs/ARCHITECTURE.md`).

## Linux desktop entry

`linux/dev.wuapi.inbox.desktop` is ours (Apache-2.0). On Wayland a window takes its icon from the desktop entry whose name matches the application id, so a package installs this file to `share/applications/` and the PNGs to `share/icons/hicolor/<size>x<size>/apps/dev.wuapi.inbox.png`. Where the platform takes the icon from the application, the window is given `icons/app-icon-256.png` directly.
