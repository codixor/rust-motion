# Reproducible generator for tests/fixtures/fallback_cjk.ttf
#
# Provenance / license:
#   * The font is defined from scratch below (no glyphs are copied from any
#     other font). It contains exactly two glyphs: ".notdef" and "uni4F60"
#     (U+4F60 你), with a single cmap entry 0x4F60 -> uni4F60.
#   * It is produced with fontTools FontBuilder (https://github.com/fonttools/fonttools),
#     MIT License (c) Just van Rossum / fontTools contributors. This script
#     itself is authored by the rust-motion maintainers under the repository's
#     license and contains no third-party code.
#   * Generated with fontTools 4.68.0; other fontTools versions may produce
#     byte-different (but functionally equivalent) output. Regeneration is
#     optional: the committed binary is the test input, this script documents
#     how it was made.
#
# Regenerate:
#   python3 tests/fixtures/fallback_cjk_gen.py
#
# Deterministic by construction (no timestamps; single fixed glyph outline).

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
import os

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fallback_cjk.ttf")

fb = FontBuilder(1000, isTTF=True)
fb.setupGlyphOrder([".notdef", "uni4F60"])
fb.setupCharacterMap({0x4F60: "uni4F60"})

pen = TTGlyphPen(None)
pen.moveTo((100, 0))
pen.lineTo((100, 700))
pen.lineTo((700, 700))
pen.lineTo((700, 0))
pen.closePath()

fb.setupGlyf({".notdef": TTGlyphPen(None).glyph(), "uni4F60": pen.glyph()})
fb.setupHorizontalMetrics({".notdef": (500, 0), "uni4F60": (800, 50)})
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({"familyName": "FallbackCJKTest", "styleName": "Regular"})
fb.setupOS2()
fb.setupPost()
fb.save(path)

with open(path, "rb") as f:
    import hashlib
    print("wrote", path, len(f.read()), "bytes sha256", hashlib.sha256(open(path, "rb").read()).hexdigest()[:16])
