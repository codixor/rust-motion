//! CI quality and robustness gate for the standalone H.264 encoder.
//!
//! These tests use `rust-motion-h264enc` directly; the renderer crate is not
//! involved. The quality test needs `ffmpeg` on PATH and skips with an explicit
//! message when it is absent (the CI job installs it and therefore always runs
//! the assertion).

use rust_motion_h264enc::{Encoder, EncoderConfig, RateControl, YuvFrame};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Deterministic RGBA fixture: gradients plus moving blocks make the encode
/// non-trivial without depending on a checked-in image.
fn rgba_fixture(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        for x in 0..width {
            let r = ((x * 255) / width.max(1)) as u8;
            let g = ((y * 255) / height.max(1)) as u8;
            let b = (((x + y) * 7) % 256) as u8;
            let checker = if ((x / 17) + (y / 13)) % 2 == 0 {
                255
            } else {
                32
            };
            rgba.push(r.saturating_add(checker / 4));
            rgba.push(g.saturating_add(checker / 4));
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

fn encode_fixture(
    width: u32,
    height: u32,
    frames: usize,
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>), String> {
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

#[test]
fn encoder_meets_crf20_psnr_gate() {
    let width = 320;
    let height = 180;
    let (bitstream, y, u, v) =
        encode_fixture(width, height, 4).expect("CRF 20 fixture must encode");

    let ffmpeg = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()
        .filter(|output| output.status.success());
    if ffmpeg.is_none() {
        eprintln!("SKIP: ffmpeg is not installed; CI installs it and runs this gate");
        return;
    }

    let dir = std::env::temp_dir().join(format!("rust-motion-h264-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("source.i420");
    let encoded = dir.join("encoded.h264");
    let stats = dir.join("psnr.log");
    let mut i420 = Vec::with_capacity(y.len() + u.len() + v.len());
    i420.extend_from_slice(&y);
    i420.extend_from_slice(&u);
    i420.extend_from_slice(&v);
    std::fs::write(&source, &i420).expect("write source");
    std::fs::write(&encoded, &bitstream).expect("write bitstream");

    let filter = format!("[0:v][1:v]psnr=stats_file={}", stats.display());
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{width}x{height}"),
            "-i",
        ])
        .arg(&source)
        .arg("-i")
        .arg(&encoded)
        .args(["-lavfi", &filter, "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = std::fs::read_to_string(&stats).expect("psnr stats file");
    let average = log
        .lines()
        .find_map(|line| line.split("average:").nth(1))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .expect("ffmpeg psnr average");
    assert!(
        average >= 38.0,
        "CRF 20 PSNR {average:.2} dB is below the 38 dB gate"
    );

    let _ = std::fs::remove_dir_all(&dir);
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
