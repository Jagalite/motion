//! Liveness policy for one execution attempt. Times are monotonic elapsed milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Expired {
    Total,
    Startup,
    NoProgress,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Deadline {
    total_ms: u64,
    startup_ms: Option<u64>,
    no_progress_ms: Option<u64>,
    last_advance: Option<u64>,
    position_ms: u64,
}
impl Deadline {
    pub fn new(total_ms: u64, startup_ms: Option<u64>, no_progress_ms: Option<u64>) -> Self {
        Self {
            total_ms,
            startup_ms,
            no_progress_ms,
            last_advance: None,
            position_ms: 0,
        }
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
