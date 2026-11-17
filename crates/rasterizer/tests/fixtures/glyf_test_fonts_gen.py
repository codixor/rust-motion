def fmt2_payload_pair(left, right, value):
    # Crafted so that a parser that (incorrectly) treats this format-2 subtable
    # as format 0 would binary-search and find pair (left, right) = value.
    # The real format-2 layout is rowWidth/leftClassCount/rightClassCount/...;
    # those fields are deliberately set so the mis-parse finds the pair.
    # A correct parser skips the subtable entirely (version != 0).
    header = struct.pack(">HHH", 2, 20, 1)  # version/format=2, length, coverage=1
    body = struct.pack(">HHHH", 1, 6, 0, 0)  # mis-parsed as: nPairs=1, searchRange=6, entrySelector=0, rangeShift=0
    body += struct.pack(">HHh", left, right, value)
    return header + body

# Reproducible generator for the differential-test fixture fonts.
#
# Provenance / license:
#   * All glyph outlines are defined from scratch in this script (no outlines
#     are copied from any other font).
#   * Produced with fontTools FontBuilder (https://github.com/fonttools/fonttools),
#     MIT License (c) Just van Rossum / fontTools contributors. This script
#     itself is authored by the rust-motion maintainers under the repository's
#     license and contains no third-party code.
#   * Generated with fontTools 4.68.0; other fontTools versions may produce
#     byte-different (but functionally equivalent) output.
#
# Outputs (next to this script):
#   glyf_quirk_test.ttf
#       Two simple glyphs (.notdef, uni4F60 = square) plus "uniMatched", a
#       composite with TWO components that both use POINT-MATCHED arguments
#       (ARGS_ARE_XY_VALUES = 0). The first component also carries a 2x2
#       transform; ttf-parser (and the port in glyf_outline.rs) intentionally
#       does NOT consume the two matched-point argument bytes before reading
#       the transform, so the transform values are read 2 bytes early. This
#       fixture exercises that quirk differentially.
#   kern_fmt0.ttf
#       Same two simple glyphs plus an OT kern table with one format-0
#       subtable: pair (glyph 0, glyph 1) = -100.
#   kern_fmt2.ttf
#       Same glyphs plus an OT kern table with one format-2 subtable whose
#       payload is crafted so that a parser that mistakes it for format 0
#       would read pair (0, 1) = -500. Correct parsers skip it and return 0.
#
# Regenerate:  python3 tests/fixtures/glyf_test_fonts_gen.py

import os
import struct
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables.DefaultTable import DefaultTable
from fontTools.ttLib.tables._g_l_y_f import Glyph, GlyphComponent

HERE = os.path.dirname(os.path.abspath(__file__))

SQUARE = TTGlyphPen(None)
SQUARE.moveTo((100, 0))
SQUARE.lineTo((100, 700))
SQUARE.lineTo((700, 700))
SQUARE.lineTo((700, 0))
SQUARE.closePath()

def base_builder():
    fb = FontBuilder(1000, isTTF=True)
    fb.setupGlyphOrder([".notdef", "uni4F60"])
    fb.setupCharacterMap({0x4F60: "uni4F60"})
    fb.setupGlyf({".notdef": TTGlyphPen(None).glyph(), "uni4F60": SQUARE.glyph()})
    fb.setupHorizontalMetrics({".notdef": (500, 0), "uni4F60": (800, 50)})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "GlyfQuirkTest", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    # Pin head timestamps so regeneration is byte-identical across runs.
    fb.font["head"].created = 0x7C259DC4
    fb.font["head"].modified = 0x7C259DC4
    return fb

def kern_table(subtables):
    body = struct.pack(">HH", 0, len(subtables))
    for sub in subtables:
        body += sub
    t = DefaultTable("kern")
    t.data = body
    return t

def fmt0_pair(left, right, value):
    # OT format-0 subtable wire layout (both ttf-parser and read-fonts parse the
    # body as: nPairs, searchRange, entrySelector, rangeShift, then pairs):
    #   header: version/format=0, length=20 (6 header + 14 body), coverage=1 (horizontal)
    #   body:   nPairs=1, searchRange=6, entrySelector=0, rangeShift=0, pair(left, right, value)
    header = struct.pack(">HHH", 0, 20, 1)
    body = struct.pack(">HHHH", 1, 6, 0, 0)
    body += struct.pack(">HHh", left, right, value)
    return header + body

def fmt2_payload_pair(left, right, value):
    header = struct.pack(">HHH", 2, 24, 1)  # version/format=2, length, coverage=horizontal
    payload = struct.pack(">HHHHHH", 0, 0, 1, 6, 0, 0)
    payload += struct.pack(">HHh", left, right, value)
    return header + payload

def matched_composite():
    g = Glyph()
    g.numberOfContours = -1
    g.xMin, g.yMin, g.xMax, g.yMax = 0, 0, 700, 700
    c1 = GlyphComponent()
    c1.glyphName = "uni4F60"
    c1.flags = 0  # compile() computes MORE_COMPONENTS itself
    c1.x = 0
    c1.y = 0  # XY offset args (ARGS_ARE_XY_VALUES set)
    c1.transform = ((1.0, 0.0), (0.0, 1.0))  # 2x2 -> WE_HAVE_A_TWO_BY_TWO
    c2 = GlyphComponent()
    c2.glyphName = "uni4F60"
    c2.flags = 0
    # Point-matched args: fontTools serializes firstPt/secondPt with
    # ARGS_ARE_XY_VALUES cleared (BB or HH), then the 2x2 transform as four
    # F2DOT14 values. ttf-parser does NOT consume the two point-argument
    # bytes before reading the transform, so it reads the F2DOT14 values two
    # bytes early (replicated in glyf_outline.rs). fontTools only supports
    # firstPt indexing into the ALREADY-ACCUMULATED composite points, so the
    # point-matched component must come after the XY component.
    c2.firstPt = 0
    c2.secondPt = 0
    c2.transform = ((1.0, 0.0), (0.0, 1.0))  # 2x2 -> WE_HAVE_A_TWO_BY_TWO
    g.components = [c1, c2]
    return g

# glyf_quirk_test.ttf — composite added directly to the glyf table (skips
# FontBuilder.calcGlyphBounds, which requires XY offsets on components).
fb = base_builder()
glyf = fb.font["glyf"]
glyf.glyphOrder.append("uniMatched")
glyf.glyphs["uniMatched"] = matched_composite()
# Invalidate the cached reverse glyph map so getGlyphID sees "uniMatched".
fb.font.__dict__.pop("_reverseGlyphOrderDict", None)
fb.setupCharacterMap({0x4F60: "uni4F60", 0xE000: "uniMatched"})
fb.setupHorizontalMetrics(
    {".notdef": (500, 0), "uni4F60": (800, 50), "uniMatched": (800, 50)}
)
fb.save(os.path.join(HERE, "glyf_quirk_test.ttf"))

fb0 = base_builder()
fb0.font["kern"] = kern_table([fmt0_pair(0, 1, -100)])
fb0.save(os.path.join(HERE, "kern_fmt0.ttf"))

fb2 = base_builder()
fb2.font["kern"] = kern_table([fmt2_payload_pair(0, 1, -500)])
fb2.save(os.path.join(HERE, "kern_fmt2.ttf"))

import hashlib
for name in ("glyf_quirk_test.ttf", "kern_fmt0.ttf", "kern_fmt2.ttf"):
    path = os.path.join(HERE, name)
    with open(path, "rb") as f:
        data = f.read()
    print("wrote", name, len(data), "bytes sha256", hashlib.sha256(data).hexdigest()[:16])
