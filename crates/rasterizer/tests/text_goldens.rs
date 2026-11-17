//! Golden-output regression tests for the typography pipeline and still-image formats.
//!
//! These fixtures were captured BEFORE the H-1 dependency remediation
//! (rustybuzz/ab_glyph -> harfrust/skrifa) and MUST remain byte-identical
//! afterwards. If a golden fails after a dependency change, investigate the
//! difference instead of regenerating the fixture.
//!
//! Fixtures can be (re)written with GOLDEN_UPDATE=1 during initial capture
//! only. The committed fixtures are the source of truth.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_motion_rasterizer::backend::{FrameConfig, RasterizerBackend};
use rust_motion_rasterizer::scene::{ImageFit, Scene, SceneNode};
use rust_motion_rasterizer::text_layout::{
    segment_bidi_runs, shaper::shape_bidi_runs, FontResolver, TextStyle, TextLayoutEngine,
    TextDirection,
};
use rust_motion_rasterizer::{Color, FontCache, TinySkiaBackend};

use image::ImageEncoder;

const FIXTURES: &str = "tests/fixtures";

const SHAPING_GOLDEN: &str = "text_shaping.golden";
const LAYOUT_GOLDEN: &str = "text_layout.golden";
const FORMAT_PNG: &str = "format_png.png";
const FORMAT_JPEG: &str = "format_jpeg.jpg";
const FORMAT_WEBP: &str = "format_webp.webp";
const FORMAT_GIF: &str = "format_gif.gif";
const SVG_SOURCE: &str = "svg_fixture.svg";
const SVG_RENDER: &str = "svg_render.png";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES).join(name)
}

fn update_enabled() -> bool {
    std::env::var("GOLDEN_UPDATE").is_ok_and(|v| v == "1")
}

/// Deterministic rendering of the shaping stage for a fixed corpus.
///
/// Covers Latin, GSUB ligatures (complex shaping), RTL Arabic, mixed
/// bidi, cross-script fallback (tofu glyphs), and font-size scaling.
fn capture_shaping() -> String {
    let cache = FontCache::bundled();
    let resolver = FontResolver::new(&cache, &[]).expect("bundled font resolver");

    let cases: Vec<(&str, &str, f32)> = vec![
        ("latin", "Hello World", 32.0),
        ("latin_ligatures", "office afford cliff", 32.0),
        ("rtl_arabic", "مرحبا بالعالم", 32.0),
        ("mixed_bidi", "Hello مرحبا World 123", 32.0),
        ("fallback_tofu", "Hello 你好 World", 32.0),
        ("scale_16", "Scaling Test", 16.0),
        ("scale_64", "Scaling Test", 64.0),
    ];

    let mut out = String::new();
    for (name, text, font_size) in cases {
        let style = TextStyle::new(font_size);
        let runs = segment_bidi_runs(text, TextDirection::Auto);
        let shaped = shape_bidi_runs(text, &runs, &resolver, &style)
            .unwrap_or_else(|e| panic!("shaping '{name}' failed: {e}"));
        out.push_str(&format!("case {name}
"));
        for (i, run) in shaped.iter().enumerate() {
            out.push_str(&format!(
                "  run {i} font={} rtl={} range={}..{} width={:.6}
",
                run.font_index, run.is_rtl, run.source_range.start, run.source_range.end, run.width
            ));
            for g in &run.glyphs {
                out.push_str(&format!(
                    "    g {} adv={:.6} xoff={:.6} yoff={:.6} cluster={}
",
                    g.glyph_id, g.x_advance, g.x_offset, g.y_offset, g.cluster
                ));
            }
        }
    }
    out
}

/// Deterministic rendering of the full layout stage: multiline wrapping,
/// alignment, CJK line breaking, and RTL paragraph layout.
fn capture_layout() -> String {
    let cache = Arc::new(FontCache::bundled());
    let engine = TextLayoutEngine::new(cache);

    struct Case {
        name: &'static str,
        text: &'static str,
        style: TextStyle,
    }

    let cases = vec![
        Case {
            name: "multiline_wrap",
            text: "This is a long sentence rendered by the native typography engine to verify wrapping.",
            style: TextStyle::new(24.0).with_max_width(180.0),
        },
        Case {
            name: "alignment_center",
            text: "Aligned",
            style: TextStyle::new(24.0)
                .with_max_width(500.0)
                .with_align(rust_motion_rasterizer::text_layout::TextAlignment::Center),
        },
        Case {
            name: "alignment_right",
            text: "Aligned",
            style: TextStyle::new(24.0)
                .with_max_width(500.0)
                .with_align(rust_motion_rasterizer::text_layout::TextAlignment::Right),
        },
        Case {
            name: "cjk_wrap",
            text: "안녕하세요 반값습니다 비디오 렌더러 테스트 문장입니다.",
            style: TextStyle::new(20.0).with_max_width(120.0),
        },
        Case {
            name: "rtl_layout",
            text: "مرحبا بالعالم",
            style: TextStyle::new(32.0),
        },
    ];

    let mut out = String::new();
    for case in cases {
        let layout = engine
            .layout(case.text, &case.style)
            .unwrap_or_else(|e| panic!("layout '{}' failed: {e}", case.name));
        out.push_str(&format!(
            "case {} width={:.6} height={:.6}
",
            case.name, layout.width, layout.height
        ));
        for (i, line) in layout.lines.iter().enumerate() {
            out.push_str(&format!(
                "  line {i} x={:.6} baseline_y={:.6} ascent={:.6} descent={:.6} line_height={:.6} width={:.6}
",
                line.x, line.baseline_y, line.ascent, line.descent, line.line_height, line.width
            ));
            for g in &line.glyphs {
                out.push_str(&format!(
                    "    g {} x={:.6} y={:.6} size={:.6} cluster={}
",
                    g.glyph_id, g.x, g.y, g.font_size, g.cluster
                ));
            }
        }
    }
    out
}

/// Deterministic 64x64 RGBA pattern used for codec goldens.
fn sample_rgba() -> Vec<u8> {
    let (w, h) = (64u32, 64u32);
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let (r, g, b) = (
                ((x * 7 + y * 3) % 256) as u8,
                ((x * 2 + y * 11) % 256) as u8,
                ((x * 5 + y * 17 + 64) % 256) as u8,
            );
            // A few opaque geometric accents keep codecs honest.
            let a = if (x >= 8 && x < 20 && y >= 8 && y < 24) || (x >= 40 && y >= 40) {
                255u8
            } else {
                200u8
            };
            pixels[idx] = r;
            pixels[idx + 1] = g;
            pixels[idx + 2] = b;
            pixels[idx + 3] = a;
        }
    }
    pixels
}

