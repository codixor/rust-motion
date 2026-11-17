//! TrueType `glyf` outline extraction + pixel-exact rasterization glue.
//!
//! Replaces the `ab_glyph`/ttf-parser outline pipeline while preserving
//! byte-identical rendered text: the standalone [`ab_glyph_rasterizer`]
//! keeps the exact AA algorithm, and the two arithmetic-critical state
//! machines below (quadratic curve builder, composite-glyph iterator) are
//! translated 1:1 from `ttf-parser` 0.25.1 `src/tables/glyf.rs`
//! (MIT/Apache-2.0); `px_bounds`/`draw` replicate `ab_glyph` 0.2.32
//! `src/outlined.rs` (Apache-2.0). Font data is accessed through
//! `read_fonts` (`FontRef` + `TableProvider`): `head`/`maxp`/
//! `loca`/`glyf` tables only; glyph records are parsed from the raw
//! `glyf` bytes with ttf-parser's exact arithmetic and checked slicing.
//! Scope: static TrueType `glyf` outlines (bundled font). CFF and `gvar`
//! variation are not reproduced; see the task report. Safe Rust only.
//! [`ab_glyph_rasterizer`]: https://crates.io/crates/ab_glyph_rasterizer

use ab_glyph_rasterizer::{point, Rasterizer};
use read_fonts::{FontRef, TableProvider};

/// Maximum composite nesting depth. Mirrors ttf-parser's `MAX_COMPONENTS = 32`
/// exactly (ab_glyph inherits it), so pathological recursive composites are
/// rejected identically to the reference path. Intentional deviation from
/// read-fonts, which imposes no such cap.
const MAX_COMPONENTS: u8 = 32;

// Bounds-checked primitive readers.
fn u8_at(d: &[u8], o: usize) -> Option<u8> {
    d.get(o).copied()
}
fn i8_at(d: &[u8], o: usize) -> Option<i8> {
    u8_at(d, o).map(|v| v as i8)
}
fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}
fn i16_at(d: &[u8], o: usize) -> Option<i16> {
    u16_at(d, o).map(|v| v as i16)
}

/// 2.14 fixed-point scale (ttf-parser `F2DOT14::to_f32`).
fn f2dot14(v: i16) -> f32 {
    f32::from(v) / 16384.0
}
/// f32 midpoint (ttf-parser `Point::lerp`, t = 0.5).
fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1))
}

/// f32 accumulator for the calculated outline bbox (ttf-parser `RectF`).
#[derive(Clone, Copy, Debug)]
struct RectF {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}
impl RectF {
    fn new() -> Self {
        RectF {
            x_min: f32::MAX,
            y_min: f32::MAX,
            x_max: f32::MIN,
            y_max: f32::MIN,
        }
    }
    fn is_default(&self) -> bool {
        self.x_min == f32::MAX
            && self.y_min == f32::MAX
            && self.x_max == f32::MIN
            && self.y_max == f32::MIN
    }
    fn extend_by(&mut self, x: f32, y: f32) {
        self.x_min = self.x_min.min(x);
        self.y_min = self.y_min.min(y);
        self.x_max = self.x_max.max(x);
        self.y_max = self.y_max.max(y);
    }
    /// Truncate-toward-zero i16 conversion (ttf-parser `TryNumFrom<f32>`:
    /// MIN inclusive, MAX+1 exclusive).
    fn to_rect(&self) -> Option<(i16, i16, i16, i16)> {
        fn cvt(v: f32) -> Option<i16> {
            let i = v as i32;
            (i >= i32::from(i16::MIN) && i <= i32::from(i16::MAX)).then_some(i as i16)
        }
        Some((
            cvt(self.x_min)?,
            cvt(self.y_min)?,
            cvt(self.x_max)?,
            cvt(self.y_max)?,
        ))
    }
}

