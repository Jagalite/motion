//! Qualification of live output before it can enter a delivery manifest.
//! Filesystem/MP4/FFprobe observations are supplied by the worker. State only
//! advances after the entire segment has been qualified; a canceled read can
//! never double-count bytes in a peak window.
use serde::{Deserialize, Serialize};

pub const MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Publication {
    pub verified: u32,
    pub ended: bool,
    pub failed: bool,
    recent: Vec<(u64, u64)>,
}
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub target_seconds: u64,
    pub cap_bps: Option<u64>,
    pub copied_video: bool,
    pub expected_start_us: Option<u64>,
    pub requested_start_ms: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub index: u32,
    pub bytes: u64,
    pub duration_ms: u64,
    pub independent: bool,
    pub start_us: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Wait,
    Failed,
    Ready {
        media_time_origin_ms: u64,
        first_segment_start_ms: u64,
        peak_bps: u64,
    },
    Segment {
        peak_bps: u64,
    },
}
impl Publication {
    /// A bounded encoder playlist must not roll past unaccounted output.
    pub fn begin(&mut self, first_index: Option<u32>) -> bool {
        if self.ended {
            return true;
        }
        if first_index.is_some_and(|first| first > self.verified) {
            self.failed = true;
        }
        !self.failed
    }
    pub fn may_observe(&self, index: u32, peer: Option<&Self>) -> bool {
        !self.failed
            && !self.ended
            && index == self.verified
            && peer.is_none_or(|p| !p.failed && index < p.verified)
    }
    pub fn qualify(&mut self, policy: Policy, segment: Segment, peer: Option<&Self>) -> Decision {
        if !self.may_observe(segment.index, peer) {
            return if self.failed {
                Decision::Failed
            } else {
                Decision::Wait
            };
        }
        let target_ms = policy.target_seconds.saturating_mul(1000);
        let first_start = if segment.index == 0 && policy.copied_video {
            match (segment.start_us, policy.expected_start_us) {
                (Some(actual), Some(expected)) if actual.abs_diff(expected) <= 1 => {
                    Some(actual / 1000)
                }
                _ => None,
            }
        } else {
            Some(0)
        };
        let mut recent = self.recent.clone();
        recent.push((segment.bytes, segment.duration_ms));
        while recent.len() > 1
            && recent[1..]
                .iter()
                .map(|(_, d)| u128::from(*d))
                .sum::<u128>()
                >= u128::from(target_ms) * 3 / 2
        {
            recent.remove(0);
        }
        if segment.bytes > MAX_SEGMENT_BYTES
            || !super::fits_target(segment.duration_ms, policy.target_seconds)
            || !segment.independent
            || first_start.is_none()
            || policy.cap_bps.is_some_and(|cap| {
                !crate::playback_session::peak_within_cap(
                    &recent,
                    cap,
                    target_ms / 2,
                    target_ms.saturating_mul(3) / 2,
                )
            })
        {
            self.failed = true;
            return Decision::Failed;
        }
        let peak_bps = crate::playback_session::peak_bitrate(
            &recent,
            target_ms / 2,
            target_ms.saturating_mul(3) / 2,
        );
        self.recent = recent;
        self.verified += 1;
        if segment.index == 0 {
            Decision::Ready {
                media_time_origin_ms: if policy.copied_video {
                    0
                } else {
                    policy.requested_start_ms
                },
                first_segment_start_ms: first_start.unwrap(),
                peak_bps,
            }
        } else {
            Decision::Segment { peak_bps }
        }
    }
    pub fn finish(&mut self, ended: bool, last_index: Option<u32>, peer: Option<&Self>) -> bool {
        self.ended = !self.failed
            && ended
            && self.verified > 0
            && last_index.and_then(|i| i.checked_add(1)) == Some(self.verified)
            && peer.is_none_or(|p| !p.failed && p.ended && p.verified == self.verified);
        self.ended
    }
}
