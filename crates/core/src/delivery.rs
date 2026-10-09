//! Delivery sessions and their generations. A delivery is bound to one timeline;
//! each generation pins exact source/track/recipe choices and a requested start.
//! Execution reports (ready, segment, completion, failure, stop) are observations
//! that are ignored unless they belong to a generation that may still produce
//! output. Client commands name the generation they expect to replace.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Lease renewed by heartbeats. Expiry closes the delivery.
pub const LEASE_MS: u64 = 30_000;
/// After an overlapping replacement is activated, already admitted transfers of
/// the old generation may finish within this window before its files are removed.
/// New requests for a replaced generation are refused immediately.
pub const DRAIN_MS: u64 = 10_000;
/// Minimum number of segments advertised in a rolling media playlist.
pub const WINDOW: usize = 6;
/// Generations one delivery may create; further seeks require a new delivery.
pub const MAX_GENERATIONS: u64 = 1_000;
/// Delivery pacing and window policy, fixed at admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Policy {
    /// Advertised playlist duration kept before eviction (at least 3 targets).
    pub min_window_ms: u64,
    /// Pause a generation's worker when its output is this far ahead of the
    /// position it must serve next (client playhead for the active generation,
    /// requested start for a candidate).
    pub ahead_pause_ms: u64,
    /// Resume once the lead falls below this.
    pub ahead_resume_ms: u64,
}
/// Production policy: a minute of playlist, encode at most 45 s ahead.
pub const POLICY: Policy = Policy {
    min_window_ms: 60_000,
    ahead_pause_ms: 45_000,
    ahead_resume_ms: 30_000,
};

/// Accepted HLS target durations, in whole seconds.
pub const TARGET_SECONDS: std::ops::RangeInclusive<u64> = 1..=20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Original,
    Prepared,
    Remux,
    AudioConvert,
    VideoTranscode,
}
impl Operation {
    /// Byte routes serve a stored representation; the others are generated HLS.
    pub fn segmented(self) -> bool {
        !matches!(self, Operation::Original | Operation::Prepared)
    }
}

/// Exact choices for one generation. A seek copies the current selection unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pin {
    pub source_file: String,
    pub source_revision: String,
    /// Selected component occurrences in output order. Empty never means "default".
    pub tracks: Vec<String>,
    pub operation: Operation,
    pub recipe_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStatus {
    Starting,
    Ready,
    Active,
    Retiring,
    Retired,
    Failed,
}
impl GenerationStatus {
    pub fn current(self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Active)
    }
}

