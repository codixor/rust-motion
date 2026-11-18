//! CI quality and robustness gate for the standalone H.264 encoder.
//!
//! These tests use `rust-motion-h264enc` directly; the renderer crate is not
//! involved. The quality test needs `ffmpeg` on PATH and skips with an explicit
//! message when it is absent (the CI job installs it and therefore always runs
//! the assertion).

use rust_motion_h264enc::{Encoder, EncoderConfig, RateControl, YuvFrame};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Deterministic RGBA fixture: smooth RGB gradients exercise the colour
/// conversion, prediction and transform without high-frequency noise that no
/// CRF 20 encoder can reproduce at the gate's PSNR threshold.
fn rgba_fixture(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
    let w = width.max(1);
    let h = height.max(1);
    for y in 0..height {
        for x in 0..width {
            let r = ((x * 255) / w) as u8;
            let g = ((y * 255) / h) as u8;
            let b = (((x + y) * 255) / (w + h)) as u8;
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            rgba.push(255);
        }
    }
    rgba
}

/// BT.601 limited-range RGB -> planar I420, matching ffmpeg's yuv420p input.
fn rgba_to_i420(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let mut y_plane = vec![0u8; w * h];
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut u_plane = vec![128u8; cw * ch];
    let mut v_plane = vec![128u8; cw * ch];

    for row in 0..h {
        for col in 0..w {
            let i = (row * w + col) * 4;
            let r = f32::from(rgba[i]);
            let g = f32::from(rgba[i + 1]);
            let b = f32::from(rgba[i + 2]);
            let y = 16.0 + 0.257 * r + 0.504 * g + 0.098 * b;
            y_plane[row * w + col] = y.round().clamp(0.0, 255.0) as u8;
        }
    }
    for row in 0..ch {
        for col in 0..cw {
            let mut u_sum = 0.0f32;
            let mut v_sum = 0.0f32;
            let mut count = 0.0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let x = col * 2 + dx;
                    let y = row * 2 + dy;
                    if x >= w || y >= h {
                        continue;
                    }
                    let i = (y * w + x) * 4;
                    let r = f32::from(rgba[i]);
                    let g = f32::from(rgba[i + 1]);
                    let b = f32::from(rgba[i + 2]);
                    u_sum += -0.148 * r - 0.291 * g + 0.439 * b + 128.0;
                    v_sum += 0.439 * r - 0.368 * g - 0.071 * b + 128.0;
                    count += 1.0;
                }
            }
            if count > 0.0 {
                u_plane[row * cw + col] = (u_sum / count).round().clamp(0.0, 255.0) as u8;
                v_plane[row * cw + col] = (v_sum / count).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    (y_plane, u_plane, v_plane)
}

/// Encoded payload: (bitstream, y_plane, u_plane, v_plane).
type EncodedFixture = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

fn encode_fixture(width: u32, height: u32, frames: usize) -> Result<EncodedFixture, String> {
    let mut cfg = EncoderConfig::new(width, height, 30, 1);
    cfg.rate = RateControl::Crf(20.0);
    cfg.bframes = 0;
    cfg.keyint = 30;
    let mut encoder = Encoder::new(cfg).map_err(|error| format!("encoder init: {error:?}"))?;
    let rgba = rgba_fixture(width, height);
    let (y, u, v) = rgba_to_i420(&rgba, width, height);
    let y_stride = width as usize;
    let uv_stride = (width as usize).div_ceil(2);

    let mut bitstream = Vec::new();
    for frame in 0..frames {
        let yuv = YuvFrame {
            y: &y,
            u: &u,
            v: &v,
            y_stride,
            uv_stride,
        };
        let packets = encoder
            .encode(&yuv, frame as i64)
            .map_err(|error| format!("encode: {error:?}"))?;
        for packet in packets {
            bitstream.extend_from_slice(&packet.data);
        }
    }
    for packet in encoder.flush() {
        bitstream.extend_from_slice(&packet.data);
    }
    Ok((bitstream, y, u, v))
}

fn has_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Extracts a numeric metric value from one whitespace-delimited token.
///
/// FFmpeg writes the PSNR summary as either a plain log line
/// (`... average:40.75 min:...`) or a filter-prefixed line
/// (`[Parsed_psnr_0 @ ...] ... average:40.75`). Both put the value in the same
/// `average:VALUE` token, so the parser only needs to trim trailing
/// punctuation such as a comma or closing bracket.
fn metric_value(token: &str, key: &str) -> Option<f64> {
    let value = token.strip_prefix(key)?;
    let value = value.trim_end_matches(|character: char| {
        character.is_ascii_punctuation() && !matches!(character, '.' | '-' | '+')
    });
    value.parse::<f64>().ok()
}

fn metric_values(text: &str, key: &str) -> Vec<f64> {
    text.split_whitespace()
        .filter_map(|token| metric_value(token, key))
        .collect()
}

fn snippet(text: &str) -> String {
    let mut output: String = text.chars().take(4000).collect();
    if text.chars().count() > 4000 {
        output.push_str("\n...[truncated]");
    }
    output
}

fn captured_output(stdout: &str, stderr: &str, stats: &str) -> String {
    format!(
        "ffmpeg stdout:\n{}\nffmpeg stderr:\n{}\npsnr stats:\n{}",
        snippet(stdout),
        snippet(stderr),
        snippet(stats)
    )
}

/// Parses the PSNR average from FFmpeg stdout/stderr, then falls back to the
/// stats file. The stats-file format is one line per frame with
/// `psnr_avg:VALUE`; its arithmetic mean is the same summary average for a
/// constant frame count. Inf/NaN are reported as test failures, never accepted.
fn parse_psnr_average(stdout: &str, stderr: &str, stats: &str) -> Result<f64, String> {
    for (label, text) in [("stderr", stderr), ("stdout", stdout), ("stats", stats)] {
        if let Some(value) = metric_values(text, "average:").last().copied() {
            if value.is_finite() {
                return Ok(value);
            }
            return Err(format!(
                "ffmpeg reported a non-finite PSNR average ({value}) in {label}.\n{}",
                captured_output(stdout, stderr, stats)
            ));
        }
    }

    let per_frame = metric_values(stats, "psnr_avg:");
    if per_frame.is_empty() {
        return Err(format!(
            "could not parse an FFmpeg PSNR average from stdout, stderr or the stats file.\n{}",
            captured_output(stdout, stderr, stats)
        ));
    }
    if let Some(value) = per_frame.iter().find(|value| !value.is_finite()) {
        return Err(format!(
            "ffmpeg reported a non-finite per-frame PSNR ({value}) in the stats file.\n{}",
            captured_output(stdout, stderr, stats)
        ));
    }
    let mean = per_frame.iter().sum::<f64>() / per_frame.len() as f64;
    Ok(mean)
}

#[test]
fn psnr_parser_handles_log_and_filter_formats() {
    let log = "PSNR y:40.10 u:41.00 v:40.90 average:40.75 min:39.80 max:42.00";
    assert_eq!(parse_psnr_average("", log, ""), Ok(40.75));

    let filter =
        "[Parsed_psnr_0 @ 0x1234] PSNR y:40.10 u:41.00 v:40.90 average:41.25 min:39.80 max:42.00";
    assert_eq!(parse_psnr_average(filter, "", ""), Ok(41.25));

    let stats = "n:1 mse_avg:1.00 psnr_avg:40.0 mse_y:1.0 mse_u:1.0 mse_v:1.0\n                 n:2 mse_avg:2.00 psnr_avg:42.0 mse_y:2.0 mse_u:2.0 mse_v:2.0";
    assert_eq!(parse_psnr_average("", "", stats), Ok(41.0));
}

#[test]
fn psnr_parser_reports_unparseable_and_non_finite_output() {
    let unparseable = parse_psnr_average("no psnr here", "still no psnr", "n:1 mse_avg:1.0")
        .expect_err("missing average must be an error");
    assert!(unparseable.contains("could not parse"));

    let non_finite = parse_psnr_average("", "PSNR average:inf min:0 max:0", "")
        .expect_err("inf must fail the gate");
    assert!(non_finite.contains("non-finite"));
    assert!(non_finite.contains("average:inf"));
}

fn psnr(source: &[u8], decoded: &[u8]) -> f64 {
    debug_assert_eq!(source.len(), decoded.len());
    let mut sum = 0.0f64;
    for (expected, actual) in source.iter().zip(decoded) {
        let delta = f64::from(*expected) - f64::from(*actual);
        sum += delta * delta;
    }
    let mse = sum / source.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[test]
fn encoder_meets_crf20_psnr_gate() -> Result<(), String> {
    let width = 320;
    let height = 180;
    let (bitstream, y, u, v) =
        encode_fixture(width, height, 4).expect("CRF 20 fixture must encode");

    if !has_ffmpeg() {
        eprintln!("SKIP: ffmpeg is not installed; the dedicated CI gate installs it");
        return Ok(());
    }

    let dir = std::env::temp_dir().join(format!("rust-motion-h264-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let encoded = dir.join("encoded.h264");
    let decoded = dir.join("decoded.i420");
    std::fs::write(&encoded, &bitstream).expect("write bitstream");

    // Decode to raw I420 and compare the planes ourselves. An ffmpeg PSNR
    // filter can silently insert a range/format conversion; the raw bytes are
    // what the decoder actually produced.
    let output = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&encoded)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&decoded)
        .output()
        .map_err(|error| format!("could not run ffmpeg: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let decoded_bytes = std::fs::read(&decoded)
        .map_err(|error| format!("could not read decoded frames: {error}"))?;
    let frame_size = y.len() + u.len() + v.len();
    if frame_size == 0 || decoded_bytes.len() % frame_size != 0 {
        return Err(format!(
            "ffmpeg decoded {} bytes, not a multiple of the {frame_size}-byte I420 frame",
            decoded_bytes.len()
        ));
    }
    let frames = decoded_bytes.len() / frame_size;
    let mut total = 0.0;
    let mut samples = 0usize;
    for frame in 0..frames {
        let base = frame * frame_size;
        for (src, offset, len) in [
            (&y, 0, y.len()),
            (&u, y.len(), u.len()),
            (&v, y.len() + u.len(), v.len()),
        ] {
            let start = base + offset;
            total += psnr(src, &decoded_bytes[start..start + len]);
            samples += 1;
        }
    }
    let average = total / samples as f64;
    assert!(
        average >= 38.0,
        "CRF 20 PSNR {average:.2} dB is below the 38 dB gate.\n\
         decoded frames: {frames}, frame bytes: {frame_size}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn encoder_never_panics_on_odd_dimensions() {
    for (width, height) in [
        (1u32, 1u32),
        (4095, 1),
        (4095, 3),
        (33, 17),
        (17, 33),
        (3, 4095),
    ] {
        let encoded = catch_unwind(AssertUnwindSafe(|| encode_fixture(width, height, 1)));
        assert!(
            encoded.is_ok(),
            "encoder panicked for {width}x{height}; it must return an error instead"
        );
        let _ = encoded.expect("checked above");
    }
}
