//! Stateless model of shared scan demands for one source, over the production
//! `demands` decisions and the production job reducer.
use playscale_core::{
    demands::{
        Admission, Attempt, Demand, DemandStatus, FollowUp, admit, can_satisfy, cancel, follow_up,
        resolve,
    },
    jobs::{Input as JobInput, Job, Phase, transition},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    barrier: u64,
    demands: Vec<Demand>,
    attempts: Vec<(Attempt, Job)>,
    next_job: u32,
    restarts: u8,
    directs: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Request {
        verify: bool,
    },
    /// A direct (v1/scheduled) request: joins any active attempt, otherwise
    /// enqueues one that no demand cancellation may stop.
    Direct,
    /// A filesystem change hint advances the barrier.
    MarkDirty,
    Start,
    Finish {
        outcome: Option<bool>,
    },
    CancelDemand(usize),
    /// Server restart: running attempts recover through the job reducer.
    Restart,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Admitted(Admission),
    Resolved(Vec<(String, DemandStatus)>),
    Stopped(Vec<String>),
    FollowedUp(FollowUp),
}

fn attempts(s: &State) -> Vec<Attempt> {
    s.attempts.iter().map(|(a, _)| a.clone()).collect()
}
fn apply_follow_up(next: &mut State, outputs: &mut Vec<Effect>) {
    let decision = follow_up(&next.demands, &attempts(next));
    match &decision {
        FollowUp::Nothing => return,
        FollowUp::Enqueue { verify } => enqueue(next, *verify),
        FollowUp::UpgradeQueued { job } => upgrade(next, job),
    }
    outputs.push(Effect::FollowedUp(decision));
}
fn enqueue(s: &mut State, verify: bool) {
    s.next_job += 1;
    s.attempts.push((
        Attempt {
            job: format!("j{}", s.next_job),
            phase: Phase::Queued,
            verify,
            started_barrier: None,
            direct: false,
        },
        Job::default(),
    ));
}
fn upgrade(s: &mut State, job: &str) {
    if let Some((a, _)) = s.attempts.iter_mut().find(|(a, _)| a.job == job) {
        a.verify = true;
    }
}
fn retire_finished(s: &mut State) {
    s.attempts
        .retain(|(a, _)| matches!(a.phase, Phase::Queued | Phase::Running | Phase::Cancelling));
}

