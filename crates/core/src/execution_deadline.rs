//! Liveness policy for one execution attempt. Times are monotonic elapsed milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Expired {
    Total,
    ExpectedDuration,
    Startup,
    NoProgress,
}

/// Optional per-command wall-clock allowance, independent of progress. This is
/// an execution deadline, not evidence that the produced media is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedDurationPolicy {
    pub allowance_seconds: u64,
    pub media_duration_multiplier: u32,
}
impl ExpectedDurationPolicy {
    pub fn valid(&self) -> bool {
        (1..=86400).contains(&self.allowance_seconds)
            && (1..=100).contains(&self.media_duration_multiplier)
    }
    pub fn budget_ms(&self, media_ms: u64) -> u64 {
        self.allowance_seconds
            .saturating_mul(1000)
            .saturating_add(media_ms.saturating_mul(u64::from(self.media_duration_multiplier)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Deadline {
    total_ms: u64,
    expected_ms: Option<u64>,
    startup_ms: Option<u64>,
    no_progress_ms: Option<u64>,
    last_advance: Option<u64>,
    position_ms: u64,
}
impl Deadline {
    pub fn new(total_ms: u64, startup_ms: Option<u64>, no_progress_ms: Option<u64>) -> Self {
        Self {
            total_ms,
            expected_ms: None,
            startup_ms,
            no_progress_ms,
            last_advance: None,
            position_ms: 0,
        }
    }
    pub fn with_expected_duration(mut self, budget_ms: Option<u64>) -> Self {
        self.expected_ms = budget_ms;
        self
    }
    /// Only advancing media time proves progress; repeated zeroes and diagnostic
    /// output cannot keep a stalled worker alive. Expiry cannot be resurrected.
    pub fn observe(&mut self, elapsed_ms: u64, position_ms: u64) {
        if self.expired(elapsed_ms).is_none() && position_ms > self.position_ms {
            self.position_ms = position_ms;
            self.last_advance = Some(elapsed_ms);
        }
    }
    pub fn expired(&self, elapsed_ms: u64) -> Option<Expired> {
        if elapsed_ms >= self.total_ms {
            return Some(Expired::Total);
        }
        if self.expected_ms.is_some_and(|limit| elapsed_ms >= limit) {
            return Some(Expired::ExpectedDuration);
        }
        match self.last_advance {
            None if self.startup_ms.is_some_and(|limit| elapsed_ms >= limit) => {
                Some(Expired::Startup)
            }
            Some(last)
                if self
                    .no_progress_ms
                    .is_some_and(|limit| elapsed_ms.saturating_sub(last) >= limit) =>
            {
                Some(Expired::NoProgress)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_requires_advancement_and_expiry_cannot_be_revived() {
        let mut d = Deadline::new(100, Some(10), Some(20));
        d.observe(9, 0);
        assert_eq!(d.expired(9), None);
        d.observe(10, 1);
        assert_eq!(d.expired(10), Some(Expired::Startup));
    }
    #[test]
    fn duplicate_and_regressing_positions_do_not_extend_deadline() {
        let mut d = Deadline::new(100, Some(10), Some(20));
        d.observe(5, 20);
        d.observe(15, 20);
        d.observe(24, 19);
        assert_eq!(d.expired(24), None);
        d.observe(25, 30);
        assert_eq!(d.expired(25), Some(Expired::NoProgress));
    }
    #[test]
    fn advancement_renews_stall_budget_but_never_total_budget() {
        let mut d = Deadline::new(40, Some(10), Some(20));
        for now in [5, 24, 39] {
            d.observe(now, now);
            assert_eq!(d.expired(now), None);
        }
        assert_eq!(d.expired(40), Some(Expired::Total));
        let d = Deadline::new(u64::MAX, None, None);
        assert_eq!(d.expired(u64::MAX - 1), None);
        assert_eq!(d.expired(u64::MAX), Some(Expired::Total));
    }
}

#[cfg(test)]
mod duration_policy_tests {
    use super::*;
    #[test]
    fn duration_budget_scales_saturates_and_cannot_renew() {
        let p = ExpectedDurationPolicy {
            allowance_seconds: 2,
            media_duration_multiplier: 3,
        };
        assert!(p.valid());
        assert_eq!(p.budget_ms(0), 2000);
        assert_eq!(p.budget_ms(1000), 5000);
        assert_eq!(p.budget_ms(u64::MAX), u64::MAX);
        let mut d =
            Deadline::new(10000, None, None).with_expected_duration(Some(p.budget_ms(1000)));
        d.observe(4999, 1);
        assert_eq!(d.expired(4999), None);
        d.observe(5000, 2);
        assert_eq!(d.expired(5000), Some(Expired::ExpectedDuration));
        assert_eq!(d.expired(10000), Some(Expired::Total));
    }
}
