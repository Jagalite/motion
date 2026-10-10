use playscale_core::delivery::{
    DRAIN_MS, Delivery, Effect, Error, GenerationStatus as G, Input, LEASE_MS, Operation, POLICY,
    Pin, Policy, Replacement, Status, WINDOW, Worker, fits_target, transition,
};
use stateless::{
    Check, Disposition, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition,
    TransitionRef,
};

/// The production delivery plus an explicit clock. Clock advances are inputs, so
/// lease expiry and drain deadlines are explored as interleavings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct World {
    delivery: Delivery,
    now: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum Step {
    Delivery(Input),
    /// Advance the clock without any delivery input.
    Clock(u64),
}

const TIMELINE: &str = "timeline";
const TARGET: u64 = 6;

fn pin(tracks: &[&str], operation: Operation) -> Pin {
    Pin {
        source_file: "file".into(),
        source_revision: "rev".into(),
        tracks: tracks.iter().map(|t| (*t).into()).collect(),
        operation,
        recipe_digest: operation.segmented().then(|| "h264-aac".into()),
    }
}

/// Bounded input domain. `segment_indices`/`durations` drive rolling-window
/// behavior; `changes` allows generation replacement.
struct Deliveries {
    policy: Policy,
    heartbeat_positions: &'static [Option<u64>],
    duration: Option<u64>,
    first: Pin,
    max_generations: u64,
    changes: bool,
    positions: &'static [u64],
    origins: &'static [u64],
    /// Media time of segment 0's start (0 = encoder starts at the request;
    /// otherwise a stream-copy keyframe).
    first_starts: &'static [u64],
    segment_indices: u32,
    durations: &'static [u64],
    clock_steps: &'static [u64],
    max_now: u64,
}

