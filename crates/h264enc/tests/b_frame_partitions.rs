//! Regression test for the B-frame 16x8/8x16 partition search
//! (`b_partition`).
//!
//! A previous `needless_range_loop` rewrite enumerated the full 4-wide
//! `mvs`/`dirs` arrays inside `b_partition`, although B 16x8/8x16
//! partitions have exactly two rects. `Part::P16x8.rect(2)` yields
//! y = 16 (past the 256-byte MB luma block), so the extra iterations hit
//! out-of-bounds slice indexing and corrupted the prediction.
//!
//! This fixture forces the partition path: high-detail deterministic noise
//! with a moving edge keeps the 16x16 search cost above the `256 * 4`
//! partition threshold, the Balanced preset enables partitions, and
//! `bframes = 2` produces B pictures. The test proves a panic-free encode
//! that emits real B frames and decodes back to the exact frame count with
//! ffmpeg (roundtrip). When `GOLDEN_OUT` is set the raw bitstream is also
//! dumped there for byte-identity capture.

use rust_motion_h264enc::{Encoder, EncoderConfig, FrameType, RateControl, YuvFrame};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Deterministic high-frequency luma noise with a moving vertical edge.
/// Chroma stays flat; the luma detail alone keeps SATD costs far above the
/// partition-search threshold while remaining reproducible bit for bit.
fn detail_frame(width: usize, height: usize, frame: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut y = vec![0u8; width * height];
    let u = vec![128u8; (width / 2) * (height / 2)];
    let v = vec![128u8; (width / 2) * (height / 2)];
    let mut state = 0x1234_5678u32 ^ frame.wrapping_mul(0x9E37_79B9);
    let edge = ((frame * 7) % (width as u32 - 32)) as usize;
    for row in 0..height {
        for col in 0..width {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state >> 24) as u8;
            let edge_px = if col.abs_diff(edge) < 4 { 0xFF } else { 0 };
            y[row * width + col] = noise ^ edge_px;
        }
    }
    (y, u, v)
}

fn encode_b_frames(width: u32, height: u32, frames: u32) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(width, height, 30, 1);
    cfg.rate = RateControl::Crf(20.0);
    cfg.bframes = 2;
    cfg.keyint = 250;
    cfg.threads = 1;
    cfg.preset = rust_motion_h264enc::Preset::Balanced;
    let mut encoder = Encoder::new(cfg).expect("encoder init");
    let mut bitstream = Vec::new();
    for frame in 0..frames {
        let (y, u, v) = detail_frame(width as usize, height as usize, frame);
        let yuv = YuvFrame {
            y: &y,
            u: &u,
            v: &v,
            y_stride: width as usize,
            uv_stride: (width as usize) / 2,
        };
        let packets = encoder.encode(&yuv, i64::from(frame)).expect("encode");
        for packet in packets {
            bitstream.extend_from_slice(&packet.data);
        }
    }
    for packet in encoder.flush() {
        bitstream.extend_from_slice(&packet.data);
    }
    bitstream
}

/// Counts B pictures by re-encoding and inspecting the packet frame types
/// (the encoder reports the coded type per access unit).
fn b_frame_count(width: u32, height: u32, frames: u32) -> usize {
    let mut cfg = EncoderConfig::new(width, height, 30, 1);
    cfg.rate = RateControl::Crf(20.0);
    cfg.bframes = 2;
    cfg.keyint = 250;
    cfg.threads = 1;
    cfg.preset = rust_motion_h264enc::Preset::Balanced;
    let mut encoder = Encoder::new(cfg).expect("encoder init");
    let mut b_count = 0usize;
    for frame in 0..frames {
        let (y, u, v) = detail_frame(width as usize, height as usize, frame);
        let yuv = YuvFrame {
            y: &y,
            u: &u,
            v: &v,
            y_stride: width as usize,
            uv_stride: (width as usize) / 2,
        };
        for packet in encoder.encode(&yuv, i64::from(frame)).expect("encode") {
            if packet.frame_type == FrameType::B {
                b_count += 1;
            }
        }
    }
    for packet in encoder.flush() {
        if packet.frame_type == FrameType::B {
            b_count += 1;
        }
    }
    b_count
}

#[test]
fn b_frame_partitions_encode_panic_free_with_roundtrip() -> Result<(), String> {
    let width = 320;
    let height = 192;
    let frames = 12;

    let encoded = catch_unwind(AssertUnwindSafe(|| encode_b_frames(width, height, frames)))
        .map_err(|_| "encoder panicked during B-frame partition search".to_string())?;
    assert!(!encoded.is_empty(), "bitstream must not be empty");

    let b_frames = b_frame_count(width, height, frames);
    assert!(b_frames > 0, "fixture produced no B pictures");

    if let Ok(out) = std::env::var("GOLDEN_OUT") {
        let dir = std::path::Path::new(&out);
        std::fs::create_dir_all(dir).map_err(|e| format!("golden dir: {e}"))?;
        std::fs::write(dir.join("bpart.bin"), &encoded)
            .map_err(|e| format!("golden write: {e}"))?;
    }

    // Decode roundtrip with ffmpeg when available (the gate environment).
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
    {
        let dir = std::env::temp_dir().join(format!("rust-motion-bpart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
        let encoded_path = dir.join("bpart.h264");
        let decoded_path = dir.join("decoded.i420");
        std::fs::write(&encoded_path, &encoded).map_err(|e| format!("write bitstream: {e}"))?;
        let output = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-i"])
            .arg(&encoded_path)
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .arg(&decoded_path)
            .output()
            .map_err(|e| format!("could not run ffmpeg: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "ffmpeg decode failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let decoded = std::fs::read(&decoded_path).map_err(|e| format!("read decoded: {e}"))?;
        let frame_size = (width as usize) * (height as usize) * 3 / 2;
        assert_eq!(
            decoded.len(),
            frame_size * frames as usize,
            "decoded byte count must match {} frames of {} bytes",
            frames,
            frame_size
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}
