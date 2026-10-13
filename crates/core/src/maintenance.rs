//! Maintenance eligibility uses observed time and durable state, never a clock.
use crate::jobs::{Job, Phase};
use serde::{Deserialize, Serialize};

/// Per-artifact ownership. The adapter supplies releases only from unique RAII
/// guards; clones retain a guard rather than manufacturing independent releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactUse {
    Readers(u32),
    Cleaning,
}
impl Default for ArtifactUse {
    fn default() -> Self {
        Self::Readers(0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactUseInput {
    Pin,
    ReleasePin,
    ClaimCleanup,
    ReleaseCleanup,
}
impl ArtifactUse {
    pub fn transition(self, input: ArtifactUseInput) -> Option<Self> {
        use ArtifactUseInput::*;
        match (self, input) {
            (Self::Readers(n), Pin) => n.checked_add(1).map(Self::Readers),
            (Self::Readers(n), ReleasePin) => n.checked_sub(1).map(Self::Readers),
            (Self::Readers(0), ClaimCleanup) => Some(Self::Cleaning),
            (Self::Cleaning, ReleaseCleanup) => Some(Self::Readers(0)),
            _ => None,
        }
    }
}
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

/// How a schedule's run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Everything due was dispatched.
    Done,
    /// More remains; the next tick continues from the cursor.
    More,
    /// The work queue was full; dispatch stopped at the cursor.
    Blocked,
}

/// How long a schedule blocked by a full work queue waits before trying
/// again, so blocked schedules never keep the oldest dispatch slots from
/// the others.
pub const BLOCKED_BACKOFF_SECONDS: i64 = 300;

/// When a claimed schedule (`claim`, which yields `claimed`) runs next:
/// after its interval when done, at once when more remains (it keeps its
/// due time, continuing from its cursor), and after a backoff when blocked.
pub fn next_run(previous_due: i64, claimed: &Schedule, now: i64, outcome: RunOutcome) -> i64 {
    match outcome {
        RunOutcome::Done => claimed.next_run,
        RunOutcome::More => previous_due,
        RunOutcome::Blocked => now
            .saturating_add(BLOCKED_BACKOFF_SECONDS)
            .min(claimed.next_run),
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

/// Snapshot publication is acknowledged only after bytes and complete inventory
/// have been verified and the current requester is still authorized.
pub struct BackupReceiptFacts {
    pub requester_authorized: bool,
    pub database_verified: bool,
    pub inventory_complete: bool,
    pub destination_durable: bool,
}
impl BackupReceiptFacts {
    pub fn publishable(&self) -> bool {
        self.requester_authorized
            && self.database_verified
            && self.inventory_complete
            && self.destination_durable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocked_schedule_backs_off_and_keeps_no_dispatch_slot() {
        let claimed = Schedule {
            next_run: 10_000 + 3_600,
            interval: 3_600,
        };
        assert_eq!(next_run(9_000, &claimed, 10_000, RunOutcome::Done), 13_600);
        assert_eq!(next_run(9_000, &claimed, 10_000, RunOutcome::More), 9_000);
        assert_eq!(
            next_run(9_000, &claimed, 10_000, RunOutcome::Blocked),
            10_000 + BLOCKED_BACKOFF_SECONDS
        );
        // Never later than its next regular run.
        let short = Schedule {
            next_run: 10_060,
            interval: 60,
        };
        assert_eq!(next_run(9_000, &short, 10_000, RunOutcome::Blocked), 10_060);
    }
}
