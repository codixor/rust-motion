//! Test-only availability checks for external tools.
//!
//! Unit tests that generate or inspect media need real FFmpeg/FFprobe. CI's
//! general test matrix deliberately does not install them: those tests skip
//! loudly instead of failing for an environment reason. The dedicated H.264
//! encoder gate installs FFmpeg and therefore exercises the real assertion.

use std::process::Command;

/// Returns true only when `ffmpeg -version` starts and exits successfully.
pub(crate) fn has_ffmpeg() -> bool {
    has_command("ffmpeg")
}

/// Returns true only when `ffprobe -version` starts and exits successfully.
pub(crate) fn has_ffprobe() -> bool {
    has_command("ffprobe")
}

fn has_command(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