/// The execution worker owned by a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Worker {
    /// Start waits until displaced workers confirm termination.
    Deferred,
    Live,
    /// Stop requested; termination not yet confirmed.
    Stopping,
    /// No process: never needed (byte route), never started, or exited.
    Idle,
}
impl Worker {
    pub fn running(self) -> bool {
        matches!(self, Worker::Live | Worker::Stopping)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Segment {
    pub index: u32,
    pub duration_ms: u64,
}

/// A segment that left the playlist but stays fetchable for clients holding an
/// older copy of it (RFC 8216 §6.2.2): until its own duration plus the duration
/// of the longest playlist that contained it has elapsed after removal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Retained {
    pub index: u32,
    pub until_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Generation {
    pub status: GenerationStatus,
    pub pin: Pin,
    pub requested_start_ms: u64,
    /// Logical timeline time of media time zero, after keyframe/preroll adjustment.
    pub media_time_origin_ms: u64,
    /// Pinned when the first segment is published; constant for the generation.
    pub target_duration_s: u64,
    /// Segments advertised in the media playlist (a rolling window).
    pub segments: Vec<Segment>,
    /// Evicted but still fetchable segments, in index order.
    pub retained: Vec<Retained>,
    pub next_segment: u32,
    /// Logical interval covered by the advertised segments.
    pub available_start_ms: u64,
    pub available_end_ms: u64,
    /// Every segment through the final one was published; the playlist may end.
    pub complete: bool,
    /// Longest playlist ever advertised; bounds retention after eviction.
    pub longest_playlist_ms: u64,
    /// The live worker is paused because its output is far enough ahead.
    pub paused: bool,
    pub worker: Worker,
    pub drain_until_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Ready,
    Transitioning,
    Closing,
    Closed,
    Failed,
    Interrupted,
}
impl Status {
    /// Statuses in which no further admission, change or media request is allowed.
    pub fn fenced(self) -> bool {
        matches!(
            self,
            Status::Closing | Status::Closed | Status::Failed | Status::Interrupted
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Replacement {
    None,
    Overlap,
    Disruptive,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Delivery {
    pub revision: u64,
    pub policy: Policy,
    /// Last logical playhead reported by the client, with the generation it was
    /// playing. A playhead never paces another generation.
    pub playhead: Option<(u64, u64)>,
    pub timeline: String,
    pub duration_ms: Option<u64>,
    pub status: Status,
    pub replacement: Replacement,
    /// The generation a client replaces next. After a disruptive change it names
    /// the displaced (retiring, no longer served) generation until activation.
    pub active: Option<u64>,
    pub pending: Option<u64>,
    pub last_generation: u64,
    pub lease_expires_ms: u64,
    /// Generations that may still serve requests or still own a worker or files.
    pub generations: BTreeMap<u64, Generation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Input {
    /// The init fragment and segment 0 were published. Segment 0 must cover the
    /// requested start; `target_duration_s` bounds every later segment.
    Ready {
        generation: u64,
        media_time_origin_ms: u64,
        first_segment_ms: u64,
        target_duration_s: u64,
    },
    /// Segment `index` (from 1) was completely and atomically published.
    Segment {
        generation: u64,
        index: u32,
        duration_ms: u64,
        now_ms: u64,
    },
    /// The worker published its final segment, `final_index`.
    Completed {
        generation: u64,
        final_index: u32,
    },
    /// The worker or source failed for this generation.
    Failed {
        generation: u64,
    },
    /// The worker's process tree terminated (requested or natural exit).
    Stopped {
        generation: u64,
    },
    /// Stage one pending generation. `replan` replaces the pin; a seek keeps it.
    Change {
        expected_generation: u64,
        position_ms: u64,
        replan: Option<(String, Pin)>,
        /// Resource admission allows the current and new workers to coexist.
        overlap: bool,
        now_ms: u64,
    },
    Activate {
        generation: u64,
        expected_active: Option<u64>,
        now_ms: u64,
    },
    Heartbeat {
        active_generation: u64,
        /// Logical timeline playhead, when the client reports it.
        position_ms: Option<u64>,
        now_ms: u64,
    },
    Tick {
        now_ms: u64,
    },
    Close,
    /// Server restart: live transports do not resume. Workers died with the owner.
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Effect {
    Start {
        generation: u64,
    },
    Stop {
        generation: u64,
    },
    /// Pause or resume the generation's live worker (encode-ahead bound).
    Pace {
        generation: u64,
        paused: bool,
    },
    /// Delete published segments below `below_index`; none is advertised or fetchable.
    Discard {
        generation: u64,
        below_index: u32,
    },
    /// Delete all owned output once nothing may serve or write it.
    Cleanup {
        generation: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    DeliveryClosed,
    GenerationConflict,
    GenerationNotReady,
    TimelineChanged,
    InvalidPosition,
    GenerationLimit,
}

/// Produced media may overrun the logical duration by at most one second.
fn within(duration_ms: Option<u64>, end_ms: u64) -> bool {
    duration_ms.is_none_or(|d| end_ms <= d.saturating_add(1_000))
}

fn valid_position(duration_ms: Option<u64>, position_ms: u64) -> bool {
    duration_ms.is_none_or(|d| position_ms < d.max(1))
}

/// HLS EXTINF values, rounded to the nearest second, must not exceed the target.
pub fn fits_target(duration_ms: u64, target_s: u64) -> bool {
    duration_ms > 0 && duration_ms.saturating_add(500) / 1000 <= target_s
}

fn new_generation(pin: Pin, start_ms: u64, duration_ms: Option<u64>) -> Generation {
    let segmented = pin.operation.segmented();
    Generation {
        status: GenerationStatus::Starting,
        pin,
        requested_start_ms: start_ms,
        media_time_origin_ms: if segmented { start_ms } else { 0 },
        target_duration_s: 0,
        segments: Vec::new(),
        retained: Vec::new(),
        next_segment: 0,
        available_start_ms: if segmented { start_ms } else { 0 },
        available_end_ms: if segmented {
            start_ms
        } else {
            duration_ms.unwrap_or(start_ms)
        },
        // A stored representation is complete and needs no worker.
        complete: !segmented,
        longest_playlist_ms: 0,
        paused: false,
        worker: if segmented {
            Worker::Deferred
        } else {
            Worker::Idle
        },
        drain_until_ms: None,
    }
}

impl Delivery {
    /// Admit a delivery and its first generation under the production policy.
    pub fn admit(
        timeline: String,
        pin: Pin,
        start_ms: u64,
        duration_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<(Self, Vec<Effect>), Error> {
        Self::admit_with_policy(POLICY, timeline, pin, start_ms, duration_ms, now_ms)
    }

    pub fn admit_with_policy(
        policy: Policy,
        timeline: String,
        pin: Pin,
        start_ms: u64,
        duration_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<(Self, Vec<Effect>), Error> {
        if !valid_position(duration_ms, start_ms) {
            return Err(Error::InvalidPosition);
        }
        let mut d = Self {
            revision: 1,
            policy,
            playhead: None,
            timeline,
            duration_ms,
            status: Status::Starting,
            replacement: Replacement::None,
            active: None,
            pending: Some(1),
            last_generation: 1,
            lease_expires_ms: now_ms.saturating_add(LEASE_MS),
            generations: BTreeMap::from([(1, new_generation(pin, start_ms, duration_ms))]),
        };
        let mut effects = Vec::new();
        if d.generations[&1].pin.operation.segmented() {
            d.start_deferred(&mut effects);
        } else {
            d.promote(1);
        }
        Ok((d, effects))
    }

    /// Whether a new request for `generation` may be admitted now. Principal
    /// authorization is a separate, prior check. Transfers admitted earlier may
    /// continue while a replaced generation drains.
    pub fn serves(&self, generation: u64, now_ms: u64) -> bool {
        self.lease_valid(now_ms)
            && self.generations.get(&generation).is_some_and(|g| {
                matches!(g.status, GenerationStatus::Ready | GenerationStatus::Active)
            })
    }

    /// A published segment that may be fetched now (advertised or recently evicted).
    pub fn fetchable(&self, generation: u64, index: u32, now_ms: u64) -> bool {
        self.serves(generation, now_ms) && {
            let g = &self.generations[&generation];
            g.pin.operation.segmented()
                && (g.segments.iter().any(|s| s.index == index)
                    || g.retained
                        .iter()
                        .any(|r| r.index == index && now_ms < r.until_ms))
        }
    }

    fn lease_valid(&self, now_ms: u64) -> bool {
        !self.status.fenced() && now_ms < self.lease_expires_ms
    }

    /// Make a generation that is ready to serve the current one.
    fn promote(&mut self, number: u64) {
        self.generations.get_mut(&number).unwrap().status = GenerationStatus::Active;
        self.active = Some(number);
        self.pending = None;
        self.status = Status::Ready;
        self.replacement = Replacement::None;
    }

    fn playable(&self) -> bool {
        self.active.is_some_and(|a| {
            self.generations
                .get(&a)
                .is_some_and(|g| g.status == GenerationStatus::Active)
        })
    }

    /// Start a deferred pending worker once no worker it must not overlap remains.
    /// Only an overlapping replacement may run beside the serving generation.
    fn start_deferred(&mut self, effects: &mut Vec<Effect>) {
        let Some(pending) = self.pending else { return };
        if self.generations[&pending].worker != Worker::Deferred {
            return;
        }
        let coexisting = self
            .active
            .filter(|_| self.replacement == Replacement::Overlap && self.playable());
        let blocked = self
            .generations
            .iter()
            .any(|(n, g)| *n != pending && Some(*n) != coexisting && g.worker.running());
        if !blocked {
            self.generations.get_mut(&pending).unwrap().worker = Worker::Live;
            effects.push(Effect::Start {
                generation: pending,
            });
        }
    }

    fn stop(&mut self, number: u64, effects: &mut Vec<Effect>) {
        let g = self.generations.get_mut(&number).expect("known generation");
        match g.worker {
            Worker::Live => {
                g.worker = Worker::Stopping;
                effects.push(Effect::Stop { generation: number });
            }
            Worker::Deferred => g.worker = Worker::Idle,
            Worker::Stopping | Worker::Idle => {}
        }
    }

    /// Mark a generation unusable and, once its worker is gone, forget it.
    fn retire(&mut self, number: u64, status: GenerationStatus, effects: &mut Vec<Effect>) {
        if self.active == Some(number) {
            self.active = None;
        }
        if self.pending == Some(number) {
            self.pending = None;
        }
        self.generations
            .get_mut(&number)
            .expect("known generation")
            .status = status;
        self.stop(number, effects);
        self.collect(number, effects);
    }

    fn collect(&mut self, number: u64, effects: &mut Vec<Effect>) {
        if let Some(g) = self.generations.get(&number)
            && g.worker == Worker::Idle
            && matches!(
                g.status,
                GenerationStatus::Retired | GenerationStatus::Failed
            )
        {
            self.generations.remove(&number);
            effects.push(Effect::Cleanup { generation: number });
        }
    }

    fn finish_closing(&mut self) {
        if self.status == Status::Closing && self.generations.is_empty() {
            self.status = Status::Closed;
        }
    }

    /// Fence everything. `status` is Closing, Failed or Interrupted.
    fn shut(&mut self, status: Status, effects: &mut Vec<Effect>) {
        self.status = status;
        self.replacement = Replacement::None;
        for number in self.generations.keys().copied().collect::<Vec<_>>() {
            let failed = self.generations[&number].status == GenerationStatus::Failed;
            self.retire(
                number,
                if failed {
                    GenerationStatus::Failed
                } else {
                    GenerationStatus::Retired
                },
                effects,
            );
        }
        self.finish_closing();
    }

    /// A generation's output failed or its worker exited before completing it.
    fn lose(&mut self, number: u64, effects: &mut Vec<Effect>) {
        let serving = self.active == Some(number) && self.playable();
        let candidate_of_unplayable = self.pending == Some(number) && !self.playable();
        self.retire(number, GenerationStatus::Failed, effects);
        if serving || candidate_of_unplayable {
            // Nothing playable remains.
            self.shut(Status::Failed, effects);
        } else if self.pending.is_none() {
            // A failed candidate leaves the serving generation intact.
            self.status = Status::Ready;
            self.replacement = Replacement::None;
        }
    }

    /// Evict advertised segments while the remaining window still spans the RFC
    /// 8216 minimum. Evicted segments stay fetchable until their deadline.
    fn evict(&mut self, number: u64, now_ms: u64) {
        let g = self.generations.get_mut(&number).unwrap();
        let minimum = (g.target_duration_s * 3_000).max(self.policy.min_window_ms);
        let mut total: u64 = g.segments.iter().map(|s| s.duration_ms).sum();
        // A conservative bound on any playlist that contained an evicted segment.
        g.longest_playlist_ms = g.longest_playlist_ms.max(total);
        let longest = g.longest_playlist_ms;
        loop {
            let head = g.segments[0].duration_ms;
            if g.segments.len() <= WINDOW || total - head < minimum {
                break;
            }
            let evicted = g.segments.remove(0);
            total -= head;
            g.available_start_ms += head;
            g.retained.push(Retained {
                index: evicted.index,
                until_ms: now_ms.saturating_add(head).saturating_add(longest),
            });
        }
    }

    /// Pause workers whose output is far enough ahead of what must be served
    /// next; resume them once the lead shrinks (hysteresis between thresholds).
    fn pace(&mut self, effects: &mut Vec<Effect>) {
        let policy = self.policy;
        for (number, g) in self.generations.iter_mut() {
            if g.worker != Worker::Live || !g.status.current() || g.complete {
                continue;
            }
            // Until the client reports playing this generation, it is consumed
            // from its requested start.
            let reference = match self.playhead {
                Some((generation, position)) if generation == *number => position,
                _ => g.requested_start_ms,
            };
            let ahead = g.available_end_ms.saturating_sub(reference);
            let paused = if g.paused {
                ahead >= policy.ahead_resume_ms
            } else {
                ahead >= policy.ahead_pause_ms
            };
            if paused != g.paused {
                g.paused = paused;
                effects.push(Effect::Pace {
                    generation: *number,
                    paused,
                });
            }
        }
    }

    /// Forget retained segments whose deadline passed and delete their files.
    fn expire(&mut self, now_ms: u64, effects: &mut Vec<Effect>) {
        for (number, g) in self.generations.iter_mut() {
            let before = g.retained.len();
            g.retained.retain(|r| now_ms < r.until_ms);
            if g.retained.len() != before {
                let below = g
                    .retained
                    .first()
                    .map(|r| r.index)
                    .or(g.segments.first().map(|s| s.index))
                    .unwrap_or(g.next_segment);
                effects.push(Effect::Discard {
                    generation: *number,
                    below_index: below,
                });
            }
        }
    }
}

fn producing(d: &Delivery, number: u64) -> bool {
    !d.status.fenced()
        && d.generations.get(&number).is_some_and(|g| {
            g.worker == Worker::Live && g.pin.operation.segmented() && g.status.current()
        })
}

/// The production reducer, also exercised directly by the Stateless model.
/// Execution observations for fenced or unknown generations are accepted no-ops.
pub fn transition(before: &Delivery, input: &Input) -> Result<(Delivery, Vec<Effect>), Error> {
    use GenerationStatus as G;
    let mut d = before.clone();
    let mut effects = Vec::new();
    match input {
        Input::Ready {
            generation,
            media_time_origin_ms,
            first_segment_ms,
            target_duration_s,
        } => {
            let n = *generation;
            let origin = *media_time_origin_ms;
            let end = origin.saturating_add(*first_segment_ms);
            if producing(&d, n) && d.generations[&n].status == G::Starting {
                let start = d.generations[&n].requested_start_ms;
                if origin <= start
                    && start < end
                    && within(d.duration_ms, end)
                    && TARGET_SECONDS.contains(target_duration_s)
                    && fits_target(*first_segment_ms, *target_duration_s)
                {
                    let g = d.generations.get_mut(&n).unwrap();
                    g.media_time_origin_ms = origin;
                    g.target_duration_s = *target_duration_s;
                    g.segments = vec![Segment {
                        index: 0,
                        duration_ms: *first_segment_ms,
                    }];
                    g.next_segment = 1;
                    g.available_start_ms = origin;
                    g.available_end_ms = end;
                    g.longest_playlist_ms = *first_segment_ms;
                    if d.active.is_none() {
                        // Nothing to hand off from: the first generation activates.
                        d.promote(n);
                    } else {
                        g.status = G::Ready;
                    }
                } else {
                    // Output that cannot satisfy the request is a failed generation.
                    d.lose(n, &mut effects);
                }
            }
        }
        Input::Segment {
            generation,
            index,
            duration_ms,
            now_ms,
        } => {
            let n = *generation;
            if producing(&d, n)
                && d.generations[&n].status != G::Starting
                && !d.generations[&n].complete
                && *index == d.generations[&n].next_segment
            {
                let g = d.generations.get_mut(&n).unwrap();
                let end = g.available_end_ms.saturating_add(*duration_ms);
                if fits_target(*duration_ms, g.target_duration_s) && within(d.duration_ms, end) {
                    g.segments.push(Segment {
                        index: *index,
                        duration_ms: *duration_ms,
                    });
                    g.next_segment += 1;
                    g.available_end_ms = end;
                    d.evict(n, *now_ms);
                } else {
                    d.lose(n, &mut effects);
                }
            }
            d.expire(*now_ms, &mut effects);
        }
        Input::Completed {
            generation,
            final_index,
        } => {
            let n = *generation;
            if producing(&d, n) && d.generations[&n].status != G::Starting {
                if final_index.checked_add(1) == Some(d.generations[&n].next_segment) {
                    d.generations.get_mut(&n).unwrap().complete = true;
                } else {
                    // A missing or extra segment report: ENDLIST would be wrong.
                    d.lose(n, &mut effects);
                }
            }
        }
        Input::Failed { generation } => {
            let n = *generation;
            if !d.status.fenced() && d.generations.get(&n).is_some_and(|g| g.status.current()) {
                d.lose(n, &mut effects);
            }
        }
        Input::Stopped { generation } => {
            let n = *generation;
            let Some(g) = d.generations.get_mut(&n) else {
                return Ok((d, effects));
            };
            if !g.worker.running() {
                return Ok((d, effects));
            }
            let unexpected = g.worker == Worker::Live && !g.complete && g.status.current();
            g.worker = Worker::Idle;
            if unexpected && !d.status.fenced() {
                d.lose(n, &mut effects);
            } else {
                d.collect(n, &mut effects);
            }
            if !d.status.fenced() {
                d.start_deferred(&mut effects);
            }
            d.finish_closing();
        }
        Input::Change {
            expected_generation,
            position_ms,
            replan,
            overlap,
            now_ms,
        } => {
            if !d.lease_valid(*now_ms) {
                return Err(Error::DeliveryClosed);
            }
            if d.active != Some(*expected_generation) {
                return Err(Error::GenerationConflict);
            }
            // The latest selection: the pending candidate if any, else the active one.
            let current = d.pending.or(d.active).and_then(|n| d.generations.get(&n));
            let pin = match replan {
                Some((timeline, _)) if *timeline != d.timeline => {
                    return Err(Error::TimelineChanged);
                }
                Some((_, pin)) => pin.clone(),
                None => current.ok_or(Error::GenerationConflict)?.pin.clone(),
            };
            if !valid_position(d.duration_ms, *position_ms) {
                return Err(Error::InvalidPosition);
            }
            if d.last_generation >= MAX_GENERATIONS {
                return Err(Error::GenerationLimit);
            }
            if let Some(old) = d.pending {
                // A newer change supersedes the unactivated candidate.
                d.retire(old, G::Retired, &mut effects);
            }
            let active = *expected_generation;
            if d.playable() && !*overlap {
                // The old worker cannot coexist. It stops now and is no longer served;
                // it remains the generation a client replaces on activation.
                d.generations.get_mut(&active).unwrap().status = G::Retiring;
                d.stop(active, &mut effects);
            }
            d.last_generation += 1;
            let n = d.last_generation;
            let generation = new_generation(pin, *position_ms, d.duration_ms);
            let segmented = generation.pin.operation.segmented();
            d.generations.insert(n, generation);
            d.pending = Some(n);
            if d.playable() {
                d.status = Status::Transitioning;
                d.replacement = Replacement::Overlap;
            } else {
                d.status = Status::Starting;
                d.replacement = Replacement::Disruptive;
            }
            if segmented {
                d.start_deferred(&mut effects);
            } else {
                d.generations.get_mut(&n).unwrap().status = G::Ready;
            }
        }
        Input::Activate {
            generation,
            expected_active,
            now_ms,
        } => {
            if !d.lease_valid(*now_ms) {
                return Err(Error::DeliveryClosed);
            }
            if d.active == Some(*generation) {
                // Duplicate of an acknowledged activation.
                return Ok((before.clone(), vec![]));
            }
            if d.active != *expected_active {
                return Err(Error::GenerationConflict);
            }
            if d.pending != Some(*generation) || d.generations[generation].status != G::Ready {
                return Err(Error::GenerationNotReady);
            }
            if let Some(old) = d.active
                && d.generations.contains_key(&old)
            {
                if d.generations[&old].status == G::Active {
                    let g = d.generations.get_mut(&old).unwrap();
                    g.status = G::Retiring;
                    g.drain_until_ms = Some(now_ms.saturating_add(DRAIN_MS));
                    d.stop(old, &mut effects);
                } else {
                    // Disruptively displaced: already stopped and not served.
                    d.retire(old, G::Retired, &mut effects);
                }
            }
            d.promote(*generation);
        }
        Input::Heartbeat {
            active_generation,
            position_ms,
            now_ms,
        } => {
            if !d.lease_valid(*now_ms) {
                return Err(Error::DeliveryClosed);
            }
            if ![d.active, d.pending].contains(&Some(*active_generation)) {
                return Err(Error::GenerationConflict);
            }
            if position_ms.is_some_and(|p| !within(d.duration_ms, p)) {
                return Err(Error::InvalidPosition);
            }
            d.lease_expires_ms = now_ms.saturating_add(LEASE_MS).max(d.lease_expires_ms);
            if let Some(position) = position_ms {
                d.playhead = Some((*active_generation, *position));
            }
        }
        Input::Tick { now_ms } => {
            if !d.status.fenced() && *now_ms >= d.lease_expires_ms {
                d.shut(Status::Closing, &mut effects);
            }
            for number in d.generations.keys().copied().collect::<Vec<_>>() {
                let g = &d.generations[&number];
                if g.status == G::Retiring && g.drain_until_ms.is_some_and(|t| *now_ms >= t) {
                    d.retire(number, G::Retired, &mut effects);
                }
            }
            d.expire(*now_ms, &mut effects);
        }
        Input::Close => {
            if !d.status.fenced() {
                d.shut(Status::Closing, &mut effects);
            }
        }
        Input::Interrupt => {
            if !matches!(d.status, Status::Closed | Status::Interrupted) {
                for g in d.generations.values_mut() {
                    // Restart recovery runs after the old workers' supervisor exited.
                    g.worker = Worker::Idle;
                }
                d.shut(Status::Interrupted, &mut effects);
            }
        }
    }
    if !d.status.fenced() {
        d.pace(&mut effects);
    }
    if d != *before {
        d.revision += 1;
    }
    Ok((d, effects))
}

/// Media playlist for a serving segmented generation. A rolling window is never
/// labelled as a complete VOD playlist; ENDLIST follows only the final segment.
pub fn media_playlist(
    generation: &Generation,
    segment_uri: impl Fn(u32) -> String,
    init_uri: &str,
) -> String {
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"{init_uri}\"\n",
        generation.target_duration_s,
        generation
            .segments
            .first()
            .map_or(generation.next_segment, |s| s.index)
    );
    for s in &generation.segments {
        out.push_str(&format!(
            "#EXTINF:{}.{:03},\n{}\n",
            s.duration_ms / 1000,
            s.duration_ms % 1000,
            segment_uri(s.index)
        ));
    }
    if generation.complete {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pin(tracks: &[&str]) -> Pin {
        Pin {
            source_file: "f".into(),
            source_revision: "r".into(),
            tracks: tracks.iter().map(|t| (*t).into()).collect(),
            operation: Operation::VideoTranscode,
            recipe_digest: Some("h264-aac".into()),
        }
    }
    fn step(d: &mut Delivery, input: Input) -> Result<Vec<Effect>, Error> {
        let (next, effects) = transition(d, &input)?;
        *d = next;
        Ok(effects)
    }
    fn ready(generation: u64, origin: u64) -> Input {
        Input::Ready {
            generation,
            media_time_origin_ms: origin,
            first_segment_ms: 4000,
            target_duration_s: 6,
        }
    }
    fn change(expected: u64, position: u64, overlap: bool, now: u64) -> Input {
        Input::Change {
            expected_generation: expected,
            position_ms: position,
            replan: None,
            overlap,
            now_ms: now,
        }
    }
    fn activate(generation: u64, expected: Option<u64>, now: u64) -> Input {
        Input::Activate {
            generation,
            expected_active: expected,
            now_ms: now,
        }
    }

    #[test]
    fn overlap_change_keeps_old_generation_until_activation() {
        let (mut d, effects) =
            Delivery::admit("t".into(), pin(&["a2", "s1"]), 0, Some(60_000), 0).unwrap();
        assert_eq!(effects, [Effect::Start { generation: 1 }]);
        assert!(!d.serves(1, 0));
        step(&mut d, ready(1, 0)).unwrap();
        assert_eq!((d.status, d.active), (Status::Ready, Some(1)));
        assert!(d.serves(1, 1));
        let effects = step(&mut d, change(1, 30_000, true, 2)).unwrap();
        assert_eq!(effects, [Effect::Start { generation: 2 }]);
        assert_eq!(d.generations[&2].pin, d.generations[&1].pin);
        assert_eq!(
            transition(&d, &activate(2, Some(1), 3)),
            Err(Error::GenerationNotReady)
        );
        step(&mut d, ready(2, 29_500)).unwrap();
        assert!(d.serves(1, 3) && d.serves(2, 3));
        assert_eq!(
            transition(&d, &activate(2, None, 3)),
            Err(Error::GenerationConflict)
        );
        let effects = step(&mut d, activate(2, Some(1), 3)).unwrap();
        assert_eq!(effects, [Effect::Stop { generation: 1 }]);
        // New requests for the replaced generation are refused immediately.
        assert!(!d.serves(1, 4) && !d.fetchable(1, 0, 4));
        // Old-generation output can no longer enter any manifest.
        step(
            &mut d,
            Input::Segment {
                generation: 1,
                index: 1,
                duration_ms: 4000,
                now_ms: 4,
            },
        )
        .unwrap();
        assert_eq!(d.generations[&1].segments.len(), 1);
        assert_eq!(step(&mut d, Input::Stopped { generation: 1 }), Ok(vec![]));
        let effects = step(
            &mut d,
            Input::Tick {
                now_ms: 3 + DRAIN_MS,
            },
        )
        .unwrap();
        assert_eq!(effects, [Effect::Cleanup { generation: 1 }]);
        let revision = d.revision;
        assert_eq!(step(&mut d, activate(2, Some(1), 5)), Ok(vec![]));
        assert_eq!(d.revision, revision);
    }

    #[test]
    fn failed_candidate_leaves_active_and_timeline_is_immutable() {
        let (mut d, _) = Delivery::admit("t".into(), pin(&["a1"]), 0, None, 0).unwrap();
        step(&mut d, ready(1, 0)).unwrap();
        let replan = |timeline: &str, tracks| Input::Change {
            expected_generation: 1,
            position_ms: 0,
            replan: Some((timeline.into(), pin(tracks))),
            overlap: true,
            now_ms: 1,
        };
        assert_eq!(
            transition(&d, &replan("other-cut", &["a1"])),
            Err(Error::TimelineChanged)
        );
        step(&mut d, replan("t", &["a2"])).unwrap();
        assert_eq!(d.generations[&2].pin.tracks, ["a2"]);
        // A seek while the replan is pending keeps the newest selection. The
        // superseded candidate must stop before the newer one starts.
        let effects = step(&mut d, change(1, 0, true, 1)).unwrap();
        assert_eq!(effects, [Effect::Stop { generation: 2 }]);
        assert_eq!(d.generations[&3].pin.tracks, ["a2"]);
        assert_eq!(
            step(&mut d, Input::Stopped { generation: 2 }).unwrap(),
            [
                Effect::Cleanup { generation: 2 },
                Effect::Start { generation: 3 }
            ]
        );
        let effects = step(&mut d, Input::Failed { generation: 3 }).unwrap();
        assert_eq!(effects, [Effect::Stop { generation: 3 }]);
        assert_eq!(
            (d.status, d.active, d.pending),
            (Status::Ready, Some(1), None)
        );
        assert_eq!(
            step(&mut d, Input::Stopped { generation: 3 }).unwrap(),
            [Effect::Cleanup { generation: 3 }]
        );
    }

    #[test]
    fn disruptive_change_defers_start_and_requires_activation() {
        let (mut d, _) = Delivery::admit("t".into(), pin(&["a1"]), 0, Some(20_000), 0).unwrap();
        step(&mut d, ready(1, 0)).unwrap();
        let effects = step(&mut d, change(1, 9_000, false, 1)).unwrap();
        assert_eq!(effects, [Effect::Stop { generation: 1 }]);
        assert_eq!(d.generations[&2].worker, Worker::Deferred);
        assert_eq!(
            (d.status, d.replacement),
            (Status::Starting, Replacement::Disruptive)
        );
        assert!(!d.serves(1, 1));
        let effects = step(&mut d, Input::Stopped { generation: 1 }).unwrap();
        assert_eq!(effects, [Effect::Start { generation: 2 }]);
        // Segment 0 must cover the requested start; otherwise the candidate failed.
        let bad = Input::Ready {
            generation: 2,
            media_time_origin_ms: 4_000,
            first_segment_ms: 4000,
            target_duration_s: 6,
        };
        assert_eq!(transition(&d, &bad).unwrap().0.status, Status::Failed);
        step(&mut d, ready(2, 8_000)).unwrap();
        assert_eq!((d.active, d.pending), (Some(1), Some(2)));
        let effects = step(&mut d, activate(2, Some(1), 2)).unwrap();
        assert_eq!(effects, [Effect::Cleanup { generation: 1 }]);
        assert_eq!((d.status, d.active), (Status::Ready, Some(2)));
    }

    #[test]
    fn lease_expiry_fences_commands_and_close_waits_for_workers() {
        let (mut d, _) = Delivery::admit("t".into(), pin(&["a1"]), 0, Some(20_000), 0).unwrap();
        step(&mut d, ready(1, 0)).unwrap();
        let heartbeat = |generation, now| Input::Heartbeat {
            active_generation: generation,
            position_ms: None,
            now_ms: now,
        };
        assert_eq!(
            transition(&d, &heartbeat(2, 1)),
            Err(Error::GenerationConflict)
        );
        step(&mut d, heartbeat(1, 20_000)).unwrap();
        let expired = 20_000 + LEASE_MS;
        assert_eq!(
            transition(&d, &change(1, 0, true, expired)),
            Err(Error::DeliveryClosed)
        );
        assert!(!d.serves(1, expired) && d.serves(1, expired - 1));
        let effects = step(&mut d, Input::Tick { now_ms: expired }).unwrap();
        assert_eq!(effects, [Effect::Stop { generation: 1 }]);
        assert_eq!(d.status, Status::Closing);
        step(&mut d, Input::Stopped { generation: 1 }).unwrap();
        assert_eq!((d.status, d.generations.len()), (Status::Closed, 0));
    }

    #[test]
    fn byte_routes_need_no_worker() {
        let mut original = pin(&["a1"]);
        original.operation = Operation::Original;
        let (mut d, effects) =
            Delivery::admit("t".into(), original.clone(), 5_000, Some(20_000), 0).unwrap();
        assert!(effects.is_empty());
        assert_eq!((d.status, d.active), (Status::Ready, Some(1)));
        assert!(d.serves(1, 0) && !d.fetchable(1, 0, 0));
        // Switch to a transcode while the original keeps serving.
        let effects = step(
            &mut d,
            Input::Change {
                expected_generation: 1,
                position_ms: 5_000,
                replan: Some(("t".into(), pin(&["a1"]))),
                overlap: false,
                now_ms: 1,
            },
        )
        .unwrap();
        // The original has no worker to stop, so the new one starts at once.
        assert_eq!(effects, [Effect::Start { generation: 2 }]);
        step(&mut d, ready(2, 4_000)).unwrap();
        let effects = step(&mut d, activate(2, Some(1), 2)).unwrap();
        assert_eq!(effects, [Effect::Cleanup { generation: 1 }]);
        assert_eq!(
            step(&mut d, Input::Close).unwrap(),
            [Effect::Stop { generation: 2 }]
        );
    }

    #[test]
    fn playlist_is_rolling_retained_and_ends_only_after_final_segment() {
        // Admitted late enough that the lease covers the clock values used below.
        let rfc_minimum = Policy {
            min_window_ms: 0,
            ahead_pause_ms: u64::MAX,
            ahead_resume_ms: u64::MAX,
        };
        let (mut d, _) = Delivery::admit_with_policy(
            rfc_minimum,
            "t".into(),
            pin(&["a1"]),
            0,
            Some(200_000),
            60_000,
        )
        .unwrap();
        let segment = |index, duration_ms| Input::Segment {
            generation: 1,
            index,
            duration_ms,
            now_ms: u64::from(index) * 4000,
        };
        // Segments before the generation is ready are not offered.
        step(&mut d, segment(1, 4000)).unwrap();
        step(&mut d, ready(1, 0)).unwrap();
        let mut discards = Vec::new();
        for index in 1..20 {
            discards.extend(step(&mut d, segment(index, 4000)).unwrap());
        }
        let g = &d.generations[&1];
        assert_eq!(g.segments.first().unwrap().index, 14);
        assert_eq!((g.available_start_ms, g.available_end_ms), (56_000, 80_000));
        // Segment 7 left the playlist at 52 s with a 28 s playlist: kept until 84 s.
        assert_eq!(
            discards.last(),
            Some(&Effect::Discard {
                generation: 1,
                below_index: 6
            })
        );
        let now = 76_000;
        assert!(d.fetchable(1, 7, now) && !d.fetchable(1, 5, now) && !d.fetchable(1, 20, now));
        assert!(!d.fetchable(1, 7, 84_000) && d.fetchable(1, 14, 84_000));
        let text = media_playlist(g, |i| format!("s/{i}"), "init.mp4");
        assert!(text.contains("#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:14\n"));
        assert!(!text.contains("ENDLIST") && !text.contains("PLAYLIST-TYPE"));
        // A completion that skips a report fails rather than ending early.
        let skipped = Input::Completed {
            generation: 1,
            final_index: 20,
        };
        assert_eq!(transition(&d, &skipped).unwrap().0.status, Status::Failed);
        step(
            &mut d,
            Input::Completed {
                generation: 1,
                final_index: 19,
            },
        )
        .unwrap();
        let text = media_playlist(&d.generations[&1], |i| format!("s/{i}"), "init.mp4");
        assert!(text.ends_with("s/19\n#EXT-X-ENDLIST\n"));
        // A segment longer than the pinned target fails the generation.
        let (mut e, _) = Delivery::admit("t".into(), pin(&["a1"]), 0, None, 0).unwrap();
        step(&mut e, ready(1, 0)).unwrap();
        step(&mut e, segment(1, 6_500)).unwrap();
        assert_eq!(e.status, Status::Failed);
    }

    #[test]
    fn workers_pause_far_ahead_of_the_playhead_and_resume_behind_it() {
        let (mut d, _) = Delivery::admit("t".into(), pin(&["a1"]), 0, None, 0).unwrap();
        step(&mut d, ready(1, 0)).unwrap();
        let mut paced = Vec::new();
        for index in 1..12 {
            paced.extend(
                step(
                    &mut d,
                    Input::Segment {
                        generation: 1,
                        index,
                        duration_ms: 4000,
                        now_ms: 1,
                    },
                )
                .unwrap()
                .into_iter()
                .filter(|e| matches!(e, Effect::Pace { .. })),
            );
        }
        // 48 s produced while the client is at 0: paused once at 48 s >= 45 s.
        assert_eq!(
            paced,
            [Effect::Pace {
                generation: 1,
                paused: true
            }]
        );
        let beat = |position| Input::Heartbeat {
            active_generation: 1,
            position_ms: Some(position),
            now_ms: 2,
        };
        // Within the hysteresis band nothing changes; behind it the worker resumes.
        assert_eq!(step(&mut d, beat(10_000)).unwrap(), []);
        assert_eq!(
            step(&mut d, beat(20_000)).unwrap(),
            [Effect::Pace {
                generation: 1,
                paused: false
            }]
        );
    }
}