/// 2x3 affine transform (ttf-parser `Transform`).
#[derive(Clone, Copy, Debug)]
struct Transform {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}
impl Default for Transform {
    fn default() -> Self {
        Transform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}
impl Transform {
    fn is_default(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.e == 0.0
            && self.f == 0.0
    }
    fn combine(ts1: Transform, ts2: Transform) -> Self {
        Transform {
            a: ts1.a * ts2.a + ts1.c * ts2.b,
            b: ts1.b * ts2.a + ts1.d * ts2.b,
            c: ts1.a * ts2.c + ts1.c * ts2.d,
            d: ts1.b * ts2.c + ts1.d * ts2.d,
            e: ts1.a * ts2.e + ts1.c * ts2.f + ts1.e,
            f: ts1.b * ts2.e + ts1.d * ts2.f + ts1.f,
        }
    }
    fn apply_to(&self, p: &mut (f32, f32)) {
        let (tx, ty) = *p;
        p.0 = self.a * tx + self.c * ty + self.e;
        p.1 = self.b * tx + self.d * ty + self.f;
    }
}

/// Glyph outline primitives in font units (ab_glyph `OutlineCurve`).
#[derive(Clone, Debug)]
enum OutlineCurve {
    Line((f32, f32), (f32, f32)),
    Quad((f32, f32), (f32, f32), (f32, f32)),
    /// CFF cubics are out of scope (bundled font is TrueType); kept for
    /// draw-path parity with ab_glyph.
    #[allow(dead_code)]
    Cubic((f32, f32), (f32, f32), (f32, f32), (f32, f32)),
}

/// Curve sink, same behavior as ab_glyph's `OutlineCurveBuilder`.
#[derive(Default)]
struct OutlineCurveBuilder {
    last: (f32, f32),
    last_move: Option<(f32, f32)>,
    outline: Vec<OutlineCurve>,
}
impl OutlineCurveBuilder {
    fn take_outline(mut self) -> Vec<OutlineCurve> {
        self.close();
        self.outline
    }
    fn move_to(&mut self, x: f32, y: f32) {
        self.last = (x, y);
        self.last_move = Some(self.last);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p1 = (x, y);
        self.outline.push(OutlineCurve::Line(self.last, p1));
        self.last = p1;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p1 = (x1, y1);
        let p2 = (x, y);
        self.outline.push(OutlineCurve::Quad(self.last, p1, p2));
        self.last = p2;
    }
    fn close(&mut self) {
        if let Some(m) = self.last_move.take() {
            self.outline.push(OutlineCurve::Line(self.last, m));
        }
    }
}

/// Transforms/bboxes points and feeds the sink; 1:1 translation of
/// ttf-parser's `glyf::Builder`.
struct Builder<'a> {
    builder: &'a mut OutlineCurveBuilder,
    transform: Transform,
    is_default_ts: bool,
    bbox: RectF,
    first_on_curve: Option<(f32, f32)>,
    first_off_curve: Option<(f32, f32)>,
    last_off_curve: Option<(f32, f32)>,
}
impl<'a> Builder<'a> {
    fn new(transform: Transform, bbox: RectF, builder: &'a mut OutlineCurveBuilder) -> Self {
        Builder {
            builder,
            transform,
            is_default_ts: transform.is_default(),
            bbox,
            first_on_curve: None,
            first_off_curve: None,
            last_off_curve: None,
        }
    }
    fn move_to(&mut self, mut p: (f32, f32)) {
        if !self.is_default_ts {
            self.transform.apply_to(&mut p);
        }
        self.bbox.extend_by(p.0, p.1);
        self.builder.move_to(p.0, p.1);
    }
    fn line_to(&mut self, mut p: (f32, f32)) {
        if !self.is_default_ts {
            self.transform.apply_to(&mut p);
        }
        self.bbox.extend_by(p.0, p.1);
        self.builder.line_to(p.0, p.1);
    }
    fn quad_to(&mut self, mut p1: (f32, f32), mut p: (f32, f32)) {
        if !self.is_default_ts {
            self.transform.apply_to(&mut p1);
            self.transform.apply_to(&mut p);
        }
        self.bbox.extend_by(p1.0, p1.1);
        self.bbox.extend_by(p.0, p.1);
        self.builder.quad_to(p1.0, p1.1, p.0, p.1);
    }
    fn push_point(&mut self, x: f32, y: f32, on_curve_point: bool, last_point: bool) {
        let p = (x, y);
        if self.first_on_curve.is_none() {
            if on_curve_point {
                self.first_on_curve = Some(p);
                self.move_to(p);
            } else if let Some(offcurve1) = self.first_off_curve {
                let mid = lerp(offcurve1, p, 0.5);
                self.first_on_curve = Some(mid);
                self.last_off_curve = Some(p);
                self.move_to(mid);
            } else {
                self.first_off_curve = Some(p);
            }
        } else {
            match (self.last_off_curve, on_curve_point) {
                (Some(offcurve), true) => {
                    self.last_off_curve = None;
                    self.quad_to(offcurve, p);
                }
                (Some(offcurve), false) => {
                    self.last_off_curve = Some(p);
                    let mid = lerp(offcurve, p, 0.5);
                    self.quad_to(offcurve, mid);
                }
                (None, true) => self.line_to(p),
                (None, false) => self.last_off_curve = Some(p),
            }
        }
        if last_point {
            self.finish_contour();
        }
    }
    fn finish_contour(&mut self) {
        if let (Some(offcurve1), Some(offcurve2)) = (self.first_off_curve, self.last_off_curve) {
            self.last_off_curve = None;
            let mid = lerp(offcurve2, offcurve1, 0.5);
            self.quad_to(offcurve2, mid);
        }
        if let (Some(p), Some(offcurve1)) = (self.first_on_curve, self.first_off_curve) {
            self.quad_to(offcurve1, p);
        } else if let (Some(p), Some(offcurve2)) = (self.first_on_curve, self.last_off_curve) {
            self.quad_to(offcurve2, p);
        } else if let Some(p) = self.first_on_curve {
            self.line_to(p);
        }
        self.first_on_curve = None;
        self.first_off_curve = None;
        self.last_off_curve = None;
        self.builder.close();
    }
}

