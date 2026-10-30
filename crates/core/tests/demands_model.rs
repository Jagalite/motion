//! Stateless model of shared scan demands for one source, over the production
//! `demands` decisions and the production job reducer.
use playscale_core::{
    demands::{
        Admission, Attempt, Demand, DemandStatus, FollowUp, RecoveryEffect, admit, can_satisfy,
        cancel, follow_up, recover, resolve,
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
    /// Server restart: recover identities and ownership through the core plan.
    Restart,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Admitted(Admission),
    Resolved(Vec<(String, DemandStatus)>),
    Stopped(Vec<String>),
    FollowedUp(FollowUp),
    Recovered(Vec<RecoveryEffect>),
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
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
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
                let effects = recover(&next.attempts);
                for effect in &effects {
                    match effect {
                        RecoveryEffect::MergeQueued {
                            job,
                            verify,
                            direct,
                        } => {
                            let (a, _) = next
                                .attempts
                                .iter_mut()
                                .find(|(a, _)| &a.job == job)
                                .unwrap();
                            a.verify = *verify;
                            a.direct = *direct;
                        }
                        RecoveryEffect::ResetInterrupted { job, phase } => {
                            let (a, j) = next
                                .attempts
                                .iter_mut()
                                .find(|(a, _)| &a.job == job)
                                .unwrap();
                            a.phase = *phase;
                            a.started_barrier = None;
                            j.phase = *phase;
                        }
                    }
                }
                outputs.push(Effect::Recovered(effects));
                retire_finished(&mut next);
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
            check(
                "scan.phase_observations_agree",
                s.attempts.iter().all(|(a, j)| a.phase == j.phase),
            ),
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
        let mut recovery_exact = true;
        let mut recovery_preserves_work = true;
        if matches!(input, Input::Restart) {
            let queued = before
                .attempts
                .iter()
                .find(|(a, _)| a.phase == Phase::Queued);
            let interrupted = before
                .attempts
                .iter()
                .find(|(a, _)| matches!(a.phase, Phase::Running | Phase::Cancelling));
            let mut expected = Vec::new();
            if let Some(old_attempt) = interrupted {
                let (old, old_job) = old_attempt;
                let retryable = old.phase == Phase::Running && old_job.attempt < u32::MAX;
                let phase = if old.phase == Phase::Cancelling {
                    Phase::Cancelled
                } else if !retryable {
                    Phase::Failed
                } else if queued.is_some() {
                    Phase::Cancelled
                } else {
                    Phase::Queued
                };
                if retryable {
                    let (survivor, counter) = queued.unwrap_or(old_attempt);
                    recovery_preserves_work = after.attempts.iter().any(|(a, j)| {
                        a.job == survivor.job
                            && a.phase == Phase::Queued
                            && j.attempt == counter.attempt
                            && a.started_barrier.is_none()
                            && a.direct == (survivor.direct || old.direct)
                            && (!old.verify || a.verify)
                            && (!survivor.verify || a.verify)
                    });
                    if let Some((q, _)) = queued {
                        expected.push(RecoveryEffect::MergeQueued {
                            job: q.job.clone(),
                            verify: q.verify || old.verify,
                            direct: q.direct || old.direct,
                        });
                    }
                }
                if phase != Phase::Queued {
                    recovery_preserves_work &=
                        !after.attempts.iter().any(|(a, _)| a.job == old.job);
                }
                expected.push(RecoveryEffect::ResetInterrupted {
                    job: old.job.clone(),
                    phase,
                });
            }
            recovery_exact = matches!(next.outputs,
                [Effect::Recovered(actual)] | [Effect::Recovered(actual), Effect::FollowedUp(_)]
                    if *actual == expected)
                && after.barrier == before.barrier
                && after.demands == before.demands;
        }
        Ok(vec![
            check("scan.terminal_demands_stable", terminal_stable),
            check("scan.freshness_preserved", fresh),
            check("scan.cancel_isolated", isolated),
            check("scan.recovery_exact_effects", recovery_exact),
            check(
                "scan.recovery_preserves_identity_mode_and_direct_owner",
                recovery_preserves_work,
            ),
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

#[test]
fn restart_keeps_the_follow_up_identity_and_direct_ownership() {
    let mut s = Demands.initial_state().unwrap();
    for input in [
        Input::Request { verify: true },
        Input::Direct,
        Input::Start,
        Input::Request { verify: false },
    ] {
        s = Demands.step(&s, &input).unwrap().state;
    }
    let restarted = Demands.step(&s, &Input::Restart).unwrap();
    assert_eq!(
        restarted.outputs,
        vec![Effect::Recovered(vec![
            RecoveryEffect::MergeQueued {
                job: "j2".into(),
                verify: true,
                direct: true
            },
            RecoveryEffect::ResetInterrupted {
                job: "j1".into(),
                phase: Phase::Cancelled
            },
        ])]
    );
    s = restarted.state;
    assert_eq!(
        s.attempts,
        vec![(
            Attempt {
                job: "j2".into(),
                phase: Phase::Queued,
                verify: true,
                started_barrier: None,
                direct: true,
            },
            Job::default()
        )]
    );
    let duplicate = Demands.step(&s, &Input::Restart).unwrap();
    assert_eq!(duplicate.state.attempts, s.attempts);
    assert_eq!(duplicate.outputs, vec![Effect::Recovered(vec![])]);
    for i in 0..2 {
        s = Demands.step(&s, &Input::CancelDemand(i)).unwrap().state;
    }
    assert_eq!(s.attempts.len(), 1, "direct requester still owns j2");
    assert_eq!(s.attempts[0].0.job, "j2");
}
