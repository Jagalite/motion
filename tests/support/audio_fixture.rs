//! Audio for timestamp-sensitive media fixtures, without encoder-delay metadata.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Encode 60 seconds through an elementary stream before muxing into Matroska.
/// Matroska muxers can write CodecDelay for AAC/AC-3 as well as Opus. Moving encoded
/// packets to zero with `setts` does not clear that encoder priming metadata and
/// can therefore move the probed start by another audio frame. Raw ADTS/AC-3
/// carries no CodecDelay; its demuxer supplies a zero-based packet timeline.
/// Keep this fixture normalization out of production copy eligibility.
pub fn audio_fixture(dir: &Path, codec: &str, rate: u32, frequency: u32) -> PathBuf {
    let format = match codec {
        "aac" => "adts",
        "ac3" => "ac3",
        _ => panic!("unsupported fixture codec {codec}"),
    };
    let path = dir.join(format!("fixture-{codec}-{rate}-{frequency}.{format}"));
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency={frequency}:sample_rate={rate}"))
        .args(["-t", "60", "-c:a", codec, "-f", format])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "audio fixture {}", path.display());
    path
}