struct Demands;
impl Model for Demands {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.scan_demands".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: concat!(
                include_str!("../src/demands.rs"),
                include_str!("../src/jobs.rs")
            )
            .into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            barrier: 0,
            demands: vec![],
            attempts: vec![],
            next_job: 0,
            restarts: 0,
            directs: 0,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let mut outputs = Vec::new();
        match input {
            Input::Request { verify } => {
                next.barrier += 1;
                let demand = Demand {
                    id: format!("d{}", next.demands.len()),
                    barrier: next.barrier,
                    verify: *verify,
                    require_complete: false,
                    status: DemandStatus::Pending,
                };
                let admission = admit(&demand, &attempts(&next));
                match &admission {
                    Admission::Join { .. } => {}
                    Admission::UpgradeQueued { job } => upgrade(&mut next, job),
                    Admission::Enqueue => enqueue(&mut next, *verify),
                }
                next.demands.push(demand);
                outputs.push(Effect::Admitted(admission));
            }
            Input::Direct => {
                next.directs += 1;
                let any_active = next.attempts.iter().any(|(a, _)| {
                    matches!(a.phase, Phase::Queued | Phase::Running | Phase::Cancelling)
                });
                if any_active {
                    // The direct requester joins the first active attempt.
                    if let Some((a, _)) = next.attempts.iter_mut().find(|(a, _)| {
                        matches!(a.phase, Phase::Queued | Phase::Running | Phase::Cancelling)
                    }) {
                        a.direct = true;
                    }
                } else {
                    enqueue(&mut next, false);
                    next.attempts.last_mut().unwrap().0.direct = true;
                }
            }
            Input::MarkDirty => next.barrier += 1,
            Input::Start => {
                let running = next
                    .attempts
                    .iter()
                    .any(|(a, _)| matches!(a.phase, Phase::Running | Phase::Cancelling));
                if !running
                    && let Some((a, job)) = next
                        .attempts
                        .iter_mut()
                        .find(|(a, _)| a.phase == Phase::Queued)
                {
                    let (j, _) = transition(job, JobInput::Start);
                    *job = j;
                    a.phase = job.phase;
                    a.started_barrier = Some(next.barrier);
                }
            }
            Input::Finish { outcome } => {
                if let Some(i) = next
                    .attempts
                    .iter()
                    .position(|(a, _)| matches!(a.phase, Phase::Running | Phase::Cancelling))
                {
                    let (attempt, job) = next.attempts[i].clone();
                    let cancelling = attempt.phase == Phase::Cancelling;
                    let (j, _) = transition(
                        &job,
                        JobInput::Finished {
                            attempt: job.attempt,
                            success: outcome.is_some(),
                        },
                    );
                    next.attempts[i].1 = j.clone();
                    next.attempts[i].0.phase = j.phase;
                    // A cancelled attempt answers nothing: the demands it served
                    // were cancelled; any other pending demand gets a follow-up.
                    let resolved = if cancelling {
                        vec![]
                    } else {
                        resolve(&attempt, *outcome, &next.demands)
                    };
                    for (id, status) in &resolved {
                        if let Some(d) = next.demands.iter_mut().find(|d| &d.id == id) {
                            d.status = *status;
                        }
                    }
                    outputs.push(Effect::Resolved(resolved));
                    retire_finished(&mut next);
                    apply_follow_up(&mut next, &mut outputs);
                }
            }
            Input::CancelDemand(i) => {
                if let Some(demand) = next.demands.get(*i).cloned() {
                    let (status, stop) = cancel(&demand, &next.demands, &attempts(&next));
                    next.demands[*i].status = status;
                    for (a, job) in next
                        .attempts
                        .iter_mut()
                        .filter(|(a, _)| stop.contains(&a.job))
                    {
                        let (j, _) = transition(job, JobInput::Cancel);
                        *job = j;
                        a.phase = job.phase;
                    }
                    outputs.push(Effect::Stopped(stop));
                    retire_finished(&mut next);
                }
            }
            Input::Restart => {
                next.restarts += 1;
                for (a, job) in next.attempts.iter_mut() {
                    let (j, _) = transition(job, JobInput::Recover);
                    *job = j;
                    a.phase = job.phase;
                    if a.phase == Phase::Queued {
                        a.started_barrier = None;
                    }
                }
                retire_finished(&mut next);
                // Two queued attempts can result (recovered + follow-up); keep
                // the recovered one and merge modes.
                let queued: Vec<usize> = next
                    .attempts
                    .iter()
                    .enumerate()
                    .filter(|(_, (a, _))| a.phase == Phase::Queued)
                    .map(|(i, _)| i)
                    .collect();
                if queued.len() == 2 {
                    let verify = next.attempts[queued[1]].0.verify;
                    next.attempts[queued[0]].0.verify |= verify;
                    next.attempts.remove(queued[1]);
                }
                apply_follow_up(&mut next, &mut outputs);
            }
        }
        Ok(Transition::accepted(next, outputs))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let active = attempts(s);
        let running = active
            .iter()
            .filter(|a| matches!(a.phase, Phase::Running | Phase::Cancelling))
            .count();
        let queued = active.iter().filter(|a| a.phase == Phase::Queued).count();
        let served = s
            .demands
            .iter()
            .filter(|d| d.status == DemandStatus::Pending)
            .all(|d| {
                active
                    .iter()
                    .any(|a| a.phase != Phase::Cancelling && can_satisfy(a, d))
            });
        Ok(vec![
            check("scan.one_running_one_queued", running <= 1 && queued <= 1),
            check("scan.no_lost_demand", served),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let terminal_stable = before
            .demands
            .iter()
            .zip(&after.demands)
            .all(|(b, a)| b.status == DemandStatus::Pending || b.status == a.status);
        // A demand is answered only by an attempt that started at or after its
        // barrier with a strong-enough mode.
        let running = before
            .attempts
            .iter()
            .find(|(a, _)| a.phase == Phase::Running)
            .map(|(a, _)| a);
        let fresh = before.demands.iter().zip(&after.demands).all(|(b, a)| {
            b.status != DemandStatus::Pending
                || matches!(a.status, DemandStatus::Pending | DemandStatus::Cancelled)
                || (matches!(input, Input::Finish { .. })
                    && running.is_some_and(|r| {
                        r.started_barrier.is_some_and(|s| s >= b.barrier) && (r.verify || !b.verify)
                    }))
        });
        // Cancelling one demand stops only attempts no other pending demand needs.
        let isolated = match (input, next.outputs) {
            (Input::CancelDemand(i), [Effect::Stopped(stop)]) => stop.iter().all(|job| {
                let attempt = before.attempts.iter().find(|(a, _)| &a.job == job).unwrap();
                !attempt.0.direct
                    && !before.demands.iter().enumerate().any(|(k, d)| {
                        k != *i && d.status == DemandStatus::Pending && can_satisfy(&attempt.0, d)
                    })
            }),
            (Input::CancelDemand(_), []) => true,
            (Input::CancelDemand(_), _) => false,
            _ => true,
        };
        Ok(vec![
            check("scan.terminal_demands_stable", terminal_stable),
            check("scan.freshness_preserved", fresh),
            check("scan.cancel_isolated", isolated),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "scan demand invariant violated")
    }
}
impl Enumerate for Demands {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        // Bounds: at most 4 attempts ever created and one restart per path, so
        // job identities and attempt counters stay finite.
        let mut inputs: Vec<Input> = (0..s.demands.len()).map(Input::CancelDemand).collect();
        if s.next_job >= 4 {
            return Ok(inputs);
        }
        inputs.extend([
            Input::Start,
            Input::Finish {
                outcome: Some(true),
            },
            Input::Finish {
                outcome: Some(false),
            },
            Input::Finish { outcome: None },
        ]);
        if s.restarts < 1 {
            inputs.push(Input::Restart);
        }
        if s.barrier < 4 {
            inputs.push(Input::MarkDirty);
            if s.demands.len() < 3 {
                inputs.push(Input::Request { verify: false });
                inputs.push(Input::Request { verify: true });
            }
            if s.directs < 1 {
                inputs.push(Input::Direct);
            }
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Demands {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Demands {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
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
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}

#[test]
fn bounded_demand_graph_checks_production_decisions() {
    let report = stateless::explore::enumerate(
        &Demands,
        stateless::explore::SearchConfig {
            max_states: 400_000,
            max_transitions: 4_000_000,
            max_depth: 100,
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
        "Stateless: {} states, {} edges; <=3 demands, barrier <=4, <=4 attempts, <=1 restart, <=1 direct request; start/finish/cancel interleavings",
        report.states, report.transitions
    );
}

#[test]
fn a_running_scan_never_satisfies_a_newer_request() {
    let mut s = Demands.initial_state().unwrap();
    for input in [
        Input::Request { verify: false },
        Input::Start,
        Input::Request { verify: false },
        Input::Finish {
            outcome: Some(true),
        },
    ] {
        s = Demands.step(&s, &input).unwrap().state;
    }
    assert_eq!(s.demands[0].status, DemandStatus::Complete);
    assert_eq!(
        s.demands[1].status,
        DemandStatus::Pending,
        "needs a follow-up"
    );
    assert_eq!(s.attempts.len(), 1);
    assert_eq!(s.attempts[0].0.phase, Phase::Queued);
}

#[test]
fn cancelling_one_demand_keeps_the_scan_another_needs() {
    let mut s = Demands.initial_state().unwrap();
    for input in [
        Input::Request { verify: false },
        Input::Request { verify: false },
        Input::CancelDemand(0),
    ] {
        s = Demands.step(&s, &input).unwrap().state;
    }
    assert_eq!(s.attempts[0].0.phase, Phase::Queued, "still needed by d1");
    s = Demands.step(&s, &Input::CancelDemand(1)).unwrap().state;
    assert!(s.attempts.is_empty(), "no demand needs it any more");
}