impl Enumerate for Deliveries {
    fn inputs(&self, w: &World) -> Result<Vec<Step>, ModelError> {
        let d = &w.delivery;
        let now = w.now;
        let mut inputs = vec![
            Step::Delivery(Input::Tick { now_ms: now }),
            Step::Delivery(Input::Close),
            Step::Delivery(Input::Interrupt),
        ];
        for step in self.clock_steps {
            if now + step <= self.max_now {
                inputs.push(Step::Clock(*step));
            }
        }
        // Current, stale and never-issued generations.
        for g in 1..=d.last_generation + 1 {
            for position in self.heartbeat_positions {
                let position = *position;
                inputs.push(Step::Delivery(Input::Heartbeat {
                    active_generation: g,
                    position_ms: position,
                    now_ms: now,
                }));
            }
            for origin in self.origins {
                for first_start in self.first_starts {
                    inputs.push(Step::Delivery(Input::Ready {
                        generation: g,
                        media_time_origin_ms: *origin,
                        first_segment_start_ms: *first_start,
                        first_segment_ms: 4_000,
                        target_duration_s: TARGET,
                    }));
                }
            }
            // The next index (plus one early index) with each duration.
            let next = d.generations.get(&g).map_or(1, |x| x.next_segment.max(1));
            for index in [next, next + 1] {
                if index <= self.segment_indices {
                    for duration in self.durations {
                        inputs.push(Step::Delivery(Input::Segment {
                            generation: g,
                            index,
                            duration_ms: *duration,
                            now_ms: now,
                        }));
                    }
                }
            }
            for final_index in [next.saturating_sub(1), next] {
                inputs.push(Step::Delivery(Input::Completed {
                    generation: g,
                    final_index,
                }));
            }
            inputs.extend(
                [
                    Input::Failed { generation: g },
                    Input::Stopped { generation: g },
                ]
                .map(Step::Delivery),
            );
            for expected in [None, Some(g - 1), Some(g)] {
                inputs.push(Step::Delivery(Input::Activate {
                    generation: g,
                    expected_active: expected.filter(|e| *e > 0),
                    now_ms: now,
                }));
            }
            if self.changes && d.last_generation < self.max_generations {
                for position in self.positions {
                    for replan in [
                        None,
                        Some((
                            TIMELINE.to_owned(),
                            pin(&["audio-2", "subtitle-1"], Operation::VideoTranscode),
                        )),
                        Some((TIMELINE.to_owned(), pin(&["audio-1"], Operation::Original))),
                        Some((
                            "other-cut".to_owned(),
                            pin(&["audio-1"], Operation::VideoTranscode),
                        )),
                    ] {
                        for overlap in [false, true] {
                            inputs.push(Step::Delivery(Input::Change {
                                expected_generation: g,
                                position_ms: *position,
                                replan: replan.clone(),
                                overlap,
                                now_ms: now,
                            }));
                        }
                    }
                }
            }
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Deliveries {
    fn generate(&self, w: &World, rng: &mut stateless::Rng) -> Result<Option<Step>, ModelError> {
        let inputs = self.inputs(w)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}

fn error_name(e: Error) -> String {
    format!("{e:?}")
}

impl Model for Deliveries {
    type State = World;
    type Input = Step;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.delivery".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}:{}",
                env!("CARGO_PKG_VERSION"),
                include_str!("../src/delivery.rs")
            ),
        }
    }
    fn initial_state(&self) -> Result<World, ModelError> {
        let (delivery, _) = Delivery::admit_with_policy(
            self.policy,
            TIMELINE.into(),
            self.first.clone(),
            0,
            self.duration,
            0,
        )
        .map_err(|e| ModelError::new(error_name(e)))?;
        Ok(World { delivery, now: 0 })
    }
    fn step(&self, w: &World, step: &Step) -> Result<Transition<World, Effect>, ModelError> {
        Ok(match step {
            Step::Clock(ms) => Transition::accepted(
                World {
                    delivery: w.delivery.clone(),
                    now: w.now + ms,
                },
                vec![],
            ),
            Step::Delivery(input) => match transition(&w.delivery, input) {
                Ok((delivery, effects)) => Transition::accepted(
                    World {
                        delivery,
                        now: w.now,
                    },
                    effects,
                ),
                Err(e) => Transition {
                    state: w.clone(),
                    outputs: vec![],
                    disposition: Disposition::Rejected(error_name(e)),
                },
            },
        })
    }
    fn check_state(&self, w: &World) -> Result<Vec<Check>, ModelError> {
        let d = &w.delivery;
        let numbers = |f: &dyn Fn(&playscale_core::delivery::Generation) -> bool| {
            d.generations
                .iter()
                .filter(|(_, g)| f(g))
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
        };
        let running = numbers(&|g| g.worker == Worker::Live);
        let serving_worker = d.active.filter(|a| {
            d.generations
                .get(a)
                .is_some_and(|g| g.status == G::Active && g.worker.running())
        });
        Ok(vec![
            check(
                "single_active_and_pending",
                numbers(&|g| g.status == G::Active)
                    .iter()
                    .all(|n| Some(*n) == d.active)
                    && numbers(&|g| matches!(g.status, G::Starting | G::Ready))
                        == d.pending.into_iter().collect::<Vec<_>>()
                    && numbers(&|g| g.worker == Worker::Deferred)
                        .iter()
                        .all(|n| Some(*n) == d.pending),
            ),
            check(
                "fenced_delivery_has_no_current_generation",
                !d.status.fenced()
                    || (d.active.is_none()
                        && d.pending.is_none()
                        && d.generations.values().all(|g| !g.status.current())),
            ),
            check(
                "closed_exactly_after_every_worker_and_file_is_gone",
                (d.status == Status::Closed)
                    == (matches!(d.status, Status::Closing | Status::Closed)
                        && d.generations.is_empty()),
            ),
            check(
                "generations_were_issued",
                d.generations
                    .keys()
                    .all(|n| (1..=d.last_generation).contains(n)),
            ),
            // A second live (not stopping) worker is only ever an admitted overlap
            // candidate beside the serving generation's worker.
            check(
                "workers_overlap_only_when_admitted",
                running.len() <= 1
                    || (running.len() == 2
                        && d.replacement == Replacement::Overlap
                        && serving_worker.is_some()
                        && running
                            .iter()
                            .all(|n| Some(*n) == serving_worker || Some(*n) == d.pending)),
            ),
            check(
                "window_is_contiguous_accounted_and_long_enough",
                d.generations
                    .values()
                    .filter(|g| g.pin.operation.segmented())
                    .all(|g| {
                        let total: u64 = g.segments.iter().map(|s| s.duration_ms).sum();
                        g.segments.windows(2).all(|s| s[0].index + 1 == s[1].index)
                            && g.segments
                                .last()
                                .is_none_or(|s| s.index + 1 == g.next_segment)
                            && g.available_end_ms - g.available_start_ms == total
                            && g.segments
                                .iter()
                                .all(|s| fits_target(s.duration_ms, TARGET))
                            && (g.segments.first().is_none_or(|s| s.index == 0)
                                || (g.segments.len() >= WINDOW && total >= 3_000 * TARGET))
                            && self
                                .duration
                                .is_none_or(|limit| g.available_end_ms <= limit + 1_000)
                    }),
            ),
            check(
                "retained_segments_precede_window_in_order",
                d.generations.values().all(|g| {
                    let first = g.segments.first().map_or(g.next_segment, |s| s.index);
                    g.retained.windows(2).all(|r| r[0].index < r[1].index)
                        && g.retained.iter().all(|r| r.index < first)
                }),
            ),
            check(
                "longest_playlist_bounds_current_window",
                d.generations.values().all(|g| {
                    g.longest_playlist_ms >= g.segments.iter().map(|s| s.duration_ms).sum::<u64>()
                }),
            ),
            // Hysteresis: a live, unfinished generation is paused at or beyond the
            // pause lead and running below the resume lead.
            check(
                "pace_follows_lead",
                d.status.fenced()
                    || d.generations.iter().all(|(n, g)| {
                        if g.worker != Worker::Live || !g.status.current() || g.complete {
                            return true;
                        }
                        let reference = match d.playhead {
                            Some((generation, position)) if generation == *n => position,
                            _ => g.requested_start_ms,
                        };
                        let ahead = g.available_end_ms.saturating_sub(reference);
                        (ahead < d.policy.ahead_pause_ms || g.paused)
                            && (ahead >= d.policy.ahead_resume_ms || !g.paused)
                    }),
            ),
            check(
                "segment_zero_covers_requested_start",
                d.generations.values().all(|g| {
                    !g.pin.operation.segmented()
                        || g.next_segment == 0
                        || (g.segment_zero_start_ms <= g.requested_start_ms
                            && g.requested_start_ms < g.segment_zero_start_ms + 4_000
                            // Until eviction, the window starts at segment 0.
                            && (g.segments.first().is_none_or(|s| s.index != 0)
                                || g.available_start_ms == g.segment_zero_start_ms))
                }),
            ),
            check(
                "playable_generation_has_worker_or_is_complete",
                d.generations
                    .values()
                    .all(|g| !g.status.current() || g.worker != Worker::Idle || g.complete),
            ),
            check(
                "serving_requires_lease_and_current_status",
                d.generations.iter().all(|(n, g)| {
                    !d.serves(*n, w.now)
                        || (w.now < d.lease_expires_ms && matches!(g.status, G::Ready | G::Active))
                }),
            ),
        ])
    }
    fn check_transition(
        &self,
        before: &World,
        step: &Step,
        next: &TransitionRef<'_, World, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let (b, a) = (&before.delivery, &next.state.delivery);
        let Step::Delivery(input) = step else {
            return Ok(vec![check(
                "clock_changes_only_time",
                a == b && next.outputs.is_empty(),
            )]);
        };
        let now = before.now;
        let rejected = match next.disposition {
            Disposition::Rejected(reason) => Some(reason.as_str()),
            _ => None,
        };
        let mut starts = Vec::new();
        let mut stops = Vec::new();
        let mut cleanups = Vec::new();
        let mut discards = Vec::new();
        let mut paces = Vec::new();
        for e in next.outputs {
            match e {
                Effect::Pace { generation, paused } => paces.push((*generation, *paused)),
                Effect::Start { generation } => starts.push(*generation),
                Effect::Stop { generation } => stops.push(*generation),
                Effect::Cleanup { generation } => cleanups.push(*generation),
                Effect::Discard {
                    generation,
                    below_index,
                } => discards.push((*generation, *below_index)),
            }
        }
        let worker = |d: &Delivery, n: &u64| d.generations.get(n).map(|g| g.worker);
        let created: Vec<u64> = (b.last_generation + 1..=a.last_generation).collect();
        let sorted = |mut v: Vec<u64>| {
            v.sort();
            v
        };
        let expected_starts: Vec<u64> = a
            .generations
            .keys()
            .filter(|n| {
                worker(a, n) == Some(Worker::Live)
                    && matches!(worker(b, n), None | Some(Worker::Deferred))
            })
            .copied()
            .collect();
        // A live worker is stopped when it is now stopping, or was forgotten
        // without an exit observation (which would be a lost worker).
        let expected_stops: Vec<u64> = b
            .generations
            .keys()
            .filter(|n| {
                worker(b, n) == Some(Worker::Live)
                    && worker(a, n).is_none_or(|w| w == Worker::Stopping)
                    && !matches!(input, Input::Stopped { generation } if generation == *n)
                    && *input != Input::Interrupt
            })
            .copied()
            .collect();
        let removed: Vec<u64> = b
            .generations
            .keys()
            .filter(|n| !a.generations.contains_key(n))
            .copied()
            .collect();
        // A retained segment is dropped exactly when its deadline has passed.
        let input_now = match input {
            Input::Segment { now_ms, .. } | Input::Tick { now_ms } => Some(*now_ms),
            _ => None,
        };
        let expected_discards: Vec<(u64, u32)> = a
            .generations
            .iter()
            .filter_map(|(n, g)| {
                let old = b.generations.get(n)?;
                let dropped = old.retained.iter().any(|r| !g.retained.contains(r));
                dropped.then(|| {
                    let below = g
                        .retained
                        .first()
                        .map(|r| r.index)
                        .or(g.segments.first().map(|s| s.index))
                        .unwrap_or(g.next_segment);
                    (*n, below)
                })
            })
            .collect();
        let retention_on_time = a.generations.iter().all(|(n, g)| {
            let Some(old) = b.generations.get(n) else {
                return true;
            };
            // Dropped only after the deadline.
            old.retained.iter().all(|r| {
                g.retained.contains(r) || input_now.is_some_and(|t| t >= r.until_ms)
            })
                // Every segment leaving the playlist stays fetchable for at least its
                // own duration plus the playlist it left.
                && old.segments.iter().all(|s| {
                    g.segments.contains(s)
                        || g.retained.iter().any(|r| {
                            r.index == s.index
                                && input_now.is_some_and(|t| {
                                    r.until_ms >= t + s.duration_ms + old.longest_playlist_ms
                                })
                        })
                })
                // Expired entries are not kept.
                && input_now.is_none_or(|t| g.retained.iter().all(|r| t < r.until_ms))
        });
        let expected_paces: Vec<(u64, bool)> = a
            .generations
            .iter()
            .filter(|(n, g)| g.paused != b.generations.get(n).is_some_and(|o| o.paused))
            .map(|(n, g)| (*n, g.paused))
            .collect();
        // Inside the hysteresis band a live worker keeps its previous pause state.
        let band_holds = a.status.fenced()
            || a.generations.iter().all(|(n, g)| {
                let Some(old) = b.generations.get(n) else {
                    return true;
                };
                if g.worker != Worker::Live || !g.status.current() || g.complete {
                    return true;
                }
                let reference = match a.playhead {
                    Some((generation, position)) if generation == *n => position,
                    _ => g.requested_start_ms,
                };
                let ahead = g.available_end_ms.saturating_sub(reference);
                let in_band = ahead >= a.policy.ahead_resume_ms && ahead < a.policy.ahead_pause_ms;
                !in_band || old.worker != Worker::Live || g.paused == old.paused
            });
        let heartbeat_recorded = match input {
            Input::Heartbeat {
                active_generation,
                position_ms,
                ..
            } if rejected.is_none() => match position_ms {
                Some(p) if b.active == Some(*active_generation) => {
                    a.playhead == Some((*active_generation, *p))
                }
                _ => a.playhead == b.playhead,
            },
            Input::Heartbeat { .. } => a.playhead == b.playhead,
            _ => true,
        };
        let longest_monotonic = a.generations.iter().all(|(n, g)| {
            b.generations
                .get(n)
                .is_none_or(|o| g.longest_playlist_ms >= o.longest_playlist_ms)
        });
        let changed = {
            let mut a2 = a.clone();
            a2.revision = b.revision;
            a2 != *b
        };
        let pins_kept = b
            .generations
            .iter()
            .all(|(n, g)| a.generations.get(n).is_none_or(|x| x.pin == g.pin));
        let selection = b.pending.or(b.active).and_then(|n| b.generations.get(&n));
        let new_pin_ok = created.iter().all(|n| {
            let pin = &a.generations[n].pin;
            match input {
                Input::Change { replan: None, .. } => selection.is_some_and(|s| s.pin == *pin),
                Input::Change {
                    replan: Some((_, p)),
                    ..
                } => p == pin,
                _ => false,
            }
        });
        let output_changed = |n: &u64| {
            a.generations.get(n).is_some_and(|g| {
                b.generations.get(n).is_some_and(|old| {
                    old.segments != g.segments
                        || old.complete != g.complete
                        || old.next_segment != g.next_segment
                })
            })
        };
        let output_from_producer = a.generations.keys().filter(|n| output_changed(n)).all(|n| {
            b.generations.get(n).is_some_and(|g| {
                g.worker == Worker::Live && !b.status.fenced() && g.status.current()
            }) && match input {
                Input::Ready { generation, .. }
                | Input::Segment { generation, .. }
                | Input::Completed { generation, .. } => generation == n,
                _ => false,
            }
        });
        // A Ready that satisfies the request makes the generation playable with
        // exactly the reported interval; otherwise the generation fails.
        let ready_effective = match input {
            Input::Ready {
                generation,
                media_time_origin_ms,
                first_segment_start_ms,
                first_segment_ms,
                target_duration_s,
            } if b.generations.get(generation).is_some_and(|g| {
                g.status == G::Starting && g.worker == Worker::Live && !b.status.fenced()
            }) =>
            {
                let g = &b.generations[generation];
                let begin = media_time_origin_ms + first_segment_start_ms;
                let end = begin + first_segment_ms;
                let valid = begin <= g.requested_start_ms
                    && g.requested_start_ms < end
                    && self.duration.is_none_or(|limit| end <= limit + 1_000)
                    && (1..=20).contains(target_duration_s)
                    && fits_target(*first_segment_ms, *target_duration_s);
                match a.generations.get(generation) {
                    Some(x) if valid => {
                        matches!(x.status, G::Ready | G::Active)
                            && x.segment_zero_start_ms == begin
                            && x.available_start_ms == begin
                            && x.available_end_ms == end
                    }
                    Some(x) => x.status == G::Failed,
                    None => !valid,
                }
            }
            _ => true,
        };
        let created_fresh = created.iter().all(|n| {
            let g = &a.generations[n];
            g.segments.is_empty() && g.next_segment == 0
        });
        // Fuzz mutation can replay an input recorded at another clock value: use
        // the time the input carries.
        let now = match input {
            Input::Change { now_ms, .. }
            | Input::Activate { now_ms, .. }
            | Input::Heartbeat { now_ms, .. } => *now_ms,
            _ => now,
        };
        let lease_ok = !b.status.fenced() && now < b.lease_expires_ms;
        let expected_error = match input {
            Input::Change { .. } | Input::Activate { .. } | Input::Heartbeat { .. }
                if !lease_ok =>
            {
                Some(Error::DeliveryClosed)
            }
            Input::Change {
                expected_generation,
                ..
            } if b.active != Some(*expected_generation) => Some(Error::GenerationConflict),
            Input::Change {
                replan: Some((t, _)),
                ..
            } if *t != b.timeline => Some(Error::TimelineChanged),
            Input::Change { position_ms, .. }
                if self.duration.is_some_and(|d| *position_ms >= d) =>
            {
                Some(Error::InvalidPosition)
            }
            Input::Heartbeat {
                active_generation, ..
            } if ![b.active, b.pending].contains(&Some(*active_generation)) => {
                Some(Error::GenerationConflict)
            }
            Input::Heartbeat {
                position_ms: Some(p),
                ..
            } if self.duration.is_some_and(|d| *p > d + 1_000) => Some(Error::InvalidPosition),
            Input::Activate {
                generation,
                expected_active,
                ..
            } if b.active != Some(*generation) && b.active != *expected_active => {
                Some(Error::GenerationConflict)
            }
            _ => None,
        };
        // A valid command must take effect, not merely avoid an error.
        let command_effective = match input {
            Input::Activate {
                generation,
                expected_active,
                ..
            } if lease_ok
                && b.active == *expected_active
                && b.pending == Some(*generation)
                && b.generations[generation].status == G::Ready =>
            {
                rejected.is_none()
                    && a.active == Some(*generation)
                    && a.generations[generation].status == G::Active
                    && a.pending.is_none()
                    && a.status == Status::Ready
            }
            // GenerationLimit is the only other rejection and is outside these bounds.
            Input::Change { .. } if expected_error.is_none() && lease_ok => {
                rejected.is_none() && created.len() == 1 && a.pending == created.first().copied()
            }
            Input::Heartbeat { now_ms, .. } if expected_error.is_none() => {
                a.lease_expires_ms >= now_ms + LEASE_MS
            }
            _ => true,
        };
        let displaced = b
            .active
            .filter(|old| Some(*old) != a.active && a.generations.contains_key(old));
        let disruptive_defers = match input {
            Input::Change { overlap: false, .. }
                if rejected.is_none()
                    && b.active.is_some_and(|x| {
                        b.generations
                            .get(&x)
                            .is_some_and(|g| g.status == G::Active && g.worker == Worker::Live)
                    }) =>
            {
                created.iter().all(|n| {
                    !a.generations[n].pin.operation.segmented()
                        || a.generations[n].worker == Worker::Deferred
                }) && starts.is_empty()
            }
            _ => true,
        };
        let incomplete_completion = matches!(input, Input::Completed { generation, final_index }
            if b.generations.get(generation).is_some_and(|g| *final_index + 1 != g.next_segment));
        Ok(vec![
            check(
                "expected_rejections",
                expected_error.is_none_or(|e| rejected == Some(error_name(e).as_str())),
            ),
            check(
                "execution_reports_never_rejected",
                rejected.is_none()
                    || matches!(
                        input,
                        Input::Change { .. } | Input::Activate { .. } | Input::Heartbeat { .. }
                    ),
            ),
            check("timeline_immutable", a.timeline == b.timeline),
            check(
                "revision_tracks_change",
                a.revision == b.revision + u64::from(changed),
            ),
            check(
                "start_exactly_when_worker_starts",
                sorted(starts.clone()) == expected_starts && created.len() <= 1,
            ),
            check(
                "start_waits_for_displaced_workers",
                starts.iter().all(|g| {
                    let coexisting = a.active.filter(|x| {
                        a.replacement == Replacement::Overlap
                            && a.generations.get(x).is_some_and(|w| w.status == G::Active)
                    });
                    a.generations
                        .iter()
                        .all(|(n, w)| n == g || Some(*n) == coexisting || !w.worker.running())
                }),
            ),
            check(
                "stop_exactly_when_live_worker_is_stopped",
                sorted(stops) == expected_stops,
            ),
            check(
                "running_worker_ends_only_by_observation",
                b.generations.iter().all(|(n, g)| {
                    !g.worker.running()
                        || a.generations.get(n).is_some_and(|x| x.worker.running())
                        || matches!(input, Input::Stopped { generation } if generation == n)
                        || *input == Input::Interrupt
                }),
            ),
            check(
                "cleanup_exactly_when_forgotten_without_worker",
                sorted(cleanups) == removed
                    && removed.iter().all(|n| {
                        !b.generations[n].worker.running()
                            || matches!(input, Input::Stopped { generation } if generation == n)
                            || *input == Input::Interrupt
                    }),
            ),
            check(
                "discard_exactly_on_retention_advance",
                discards == expected_discards && retention_on_time,
            ),
            check(
                "forgotten_generation_never_returns",
                a.generations
                    .keys()
                    .all(|n| b.generations.contains_key(n) || created.contains(n)),
            ),
            check("track_choice_preserved", pins_kept && new_pin_ok),
            check("pace_exactly_on_flip", {
                let mut p = paces.clone();
                p.sort();
                p == expected_paces
            }),
            check("longest_playlist_monotonic", longest_monotonic),
            check("pause_state_held_in_band", band_holds),
            check("heartbeat_records_generation_playhead", heartbeat_recorded),
            check(
                "generation_fenced_output",
                output_from_producer && created_fresh,
            ),
            check("valid_command_takes_effect", command_effective),
            check("ready_takes_effect_with_exact_interval", ready_effective),
            check(
                "activation_requires_expected_active",
                !matches!(input, Input::Activate { .. })
                    || a.active == b.active
                    || matches!(input, Input::Activate { expected_active, generation, .. }
                        if *expected_active == b.active && a.active == Some(*generation)),
            ),
            check(
                "displaced_generation_not_served_and_stopping",
                displaced.is_none_or(|old| {
                    let g = &a.generations[&old];
                    !g.status.current() && g.worker != Worker::Live
                }),
            ),
            check("disruptive_change_defers_new_worker", disruptive_defers),
            check(
                "failed_candidate_keeps_active",
                !matches!(input, Input::Failed { generation }
                    if b.active.is_some_and(|x| b.generations.get(&x).is_some_and(|g| g.status == G::Active))
                        && b.active != Some(*generation))
                    || a.active == b.active,
            ),
            check(
                "lease_expiry_fences",
                !matches!(input, Input::Tick { now_ms } if *now_ms >= b.lease_expires_ms)
                    || a.status.fenced(),
            ),
            check(
                "incomplete_final_index_never_ends_playlist",
                !incomplete_completion
                    || a.generations.iter().all(|(n, g)| {
                        g.complete == b.generations.get(n).is_some_and(|o| o.complete)
                    }),
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "delivery invariant violated")
    }
}
fn encode<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
impl ModelCodec for Deliveries {
    fn encode_state(&self, v: &World) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<World, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Step) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Step, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}

/// Scaled-down policy so pacing and eviction occur within model bounds.
const MODEL_POLICY: Policy = Policy {
    min_window_ms: 0,
    ahead_pause_ms: 30_000,
    ahead_resume_ms: 18_000,
};

/// Replacement lifecycle: up to three generations, byte and HLS routes, clock
/// boundaries at drain and lease deadlines (and one millisecond before them).
fn lifecycle() -> Deliveries {
    Deliveries {
        policy: MODEL_POLICY,
        // Playhead reports are explored by `playhead`, where pacing can trigger.
        heartbeat_positions: &[None],
        duration: Some(12_000),
        first: pin(&["audio-1", "subtitle-off"], Operation::VideoTranscode),
        max_generations: 3,
        changes: true,
        positions: &[6_000, 12_000],
        origins: &[0, 6_000],
        first_starts: &[0, 4_000],
        segment_indices: 1,
        durations: &[4_000],
        clock_steps: &[DRAIN_MS],
        max_now: LEASE_MS,
    }
}

/// Rolling window: one generation of unknown duration, variable segment lengths.
fn window() -> Deliveries {
    Deliveries {
        policy: MODEL_POLICY,
        heartbeat_positions: &[None, Some(12_000)],
        duration: None,
        first: pin(&["audio-1"], Operation::VideoTranscode),
        max_generations: 1,
        changes: false,
        positions: &[],
        origins: &[0],
        first_starts: &[0],
        segment_indices: 8,
        durations: &[1_000, 6_000],
        clock_steps: &[24_000],
        max_now: 48_000,
    }
}

/// Generation-bound playhead and pacing: two generations, thresholds small
/// enough that segments pause and resume workers, playheads for either.
fn playhead() -> Deliveries {
    Deliveries {
        policy: Policy {
            min_window_ms: 0,
            ahead_pause_ms: 8_000,
            ahead_resume_ms: 4_000,
        },
        // One reported position: alternating values would grow the revision forever.
        heartbeat_positions: &[None, Some(6_000)],
        duration: Some(12_000),
        first: pin(&["audio-1"], Operation::VideoTranscode),
        max_generations: 2,
        changes: true,
        positions: &[6_000],
        origins: &[0, 6_000],
        first_starts: &[0],
        segment_indices: 2,
        durations: &[4_000],
        clock_steps: &[],
        max_now: 0,
    }
}

fn exhaust(model: &Deliveries, label: &str) {
    let report = stateless::explore::enumerate(
        model,
        stateless::explore::SearchConfig {
            max_states: 3_000_000,
            max_transitions: 300_000_000,
            max_depth: 200,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Stateless delivery {label}: {} states, {} edges",
        report.states, report.transitions
    );
}

#[test]
fn bounded_delivery_lifecycle_checks_production_reducer() {
    exhaust(
        &lifecycle(),
        "lifecycle (<=3 generations, byte+HLS, clock 0..lease in drain steps)",
    );
}

#[test]
fn bounded_delivery_playhead_checks_production_reducer() {
    exhaust(
        &playhead(),
        "playhead (2 generations, playhead 6s, heartbeats for active and pending, pause 8s / resume 4s)",
    );
}

#[test]
fn bounded_delivery_window_checks_production_reducer() {
    exhaust(
        &window(),
        "window (segments 0..8, 1s/6s, clock 0..48s in 24s steps, pacing 30/18s, unknown duration)",
    );
}

#[test]
fn seeded_delivery_sequences_with_more_generations() {
    let report = stateless::explore::fuzz(
        &Deliveries {
            policy: POLICY,
            heartbeat_positions: &[None, Some(0), Some(30_000), Some(61_001)],
            duration: Some(60_000),
            first: pin(&["audio-1"], Operation::Original),
            max_generations: 8,
            changes: true,
            positions: &[0, 6_000, 30_000, 59_999, 60_000],
            origins: &[0, 6_000, 28_000, 30_000],
            first_starts: &[0],
            segment_indices: 20,
            durations: &[1_000, 4_000, 6_000, 6_600],
            clock_steps: &[1, DRAIN_MS - 1, LEASE_MS],
            max_now: 10 * LEASE_MS,
        },
        stateless::explore::FuzzConfig {
            seed: 20261009,
            cases: 3000,
            max_steps: 150,
            max_transitions: 450_000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::FuzzTermination::CasesCompleted
    );
    assert_eq!(report.cases, 3000);
    assert_eq!(report.transitions, 450000);
    println!(
        "Stateless delivery seeded sequences: seed {}; {} cases, {} transitions; {:?}",
        report.seed, report.cases, report.transitions, report.termination
    );
}

/// Exact end-of-duration tolerance and generation/lease error precedence do not
/// depend on a random sequence reaching a valid heartbeat at the boundary.
#[test]
fn heartbeat_boundaries_renew_only_valid_generation_leases() {
    let (initial, _) = Delivery::admit(
        TIMELINE.into(),
        pin(&["audio-1"], Operation::VideoTranscode),
        0,
        Some(60_000),
        0,
    )
    .unwrap();
    let (active, _) = transition(
        &initial,
        &Input::Ready {
            generation: 1,
            media_time_origin_ms: 0,
            first_segment_start_ms: 0,
            first_segment_ms: 4_000,
            target_duration_s: TARGET,
        },
    )
    .unwrap();
    let (staged, _) = transition(
        &active,
        &Input::Change {
            expected_generation: 1,
            position_ms: 30_000,
            replan: None,
            overlap: true,
            now_ms: 1,
        },
    )
    .unwrap();
    for generation in [1, 2] {
        for position in [0, 60_000, 61_000] {
            let (after, effects) = transition(
                &staged,
                &Input::Heartbeat {
                    active_generation: generation,
                    position_ms: Some(position),
                    now_ms: 2,
                },
            )
            .unwrap();
            assert_eq!(after.lease_expires_ms, 2 + LEASE_MS);
            assert_eq!(after.revision, staged.revision + 1);
            assert_eq!(after.playhead, (generation == 1).then_some((1, position)));
            assert_eq!(effects, []);
        }
        for position in [61_001, u64::MAX] {
            assert_eq!(
                transition(
                    &staged,
                    &Input::Heartbeat {
                        active_generation: generation,
                        position_ms: Some(position),
                        now_ms: 2,
                    }
                ),
                Err(Error::InvalidPosition)
            );
        }
        assert_eq!(
            transition(
                &staged,
                &Input::Heartbeat {
                    active_generation: generation,
                    position_ms: Some(0),
                    now_ms: staged.lease_expires_ms,
                }
            ),
            Err(Error::DeliveryClosed)
        );
    }
    assert_eq!(
        transition(
            &staged,
            &Input::Heartbeat {
                active_generation: 3,
                position_ms: Some(u64::MAX),
                now_ms: 2,
            }
        ),
        Err(Error::GenerationConflict)
    );
    assert_eq!(
        transition(
            &staged,
            &Input::Heartbeat {
                active_generation: 3,
                position_ms: Some(u64::MAX),
                now_ms: staged.lease_expires_ms,
            }
        ),
        Err(Error::DeliveryClosed)
    );
}

#[test]
fn seek_switch_and_stale_segment_replay_exactly() {
    let model = lifecycle();
    let inputs = vec![
        Step::Delivery(Input::Ready {
            generation: 1,
            media_time_origin_ms: 0,
            first_segment_start_ms: 0,
            first_segment_ms: 4_000,
            target_duration_s: TARGET,
        }),
        Step::Delivery(Input::Change {
            expected_generation: 1,
            position_ms: 6_000,
            replan: None,
            overlap: true,
            now_ms: 0,
        }),
        Step::Delivery(Input::Ready {
            generation: 2,
            media_time_origin_ms: 6_000,
            first_segment_start_ms: 0,
            first_segment_ms: 4_000,
            target_duration_s: TARGET,
        }),
        Step::Delivery(Input::Activate {
            generation: 2,
            expected_active: Some(1),
            now_ms: 0,
        }),
        Step::Delivery(Input::Segment {
            generation: 1,
            index: 1,
            duration_ms: 4_000,
            now_ms: 0,
        }),
        Step::Clock(DRAIN_MS),
        Step::Delivery(Input::Stopped { generation: 1 }),
        Step::Delivery(Input::Tick { now_ms: DRAIN_MS }),
    ];
    let trace = stateless::execution::record(&model, inputs, Default::default(), 100).unwrap();
    let replay = stateless::execution::replay(&model, &trace, Default::default()).unwrap();
    assert_eq!(replay.outcome, stateless::execution::ReplayOutcome::Exact);
    assert_eq!(replay.steps_verified, 8);
}