/// Parses one simple-glyph record's points into `builder`; 1:1 translation
/// of ttf-parser's `parse_simple_outline` + `GlyphPointsIter`.
/// Returns `false` on truncation.
fn parse_simple_outline(glyph_data: &[u8], number_of_contours: u16, builder: &mut Builder) -> bool {
    let n = number_of_contours as usize;
    let mut off = 10; // skip i16 contour count + 8-byte header bbox
    let mut endpoints = Vec::with_capacity(n);
    for _ in 0..n {
        endpoints.push(match u16_at(glyph_data, off) {
            Some(v) => v,
            None => return false,
        });
        off += 2;
    }
    let points_total = match endpoints.last().copied().and_then(|e| e.checked_add(1)) {
        Some(v) => v,
        None => return false,
    };
    if points_total == 1 {
        return true;
    } // ttf-parser: empty iterator
    let instructions_len = match u16_at(glyph_data, off) {
        Some(v) => v as usize,
        None => return false,
    };
    off += 2;
    off = off.saturating_add(instructions_len); // NOT validated (ttf-parser)
                                                // resolve_coords_len: walk the flags to size the coordinate arrays.
    let mut flags_left = u32::from(points_total);
    let (mut x_coords_len, mut y_coords_len) = (0u32, 0u32);
    let mut flags_off = off;
    while flags_left > 0 {
        let flags = match u8_at(glyph_data, flags_off) {
            Some(v) => v,
            None => return false,
        };
        flags_off += 1;
        let repeats = if flags & 0x08 != 0 {
            let repeats = match u8_at(glyph_data, flags_off) {
                Some(v) => u32::from(v) + 1,
                None => return false,
            };
            flags_off += 1;
            if repeats > flags_left {
                return false;
            }
            repeats
        } else {
            1
        };
        x_coords_len += u32::from(flags & 0x02 != 0) * repeats;
        x_coords_len += u32::from(flags & (0x02 | 0x10) == 0) * (repeats * 2);
        y_coords_len += u32::from(flags & 0x04 != 0) * repeats;
        y_coords_len += u32::from(flags & (0x04 | 0x20) == 0) * (repeats * 2);
        flags_left -= repeats;
    }
    let x_coords_off = flags_off;
    let y_coords_off = match x_coords_off.checked_add(x_coords_len as usize) {
        Some(v) => v,
        None => return false,
    };
    let end_off = match y_coords_off.checked_add(y_coords_len as usize) {
        Some(v) => v,
        None => return false,
    };
    if end_off > glyph_data.len() {
        return false;
    }
    let (mut prev_x, mut prev_y) = (0i16, 0i16);
    let (mut x_off, mut y_off) = (x_coords_off, y_coords_off);
    let (mut repeats_left, mut cur_flags) = (0u8, 0u8);
    let mut flags_read_off = off;
    // ttf-parser EndpointsIter::new: index = 1, left = endpoints[0].
    let mut endpoint_idx = 1usize;
    let mut contour_left = endpoints[0];
    for _ in 0..points_total {
        let last_point = if contour_left == 0 {
            if let Some(&end) = endpoints.get(endpoint_idx) {
                let prev = endpoints[endpoint_idx - 1];
                contour_left = end.saturating_sub(prev).saturating_sub(1);
            }
            endpoint_idx = endpoint_idx.saturating_add(1);
            true
        } else {
            contour_left -= 1;
            false
        };
        if repeats_left == 0 {
            cur_flags = match u8_at(glyph_data, flags_read_off) {
                Some(v) => v,
                None => return false,
            };
            flags_read_off += 1;
            if cur_flags & 0x08 != 0 {
                repeats_left = match u8_at(glyph_data, flags_read_off) {
                    Some(v) => v,
                    None => return false,
                };
                flags_read_off += 1;
            }
        } else {
            repeats_left -= 1;
        }
        let read_coord =
            |off: &mut usize, is_short: bool, is_same: bool, prev: &mut i16| -> Option<i16> {
                let n: i16 = if is_short {
                    let v = *glyph_data.get(*off)? as i16;
                    *off += 1;
                    if is_same {
                        v
                    } else {
                        -v
                    }
                } else if !is_same {
                    let v = i16_at(glyph_data, *off)?;
                    *off += 2;
                    v
                } else {
                    0
                };
                *prev = prev.wrapping_add(n);
                Some(*prev)
            };
        let x = match read_coord(
            &mut x_off,
            cur_flags & 0x02 != 0,
            cur_flags & 0x10 != 0,
            &mut prev_x,
        ) {
            Some(v) => v,
            None => return false,
        };
        let y = match read_coord(
            &mut y_off,
            cur_flags & 0x04 != 0,
            cur_flags & 0x20 != 0,
            &mut prev_y,
        ) {
            Some(v) => v,
            None => return false,
        };
        builder.push_point(
            f32::from(x),
            f32::from(y),
            cur_flags & 0x01 != 0,
            last_point,
        );
    }
    true
}

