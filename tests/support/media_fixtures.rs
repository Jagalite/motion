//! Explicit stream timestamp alignment for copy-route media fixtures only.
use serde_json::Value;
use std::{path::Path, process::Command};

fn probe(path: &Path) -> Value {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Encoder priming can give Matroska audio a negative start. These fixtures
/// deliberately test aligned copy routes, not support for that offset. Keep
/// every encoded video packet (including open GOPs), shifting only each audio
/// stream by its independently measured difference from the video origin.
pub fn align_fixture_audio_to_video(path: &Path) {
    let observed = probe(path);
    let streams = observed["streams"].as_array().unwrap();
    let start = |stream: &Value| {
        stream["start_time"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
    };
    let videos: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "video")
        .collect();
    assert_eq!(videos.len(), 1, "fixture requires exactly one video stream");
    let origin = start(videos[0]);
    assert!(origin.is_finite());
    let offsets: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .map(|audio| origin - start(audio))
        .collect();
    assert!(!offsets.is_empty() && offsets.iter().all(|v| v.is_finite()));
    let video_packets = |path: &Path| {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_packets",
                "-show_data_hash",
                "sha256",
                "-show_entries",
                "packet=data_hash",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let packets = serde_json::from_slice::<Value>(&output.stdout).unwrap()["packets"].clone();
        assert!(!packets.as_array().unwrap().is_empty());
        packets
    };
    let before = video_packets(path);
    let aligned = path.with_extension("aligned.mkv");
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-copyts", "-i"])
        .arg(path);
    for offset in &offsets {
        command
            .arg("-itsoffset")
            .arg(format!("{offset:.6}"))
            .arg("-i")
            .arg(path);
    }
    command.args(["-map", "0:v:0"]);
    for index in 0..offsets.len() {
        command.arg("-map").arg(format!("{}:a:{index}", index + 1));
    }
    let output = command
        .args([
            "-map",
            "0:s?",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
        ])
        .arg(&aligned)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        video_packets(&aligned),
        before,
        "fixture alignment changed encoded video/GOPs"
    );
    let checked = probe(&aligned);
    for stream in checked["streams"].as_array().unwrap() {
        assert!(
            (start(stream) - origin).abs() < 0.0005,
            "fixture stream not aligned: {stream}"
        );
    }
    let container_start: f64 = checked["format"]["start_time"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((container_start - origin).abs() < 0.0005);
    std::fs::rename(aligned, path).unwrap();
}
