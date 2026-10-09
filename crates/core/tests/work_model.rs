use playscale_core::work::{
    Budget, Class, Effect, Hold, Input, Ledger, Reservation, Waiter, transition,
};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

const BUDGET: Budget = Budget {
    units: 3,
    interactive_reserve: 1,
};

/// Finite input domain. Properties are stated over observable reservations and
/// effects rather than by re-running the admission policy.
struct Work {
    owners: &'static [&'static str],
    classes: &'static [Class],
    units: &'static [u32],
    /// Requests are offered only while fewer tickets were issued (bounds the graph).
    max_tickets: u64,
}
impl stateless::Generate for Work {
    fn generate(
        &self,
        state: &Ledger,
        rng: &mut stateless::Rng,
    ) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(state)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl Enumerate for Work {
    fn inputs(&self, state: &Ledger) -> Result<Vec<Input>, ModelError> {
        let mut inputs = Vec::new();
        if state.last_ticket < self.max_tickets {
            for owner in self.owners {
                for class in self.classes {
                    for units in self.units {
                        inputs.push(Input::Request {
                            owner: (*owner).into(),
                            class: *class,
                            units: *units,
                        });
                    }
                }
            }
        }
        // Every issued ticket (current and stale) plus one never issued.
        for ticket in 1..=state.last_ticket + 1 {
            inputs.extend([
                Input::Cancel { ticket },
                Input::Exited { ticket },
                Input::Stuck { ticket },
            ]);
        }
        Ok(inputs)
    }
}

/// Independent statement of "could fit once every non-stuck holder exits".
fn fits_beside_stuck(l: &Ledger, class: Class, units: u32) -> bool {
    let (mut all, mut background) = (u64::from(units), u64::from(units));
    for r in l.held.values().filter(|r| r.hold == Hold::Stuck) {
        all += u64::from(r.units);
        if !r.class.interactive() {
            background += u64::from(r.units);
        }
    }
    all <= u64::from(l.budget.units)
        && (class.interactive()
            || background <= u64::from(l.budget.units - l.budget.interactive_reserve))
}

/// Specification of admission precedence: interactive first, then background
/// classes rotating after `last_background`, then arrival (ticket) order.
fn rank(l: &Ledger, w: &Waiter) -> (usize, u64) {
    const ROTATION: [Class; 3] = [Class::Preparation, Class::Inventory, Class::Maintenance];
    let start = l
        .last_background
        .and_then(|c| ROTATION.iter().position(|r| *r == c))
        .map_or(0, |i| i + 1);
    let class = match ROTATION.iter().position(|r| *r == w.class) {
        None => 0,
        Some(i) => 1 + (i + ROTATION.len() - start) % ROTATION.len(),
    };
    (class, w.ticket)
}

/// Waiters that may justify withholding later work: they do not fit now but can
/// be served once live (non-stuck) holders exit.
fn legitimately_blocked(l: &Ledger, w: &Waiter) -> bool {
    !l.fits(w.class, w.units) && fits_beside_stuck(l, w.class, w.units)
}

impl Model for Work {
    type State = Ledger;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.work".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}:{}",
                env!("CARGO_PKG_VERSION"),
                include_str!("../src/work.rs")
            ),
        }
    }
    fn initial_state(&self) -> Result<Ledger, ModelError> {
        Ok(Ledger::new(BUDGET))
    }
    fn step(&self, s: &Ledger, i: &Input) -> Result<Transition<Ledger, Effect>, ModelError> {
        let (state, outputs) = transition(s, i);
        Ok(Transition::accepted(state, outputs))
    }
    fn check_state(&self, s: &Ledger) -> Result<Vec<Check>, ModelError> {
        let mut owners: Vec<&str> = s.waiting.iter().map(|w| w.owner.as_str()).collect();
        owners.extend(s.held.values().map(|r| r.owner.as_str()));
        let mut tickets: Vec<u64> = s.waiting.iter().map(|w| w.ticket).collect();
        tickets.extend(s.held.keys());
        let count = owners.len();
        owners.sort();
        owners.dedup();
        tickets.sort();
        tickets.dedup();
        Ok(vec![
            check(
                "capacity_within_budget",
                s.used() <= u64::from(s.budget.units)
                    && s.background_used()
                        <= u64::from(s.budget.units - s.budget.interactive_reserve),
            ),
            check(
                "owner_has_one_reservation_or_request",
                owners.len() == count,
            ),
            check(
                "tickets_unique_and_issued",
                tickets.len() == count && tickets.iter().all(|t| (1..=s.last_ticket).contains(t)),
            ),
            check(
                "waiting_in_arrival_order",
                s.waiting.windows(2).all(|w| w[0].ticket < w[1].ticket),
            ),
            // A fitting waiter may only wait behind an earlier-ranked waiter that
            // can still be served by live capacity; never behind nothing, behind
            // lower-priority work, or behind stuck capacity only.
            check(
                "admission_progress",
                s.waiting.iter().all(|w| {
                    !s.fits(w.class, w.units)
                        || s.waiting
                            .iter()
                            .any(|b| rank(s, b) < rank(s, w) && legitimately_blocked(s, b))
                }),
            ),
        ])
    }
    fn check_transition(
        &self,
        before: &Ledger,
        input: &Input,
        next: &TransitionRef<'_, Ledger, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let mut released = Vec::new();
        let mut started = Vec::new();
        let mut terminated = Vec::new();
        let mut accepted = Vec::new();
        let mut rejected = 0;
        for effect in next.outputs {
            match effect {
                Effect::Released { ticket, units } => released.push((*ticket, *units)),
                Effect::Start { ticket } => started.push(*ticket),
                Effect::Terminate { ticket } => terminated.push(*ticket),
                Effect::Accepted { owner, ticket } => accepted.push((owner.clone(), *ticket)),
                Effect::Rejected { .. } => rejected += 1,
            }
        }
        let ticket = match input {
            Input::Cancel { ticket } | Input::Exited { ticket } | Input::Stuck { ticket } => {
                Some(*ticket)
            }
            Input::Request { .. } => None,
        };
        let known = ticket.is_some_and(|t| before.holds(t).is_some() || before.is_waiting(t));
        let expected_release: Vec<(u64, u32)> = match input {
            Input::Exited { ticket } => before
                .holds(*ticket)
                .map(|r| (*ticket, r.units))
                .into_iter()
                .collect(),
            _ => vec![],
        };
        let expected_terminate: Vec<u64> = match input {
            Input::Cancel { ticket }
                if before
                    .holds(*ticket)
                    .is_some_and(|r| r.hold == Hold::Running) =>
            {
                vec![*ticket]
            }
            _ => vec![],
        };
        let disposition = match input {
            Input::Request {
                owner,
                class,
                units,
            } => match before.ticket(owner) {
                Some(t) => {
                    let same = before.holds(t).map(|r| (r.class, r.units)).or_else(|| {
                        before
                            .waiting
                            .iter()
                            .find(|w| w.ticket == t)
                            .map(|w| (w.class, w.units))
                    }) == Some((*class, *units));
                    if same {
                        accepted == [(owner.clone(), t)] && rejected == 0 && after == before
                    } else {
                        accepted.is_empty() && rejected == 1 && after == before
                    }
                }
                None => {
                    let admissible = *units > 0 && *units <= before.budget.limit(*class);
                    if admissible {
                        accepted == [(owner.clone(), before.last_ticket + 1)]
                            && rejected == 0
                            && after.last_ticket == before.last_ticket + 1
                    } else {
                        accepted.is_empty() && rejected == 1 && after == before
                    }
                }
            },
            _ => accepted.is_empty() && rejected == 0 && after.last_ticket == before.last_ticket,
        };
        let units_of = |t: u64| {
            before
                .waiting
                .iter()
                .find(|w| w.ticket == t)
                .map(|w| (w.class, w.units))
                .or(match input {
                    Input::Request { class, units, .. } if t == before.last_ticket + 1 => {
                        Some((*class, *units))
                    }
                    _ => None,
                })
        };
        let started_units: Option<u64> = started
            .iter()
            .map(|t| units_of(*t).map(|(_, u)| u64::from(u)))
            .sum();
        let released_units: u64 = released.iter().map(|(_, u)| u64::from(*u)).sum();
        // Tickets are issued in arrival order: within a class, starts are ascending
        // and an earlier ticket of that class is left waiting only while it is
        // blocked by stuck capacity alone.
        let fifo = started.iter().enumerate().all(|(i, t)| {
            let Some((class, _)) = units_of(*t) else {
                return false;
            };
            started[..i]
                .iter()
                .all(|p| units_of(*p).is_none_or(|(c, _)| c != class || p < t))
                && after.waiting.iter().all(|w| {
                    w.class != class || w.ticket > *t || !fits_beside_stuck(after, w.class, w.units)
                })
        });
        let accepted_recorded = match input {
            Input::Request {
                owner: _,
                class,
                units,
            } => accepted.iter().all(|(o, t)| {
                let waiting = after
                    .waiting
                    .iter()
                    .filter(|w| w.ticket == *t)
                    .all(|w| &w.owner == o && w.class == *class && w.units == *units)
                    && after.waiting.iter().filter(|w| w.ticket == *t).count() <= 1;
                let held = after
                    .holds(*t)
                    .is_none_or(|r| &r.owner == o && r.class == *class && r.units == *units);
                waiting && held && (after.is_waiting(*t) != after.holds(*t).is_some())
            }),
            _ => true,
        };
        // Replay the starts one at a time from the state immediately before the
        // first of them, checking precedence obligations before each start.
        let mut stepwise = after.clone();
        stepwise.last_background = before.last_background;
        for t in &started {
            if let Some(r) = stepwise.held.remove(t) {
                stepwise.waiting.push(Waiter {
                    ticket: *t,
                    owner: r.owner,
                    class: r.class,
                    units: r.units,
                });
            }
        }
        stepwise.waiting.sort_by_key(|w| w.ticket);
        let mut ordered_starts = true;
        for t in &started {
            let Some(index) = stepwise.waiting.iter().position(|w| w.ticket == *t) else {
                ordered_starts = false;
                break;
            };
            let chosen = stepwise.waiting[index].clone();
            // Nothing that ranks earlier may still be admissible or legitimately blocking.
            ordered_starts &= stepwise.fits(chosen.class, chosen.units)
                && stepwise.waiting.iter().all(|w| {
                    rank(&stepwise, w) >= rank(&stepwise, &chosen)
                        || !fits_beside_stuck(&stepwise, w.class, w.units)
                });
            stepwise.waiting.remove(index);
            if !chosen.class.interactive() {
                stepwise.last_background = Some(chosen.class);
            }
            stepwise.held.insert(
                chosen.ticket,
                Reservation {
                    owner: chosen.owner,
                    class: chosen.class,
                    units: chosen.units,
                    hold: Hold::Running,
                },
            );
        }
        let background_started: Vec<Class> = started
            .iter()
            .filter_map(|t| after.holds(*t).map(|r| r.class))
            .filter(|c| !c.interactive())
            .collect();
        let interactive_blocks = after
            .waiting
            .iter()
            .any(|w| w.class.interactive() && fits_beside_stuck(after, w.class, w.units));
        // Repeating the previous background class requires every other background
        // class head to be blocked by stuck capacity alone.
        let rotation = match background_started.as_slice() {
            [class] if before.last_background == Some(*class) => after.waiting.iter().all(|w| {
                w.class.interactive()
                    || w.class == *class
                    || !fits_beside_stuck(after, w.class, w.units)
            }),
            _ => true,
        };
        Ok(vec![
            check("request_disposition", disposition && accepted_recorded),
            check("starts_follow_precedence", ordered_starts),
            check(
                "release_exactly_on_owner_exit",
                released == expected_release,
            ),
            check(
                "terminate_exactly_once_per_running_owner",
                terminated == expected_terminate,
            ),
            check(
                "capacity_tracks_live_owner",
                started_units.is_some_and(|s| after.used() + released_units == before.used() + s),
            ),
            check(
                "start_only_from_request",
                started.iter().all(|t| {
                    before.holds(*t).is_none()
                        && after.holds(*t).is_some_and(|r| r.hold == Hold::Running)
                }),
            ),
            check("fifo_within_class", fifo),
            check(
                "interactive_waiter_blocks_background",
                !interactive_blocks || background_started.is_empty(),
            ),
            check("background_classes_rotate", rotation),
            check(
                "fenced_owner_never_resumes",
                before.held.iter().all(|(t, r)| {
                    r.hold == Hold::Running
                        || after.holds(*t).is_none_or(|n| n.hold != Hold::Running)
                }),
            ),
            check(
                "stuck_or_cancel_never_releases",
                !matches!(input, Input::Stuck { .. } | Input::Cancel { .. }) || released.is_empty(),
            ),
            check(
                "stale_or_unknown_ticket_is_noop",
                ticket.is_none() || known || (after == before && next.outputs.is_empty()),
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "work ledger invariant violated")
    }
}
fn encode<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
impl ModelCodec for Work {
    fn encode_state(&self, v: &Ledger) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<Ledger, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}

const SMALL: Work = Work {
    owners: &["a", "b", "c"],
    classes: &[
        Class::Interactive,
        Class::Preparation,
        Class::Inventory,
        Class::Maintenance,
    ],
    units: &[1, 2],
    max_tickets: 5,
};

#[test]
fn bounded_work_graph_checks_production_reducer() {
    let report = stateless::explore::enumerate(
        &SMALL,
        stateless::explore::SearchConfig {
            max_states: 2_000_000,
            max_transitions: 100_000_000,
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
        "Stateless work: {} states, {} edges; 3 owners, 4 classes, 1..2 units, <=5 tickets, budget 3 reserve 1",
        report.states, report.transitions
    );
}

#[test]
fn seeded_work_sequences_cover_wider_units_and_owners() {
    let report = stateless::explore::fuzz(
        &Work {
            owners: &["a", "b", "c", "d"],
            classes: &[
                Class::Interactive,
                Class::Preparation,
                Class::Inventory,
                Class::Maintenance,
            ],
            units: &[0, 1, 2, 3, 4],
            max_tickets: 40,
        },
        stateless::explore::FuzzConfig {
            seed: 20261009,
            cases: 2000,
            max_steps: 100,
            max_transitions: 200_000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert!(report.transitions > 100_000);
}

#[test]
fn stuck_then_exit_replays_exactly() {
    let owner = |o: &str| o.to_owned();
    let inputs = vec![
        Input::Request {
            owner: owner("a"),
            class: Class::Preparation,
            units: 2,
        },
        Input::Request {
            owner: owner("b"),
            class: Class::Preparation,
            units: 1,
        },
        Input::Cancel { ticket: 1 },
        Input::Stuck { ticket: 1 },
        Input::Exited { ticket: 1 },
        Input::Exited { ticket: 1 },
    ];
    let trace = stateless::execution::record(&SMALL, inputs, Default::default(), 100).unwrap();
    let replay = stateless::execution::replay(&SMALL, &trace, Default::default()).unwrap();
    assert_eq!(replay.outcome, stateless::execution::ReplayOutcome::Exact);
    assert_eq!(replay.steps_verified, 6);
}