/// Component of a composite glyph; mirrors ttf-parser's
/// `CompositeGlyphIter`, including the quirk that matched-point (non-XY)
/// arguments do NOT consume the two argument bytes.
struct CompositeGlyphInfo {
    glyph_id: u16,
    transform: Transform,
}
struct CompositeGlyphIter<'a> {
    data: &'a [u8],
    off: usize,
}
impl<'a> Iterator for CompositeGlyphIter<'a> {
    type Item = Option<CompositeGlyphInfo>;
    fn next(&mut self) -> Option<Self::Item> {
        let flags = u16_at(self.data, self.off)?;
        self.off += 2;
        let glyph_id = u16_at(self.data, self.off)?;
        self.off += 2;
        let mut ts = Transform::default();
        if flags & 0x0002 != 0 {
            // args_are_xy_values
            if flags & 0x0001 != 0 {
                ts.e = i16_at(self.data, self.off)? as f32;
                self.off += 2;
                ts.f = i16_at(self.data, self.off)? as f32;
                self.off += 2;
            } else {
                ts.e = i8_at(self.data, self.off)? as f32;
                self.off += 1;
                ts.f = i8_at(self.data, self.off)? as f32;
                self.off += 1;
            }
        }
        if flags & 0x0080 != 0 {
            ts.a = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
            ts.b = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
            ts.c = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
            ts.d = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
        } else if flags & 0x0040 != 0 {
            ts.a = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
            ts.d = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
        } else if flags & 0x0008 != 0 {
            ts.a = f2dot14(i16_at(self.data, self.off)?);
            self.off += 2;
            ts.d = ts.a;
        }
        if flags & 0x0020 == 0 {
            self.off = self.data.len();
        }
        Some(Some(CompositeGlyphInfo {
            glyph_id,
            transform: ts,
        }))
    }
}

/// Mirrors ttf-parser's `glyf::outline_impl`; returns the calculated bbox.
fn outline_impl(
    font: &FontRef,
    data: &[u8],
    depth: u8,
    builder: &mut Builder,
) -> Option<Option<(i16, i16, i16, i16)>> {
    if depth >= MAX_COMPONENTS {
        return None;
    }
    let number_of_contours = i16_at(data, 0)?;
    if number_of_contours > 0 {
        parse_simple_outline(data, number_of_contours as u16, builder);
    } else if number_of_contours < 0 {
        for comp in (CompositeGlyphIter { data, off: 10 }) {
            let comp = comp?;
            let Some(child_data) = glyph_bytes(font, comp.glyph_id) else {
                continue;
            };
            let transform = Transform::combine(builder.transform, comp.transform);
            let mut b = Builder::new(transform, builder.bbox, builder.builder);
            outline_impl(font, child_data, depth + 1, &mut b)?;
            builder.bbox = b.bbox;
        }
    }
    if builder.bbox.is_default() {
        return Some(None);
    }
    Some(builder.bbox.to_rect())
}