fn encode_png(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(pixels, w, h, image::ExtendedColorType::Rgba8)
        .expect("png encode");
    out
}

fn encode_jpeg(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let rgba = image::RgbaImage::from_raw(w, h, pixels.to_vec()).expect("rgba image");
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&rgba)
        .expect("jpeg encode");
    out
}

fn encode_webp(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(pixels, w, h, image::ExtendedColorType::Rgba8)
        .expect("webp encode");
    out
}

fn encode_gif(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame};
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(&mut out, 10);
        encoder.set_repeat(Repeat::Infinite).expect("gif repeat");
        let rgba = image::RgbaImage::from_raw(w, h, pixels.to_vec()).expect("rgba image");
        let frame = Frame::from_parts(rgba, 0, 0, Delay::from_numer_denom_ms(40, 1));
        encoder.encode_frame(frame).expect("gif frame");
    }
    out
}

fn render_svg_fixture() -> Vec<u8> {
    let svg = std::fs::read(fixture_path(SVG_SOURCE)).expect("svg fixture");
    let mut scene = Scene::new();
    scene.push(SceneNode::Rect {
        x: 0.0,
        y: 0.0,
        w: 160.0,
        h: 120.0,
        fill: Color::rgb(240, 240, 240),
        stroke: None,
        stroke_width: 0.0,
        corner_radius: 0.0,
    });
    scene.push(SceneNode::ImageBytes {
        bytes: Arc::new(svg),
        x: 0.0,
        y: 0.0,
        w: 160.0,
        h: 120.0,
        fit: ImageFit::Fill,
        opacity: 1.0,
    });
    let backend = TinySkiaBackend::new();
    let config = FrameConfig::new(160, 120, 0, 30.0);
    let img = backend
        .render_frame(&scene, &config)
        .expect("svg render frame");
    encode_png(img.as_raw(), img.width(), img.height())
}

fn check_fixture(name: &str, actual: &[u8]) {
    let path = fixture_path(name);
    if update_enabled() {
        std::fs::write(&path, actual).expect("write golden fixture");
        println!("wrote fixture {}", path.display());
        return;
    }
    let expected = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("missing golden fixture {}: {e}", path.display()));
    assert_eq!(
        expected.len(),
        actual.len(),
        "golden length mismatch for {}: fixture {} bytes, actual {} bytes",
        name,
        expected.len(),
        actual.len()
    );
    if expected != actual {
        let first_diff = expected
            .iter()
            .zip(actual.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(0);
        panic!(
            "golden byte mismatch for {} at byte {}: fixture {:#04x?}, actual {:#04x?} (investigate, do not regenerate)",
            name,
            first_diff,
            expected.get(first_diff),
            actual.get(first_diff)
        );
    }
}

#[test]
fn text_shaping_golden() {
    let actual = capture_shaping();
    check_fixture(SHAPING_GOLDEN, actual.as_bytes());
    assert!(!actual.is_empty());
    assert!(actual.contains("case latin"));
    assert!(actual.contains("case rtl_arabic"));
    assert!(actual.contains("case fallback_tofu"));
}

#[test]
fn text_layout_golden() {
    let actual = capture_layout();
    check_fixture(LAYOUT_GOLDEN, actual.as_bytes());
    assert!(!actual.is_empty());
    assert!(actual.contains("case multiline_wrap"));
    assert!(actual.contains("case cjk_wrap"));
    assert!(actual.contains("case rtl_layout"));
}

#[test]
fn image_format_goldens() {
    let pixels = sample_rgba();
    check_fixture(FORMAT_PNG, &encode_png(&pixels, 64, 64));
    check_fixture(FORMAT_JPEG, &encode_jpeg(&pixels, 64, 64));
    check_fixture(FORMAT_WEBP, &encode_webp(&pixels, 64, 64));
    check_fixture(FORMAT_GIF, &encode_gif(&pixels, 64, 64));
}

#[test]
fn svg_render_golden() {
    let rendered = render_svg_fixture();
    check_fixture(SVG_RENDER, &rendered);
}
