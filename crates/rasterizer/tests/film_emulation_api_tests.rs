//! Regression tests for the film_emulation public-API refactor.
//!
//! The nine-argument `apply_film_emulation` entry point became a deprecated
//! compatibility shim over the new `FilmEmulation` settings struct and
//! `apply_film_emulation_with`. These tests pin the two entry points to
//! byte-identical pixel output and pin the settings defaults.

use rust_motion_rasterizer::film_emulation::{apply_film_emulation_with, FilmEmulation};
use tiny_skia::Pixmap;

/// Deterministic synthetic pixmap: linear-ish gradient plus a bright centre
/// region that triggers halation.
fn make_pixmap() -> Pixmap {
    let mut pix = Pixmap::new(96, 64).expect("pixmap");
    for y in 0..64u32 {
        for x in 0..96u32 {
            let v = (((x * 7 + y * 13) % 256) as u8) ^ ((x * y % 256) as u8);
            let bright = (x as i32 - 48).abs() < 6 && (y as i32 - 32).abs() < 6;
            let (r, g, b) = if bright {
                (255u8, 240u8, 220u8)
            } else {
                (v, v.wrapping_mul(3), 255u8 - v)
            };
            pix.pixels_mut()[(y * 96 + x) as usize] =
                tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, 255).expect("color");
        }
    }
    pix
}

fn settings() -> FilmEmulation {
    FilmEmulation {
        grain_amount: 60.0,
        grain_size: 30.0,
        grain_roughness: 45.0,
        grain_seed: 1234,
        halation_intensity: 1.2,
        halation_threshold: 0.6,
        halation_radius: 14.0,
        halation_color: [255, 60, 20, 255],
    }
}

/// The deprecated nine-argument shim must produce byte-identical pixels to
/// the new settings-based API for the same parameters.
/// The deprecated shim itself is the subject of this test: allow the
/// deprecation lint for exactly this one test so the compatibility wrapper
/// keeps being exercised.
#[test]
#[allow(deprecated)]
fn apply_film_emulation_with_matches_legacy_shim_byte_for_byte() {
    let s = settings();
    let mut via_new = make_pixmap();
    let mut via_shim = make_pixmap();
    apply_film_emulation_with(&mut via_new, &s);
    rust_motion_rasterizer::film_emulation::apply_film_emulation(
        &mut via_shim,
        s.grain_amount,
        s.grain_size,
        s.grain_roughness,
        s.grain_seed,
        s.halation_intensity,
        s.halation_threshold,
        s.halation_radius,
        s.halation_color,
    );
    assert_eq!(
        via_new.data(),
        via_shim.data(),
        "legacy shim must produce byte-identical pixels to the settings API"
    );
}

/// The settings API must be deterministic for identical inputs.
#[test]
fn apply_film_emulation_with_is_deterministic() {
    let s = settings();
    let mut a = make_pixmap();
    let mut b = make_pixmap();
    apply_film_emulation_with(&mut a, &s);
    apply_film_emulation_with(&mut b, &s);
    assert_eq!(a.data(), b.data());
}

/// Non-trivial grain + halation settings must visibly modify the pixels, so
/// the byte-identity tests above are not vacuously comparing no-ops.
#[test]
fn film_emulation_settings_produce_visible_change() {
    let s = settings();
    let mut pix = make_pixmap();
    let before = pix.data().to_vec();
    apply_film_emulation_with(&mut pix, &s);
    assert_ne!(
        before,
        pix.data(),
        "grain + halation settings must modify the pixels"
    );
}

/// Default settings disable both effects and must leave pixels untouched.
#[test]
fn default_film_emulation_is_a_noop() {
    let mut pix = make_pixmap();
    let before = pix.data().to_vec();
    apply_film_emulation_with(&mut pix, &FilmEmulation::default());
    assert_eq!(before, pix.data());
}

/// The settings defaults mirror the serde defaults of
/// `SceneFilter::FilmEmulation` in the scene module.
#[test]
fn film_emulation_defaults_match_scene_filter_serde_defaults() {
    let d = FilmEmulation::default();
    assert_eq!(d.grain_amount, 0.0);
    assert_eq!(d.grain_size, 25.0);
    assert_eq!(d.grain_roughness, 50.0);
    assert_eq!(d.grain_seed, 0);
    assert_eq!(d.halation_intensity, 0.0);
    assert_eq!(d.halation_threshold, 0.75);
    assert_eq!(d.halation_radius, 12.0);
    assert_eq!(d.halation_color, [255, 60, 20, 255]);
}