/// Unscaled, unpositioned glyph outline.
#[derive(Clone, Debug)]
struct Outline {
    /// Unscaled bounding box (y-flipped into top-left space, like ab_glyph).
    bounds: (f32, f32, f32, f32),
    curves: Vec<OutlineCurve>,
}

/// Scale factors matching ab_glyph's `PxScaleFactor`.
#[derive(Clone, Copy, Debug)]
struct PxScaleFactor {
    horizontal: f32,
    vertical: f32,
}

/// Raw `glyf` record bytes for a glyph id, mirroring ttf-parser's
/// `loca::glyph_range` semantics (Short offsets ×2; empty range → None).
fn glyph_bytes<'a>(font: &FontRef<'a>, gid: u16) -> Option<&'a [u8]> {
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;
    let start = loca.get_raw(gid as usize)? as usize;
    let end = loca.get_raw(gid as usize + 1)? as usize;
    if start >= end {
        return None;
    }
    glyf.offset_data().as_bytes().get(start..end)
}

/// Extracts the `glyf` outline for `glyph_id`, mirroring
/// `ab_glyph::FontVec::outline` (y-flip and empty-bbox filter included).
fn outline(font: &FontRef, glyph_id: u16) -> Option<Outline> {
    let glyph_data = glyph_bytes(font, glyph_id)?;
    let mut curve_builder = OutlineCurveBuilder::default();
    let mut builder = Builder::new(Transform::default(), RectF::new(), &mut curve_builder);
    let rect = outline_impl(font, glyph_data, 0, &mut builder)??;
    let curves = curve_builder.take_outline();
    // ab_glyph filters the UNFLIPPED ttf-parser rect before the y-flip.
    if rect.0 >= rect.2 || rect.1 >= rect.3 {
        return None;
    }
    let bounds = (
        f32::from(rect.0),
        f32::from(rect.3),
        f32::from(rect.2),
        f32::from(rect.1),
    );
    Some(Outline { bounds, curves })
}

/// ab_glyph `Outline::px_bounds` math (subpixel fractions included).
fn px_bounds(
    bounds: (f32, f32, f32, f32),
    scale_factor: PxScaleFactor,
    position: (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    let (px, py) = position;
    let (x_trunc, x_fract) = (px.trunc(), px.fract());
    let (y_trunc, y_fract) = (py.trunc(), py.fract());
    (
        (
            (bounds.0 * scale_factor.horizontal + x_fract).floor() + x_trunc,
            (bounds.1 * -scale_factor.vertical + y_fract).floor() + y_trunc,
        ),
        (
            (bounds.2 * scale_factor.horizontal + x_fract).ceil() + x_trunc,
            (bounds.3 * -scale_factor.vertical + y_fract).ceil() + y_trunc,
        ),
    )
}

fn addp(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 + b.0, a.1 + b.1)
}

/// ab_glyph `OutlinedGlyph::draw` math feeding the same
/// `ab_glyph_rasterizer` calls.
fn draw_outline<O: FnMut(u32, u32, f32)>(
    outline: &Outline,
    scale_factor: PxScaleFactor,
    position: (f32, f32),
    o: O,
) {
    let h_factor = scale_factor.horizontal;
    let v_factor = -scale_factor.vertical;
    let (px_min, px_max) = px_bounds(outline.bounds, scale_factor, position);
    let offset = (position.0 - px_min.0, position.1 - px_min.1);
    let w = (px_max.0 - px_min.0) as usize;
    let h = (px_max.1 - px_min.1) as usize;
    let scale_up = |p: &(f32, f32)| (p.0 * h_factor, p.1 * v_factor);
    let mut rasterizer = Rasterizer::new(w, h);
    for curve in &outline.curves {
        match curve {
            OutlineCurve::Line(p0, p1) => {
                let (a, b) = (addp(scale_up(p0), offset), addp(scale_up(p1), offset));
                rasterizer.draw_line(point(a.0, a.1), point(b.0, b.1));
            }
            OutlineCurve::Quad(p0, p1, p2) => {
                let (a, b, c) = (
                    addp(scale_up(p0), offset),
                    addp(scale_up(p1), offset),
                    addp(scale_up(p2), offset),
                );
                rasterizer.draw_quad(point(a.0, a.1), point(b.0, b.1), point(c.0, c.1));
            }
            OutlineCurve::Cubic(p0, p1, p2, p3) => {
                let (a, b, c, d) = (
                    addp(scale_up(p0), offset),
                    addp(scale_up(p1), offset),
                    addp(scale_up(p2), offset),
                    addp(scale_up(p3), offset),
                );
                rasterizer.draw_cubic(
                    point(a.0, a.1),
                    point(b.0, b.1),
                    point(c.0, c.1),
                    point(d.0, d.1),
                );
            }
        }
    }
    rasterizer.for_each_pixel_2d(o);
}

