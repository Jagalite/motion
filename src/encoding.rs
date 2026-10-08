//! Versioned, bounded encoder inputs. No caller-provided FFmpeg arguments.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}
impl FrameRate {
    pub fn value(&self) -> Option<f64> {
        (self.numerator > 0 && self.denominator > 0)
            .then(|| self.numerator as f64 / self.denominator as f64)
    }
    pub fn parse(value: &str) -> Option<Self> {
        let (n, d) = value.split_once('/')?;
        let rate = Self {
            numerator: n.parse().ok()?,
            denominator: d.parse().ok()?,
        };
        rate.value().filter(|v| *v <= 1000.0).map(|_| rate)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VideoProfile {
    pub version: u32,
    pub codec: VideoCodec,
    /// Bounding box, preserving aspect ratio and never increasing source dimensions.
    pub max_width: u32,
    pub max_height: u32,
    /// Encoder target, not a measured delivery throughput guarantee.
    pub video_bitrate: u64,
    pub audio_bitrate: u64,
    /// Null preserves source timestamps/cadence; a rational requests constant frame rate.
    #[serde(default)]
    pub frame_rate: Option<FrameRate>,
}
impl VideoProfile {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || !(128..=3840).contains(&self.max_width)
            || !(72..=2160).contains(&self.max_height)
            || !self.max_width.is_multiple_of(2)
            || !self.max_height.is_multiple_of(2)
            || !(64_000..=80_000_000).contains(&self.video_bitrate)
            || !(32_000..=320_000).contains(&self.audio_bitrate)
            || self.frame_rate.as_ref().is_some_and(|r| {
                r.value().is_none_or(|v| !(1.0..=120.0).contains(&v))
                    || r.numerator > 120_000
                    || r.denominator > 100_000
            })
        {
            return Err(
                "Invalid video profile: version 1, even dimensions 128..3840 x 72..2160, video 64000..80000000 bps, audio 32000..320000 bps, fps 1..120",
            );
        }
        Ok(())
    }
    pub fn admits_duration(&self, duration: f64) -> bool {
        duration.is_finite()
            && duration > 0.0
            && self
                .frame_rate
                .as_ref()
                .is_none_or(|rate| rate.value().is_some_and(|fps| duration * fps >= 0.5))
    }
    pub fn codec_name(&self) -> &'static str {
        match self.codec {
            VideoCodec::H264 => "h264",
            VideoCodec::Hevc => "hevc",
        }
    }
    pub fn arguments(&self, hardware: bool) -> Vec<String> {
        let encoder = match (&self.codec, hardware) {
            (VideoCodec::H264, false) => "libx264",
            (VideoCodec::Hevc, false) => "libx265",
            (VideoCodec::H264, true) => "h264_videotoolbox",
            (VideoCodec::Hevc, true) => "hevc_videotoolbox",
        };
        // Resample before encoding. Output -r/CFR can duplicate delayed frames
        // at EOF, adding whole seconds when downsampling to 1 or 2 fps.
        let mut filter = format!(
            "scale=w='min({},iw)':h='min({},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
            self.max_width, self.max_height
        );
        if let Some(rate) = &self.frame_rate {
            filter.push_str(&format!(
                ",fps=fps={}/{}:round=near",
                rate.numerator, rate.denominator
            ));
        }
        let mut args = vec![
            "-vf".into(),
            filter,
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-c:v".into(),
            encoder.into(),
            "-b:v".into(),
            self.video_bitrate.to_string(),
            "-maxrate".into(),
            self.video_bitrate.to_string(),
            "-bufsize".into(),
            (self.video_bitrate * 2).to_string(),
        ];
        if hardware {
            args.extend(["-allow_sw", "0"].map(str::to_owned));
        } else {
            args.extend(["-preset", "veryfast", "-threads", "2"].map(str::to_owned));
            if self.codec == VideoCodec::Hevc {
                args.extend(["-x265-params", "pools=2:frame-threads=2"].map(str::to_owned));
            }
        }
        if self.codec == VideoCodec::Hevc {
            args.extend(["-tag:v", "hvc1"].map(str::to_owned));
        }
        // The filter already emits the requested cadence. Do not run a second
        // frame duplicator in the output synchronization stage.
        args.extend(["-fps_mode", "passthrough"].map(str::to_owned));
        args.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            self.audio_bitrate.to_string(),
        ]);
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_profile_and_rational_fps() {
        let mut p = VideoProfile {
            version: 1,
            codec: VideoCodec::H264,
            max_width: 1280,
            max_height: 720,
            video_bitrate: 2_000_000,
            audio_bitrate: 128_000,
            frame_rate: Some(FrameRate {
                numerator: 30000,
                denominator: 1001,
            }),
        };
        assert!(p.validate().is_ok());
        assert!(
            p.arguments(false)
                .iter()
                .any(|arg| arg.contains("fps=fps=30000/1001:round=near"))
        );
        p.frame_rate.as_mut().unwrap().denominator = 0;
        assert!(p.validate().is_err());
        p.frame_rate = None;
        assert!(p.arguments(false).contains(&"passthrough".to_owned()));
        p.max_width = 1279;
        assert!(p.validate().is_err());
        assert!(FrameRate::parse("0/0").is_none());
        assert!(FrameRate::parse("NaN").is_none());
    }
}
