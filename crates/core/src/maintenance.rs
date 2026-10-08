//! Maintenance eligibility uses observed time and durable state, never a clock.
use crate::jobs::{Job, Phase};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub job: Job,
    pub cleaned: bool,
    pub updated_at: i64,
    pub last_active_playback: Option<i64>,
}
impl CacheEntry {
    pub fn removable(&self, now: i64, retention: u64) -> bool {
        !self.cleaned
            && self
                .last_active_playback
                .is_none_or(|t| t <= playback_cutoff(now))
            && match self.job.phase {
                Phase::Failed | Phase::Cancelled => true,
                Phase::Completed => self.updated_at < retention_cutoff(now, retention),
                _ => false,
            }
    }
    pub fn acknowledge_removal(&self, attempt: u32) -> bool {
        self.job.attempt == attempt
            && !self.cleaned
            && matches!(
                self.job.phase,
                Phase::Completed | Phase::Failed | Phase::Cancelled
            )
    }
    pub fn abandoned_attempt(&self) -> Option<u32> {
        (self.job.phase == Phase::Queued && self.job.attempt > 0).then_some(self.job.attempt)
    }
}
pub fn enough_space(available: u64, required: u64, reserve: u64) -> bool {
    required
        .checked_add(reserve)
        .is_some_and(|needed| available >= needed)
}
pub fn due(last_success: Option<i64>, now: i64, interval: u64) -> bool {
    interval > 0
        && last_success
            .is_none_or(|last| now >= last && (now as i128 - last as i128) >= interval as i128)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub next_run: i64,
    pub interval: i64,
}
impl Schedule {
    pub fn claim(&self, now: i64) -> Result<Option<Self>, &'static str> {
        if !(60..=31_536_000).contains(&self.interval) {
            return Err("invalid_interval");
        }
        if self.next_run > now {
            return Ok(None);
        }
        Ok(Some(Self {
            next_run: now.checked_add(self.interval).ok_or("schedule_exhausted")?,
            interval: self.interval,
        }))
    }
}

/// Query adapters may use these bounds to preselect candidates before applying
/// CacheEntry::removable; the same observation time must be used for both.
pub fn retention_cutoff(now: i64, retention: u64) -> i64 {
    now.saturating_sub(i64::try_from(retention).unwrap_or(i64::MAX))
}
pub fn playback_cutoff(now: i64) -> i64 {
    now.saturating_sub(3600)
}