/// The outline of a glyph for a TrueType (glyf) font, with the exact
/// `ab_glyph` pixel-bounds / draw semantics.
pub(crate) struct GlyfOutline {
    outline: Outline,
    scale_factor: PxScaleFactor,
    position: (f32, f32),
    px_min: (f32, f32),
    px_max: (f32, f32),
}
impl GlyfOutline {
    /// Builds a glyph outline for `glyph_id` at `font_size`, positioned at
    /// `(x, y)` — replicating `ab_glyph::FontVec::outline_glyph` +
    /// `ab_glyph::Glyph::with_scale_and_position` exactly.
    pub(crate) fn new(
        font: &FontRef,
        glyph_id: u16,
        font_size: f32,
        height_unscaled: f32,
        position: (f32, f32),
    ) -> Option<Self> {
        // Guard against malformed fonts whose hhea ascender equals the
        // descender: a zero divisor would produce infinite scale factors.
        if !(height_unscaled > 0.0) {
            return None;
        }
        let outline = outline(font, glyph_id)?;
        let scale_factor = PxScaleFactor {
            horizontal: font_size / height_unscaled,
            vertical: font_size / height_unscaled,
        };
        let (px_min, px_max) = px_bounds(outline.bounds, scale_factor, position);
        Some(GlyfOutline {
            outline,
            scale_factor,
            position,
            px_min,
            px_max,
        })
    }
    /// Conservative whole-pixel bounds (same as `OutlinedGlyph::px_bounds`).
    pub(crate) fn px_bounds(&self) -> ((f32, f32), (f32, f32)) {
        (self.px_min, self.px_max)
    }
    /// Per-pixel `(x, y, coverage)` callback, byte-identical to
    /// `ab_glyph::OutlinedGlyph::draw`.
    pub(crate) fn draw(&self, o: impl FnMut(u32, u32, f32)) {
        draw_outline(&self.outline, self.scale_factor, self.position, o)
    }
    /// Deterministic outline dump used by the golden differential tests
    /// (fixtures captured from the ab_glyph 0.2.32 reference path).
    /// Test-only: compiled out of production builds.
    #[cfg(test)]
    pub(crate) fn dump_repr(&self) -> String {
        let (x_min, y_min, x_max, y_max) = self.outline.bounds;
        let mut s = format!(
            "bounds {:?} {:?} {:?} {:?}
ncurves {}
",
            x_min,
            y_min,
            x_max,
            y_max,
            self.outline.curves.len()
        );
        for c in &self.outline.curves {
            match c {
                OutlineCurve::Line(p0, p1) => s.push_str(&format!(
                    "L {:?} {:?} {:?} {:?}
",
                    p0.0, p0.1, p1.0, p1.1
                )),
                OutlineCurve::Quad(p0, p1, p2) => s.push_str(&format!(
                    "Q {:?} {:?} {:?} {:?} {:?} {:?}
",
                    p0.0, p0.1, p1.0, p1.1, p2.0, p2.1
                )),
                OutlineCurve::Cubic(p0, p1, p2, p3) => s.push_str(&format!(
                    "C {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}
",
                    p0.0, p0.1, p1.0, p1.1, p2.0, p2.1, p3.0, p3.1
                )),
            }
        }
        let ((min_x, min_y), (max_x, max_y)) = self.px_bounds();
        s.push_str(&format!(
            "px {:?} {:?} {:?} {:?}
",
            min_x, min_y, max_x, max_y
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLED_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");

    #[test]
    fn zero_height_returns_none() {
        let font = FontRef::new(BUNDLED_FONT).expect("bundled font valid");
        assert!(GlyfOutline::new(&font, 43, 24.0, 0.0, (0.0, 0.0)).is_none());
        assert!(GlyfOutline::new(&font, 43, 24.0, -1.0, (0.0, 0.0)).is_none());
    }
}
