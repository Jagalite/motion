//! Transaction interleavings exercise the production batch planner. SQL owns
//! atomic commit/rollback and the revision-reset trigger; adapter tests exercise
//! those effects against SQLite.
use playscale_core::maintenance::{CursorUpdate, DispatchBatch, DispatchCandidate};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    revision: i64,
    cursor: Option<String>,
    jobs: Vec<String>,
    pending: Option<(i64, DispatchBatch)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Begin(bool),
    Candidate(&'static str, DispatchCandidate),
    Commit,
    Crash,
    Revise,
}
struct Dispatch;
impl Model for Dispatch {
    type State = State;
    type Input = Input;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.schedule.dispatch".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/maintenance.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            revision: 0,
            cursor: None,
            jobs: vec![],
            pending: None,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, String>, ModelError> {
        let mut n = s.clone();
        let mut effects = vec![];
        match i {
            Input::Begin(more) if n.pending.is_none() => {
                n.pending = Some((n.revision, DispatchBatch::new(*more)))
            }
            Input::Candidate(id, observation) => {
                if let Some((_, batch)) = &mut n.pending
                    && batch.last.as_deref().is_none_or(|last| last < *id)
                {
                    let observed = if n.jobs.iter().any(|j| j == id) {
                        DispatchCandidate::Skip
                    } else {
                        *observation
                    };
                    batch.candidate(id, observed);
                }
            }
            Input::Commit => {
                if let Some((revision, batch)) = n.pending.take()
                    && let Some(cursor) = batch.cursor(revision, n.revision)
                {
                    effects = batch.queued;
                    n.jobs.extend(effects.clone());
                    n.jobs.sort();
                    match cursor {
                        CursorUpdate::Preserve => {}
                        CursorUpdate::Clear => n.cursor = None,
                        CursorUpdate::Advance(id) => n.cursor = Some(id),
                    }
                }
            }
            Input::Crash => n.pending = None,
            Input::Revise if n.revision == 0 => {
                n.revision = 1;
                n.cursor = None;
            }
            _ => {}
        }
        Ok(Transition::accepted(n, effects))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let mut jobs = s.jobs.clone();
        jobs.dedup();
        Ok(vec![check("no_duplicate_jobs", jobs == s.jobs)])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let committed =
            matches!(i, Input::Commit) && s.pending.as_ref().is_some_and(|(r, _)| *r == s.revision);
        let expected = if committed {
            s.pending.as_ref().unwrap().1.queued.clone()
        } else {
            vec![]
        };
        let mut checks = vec![check("exact_atomic_job_effects", n.outputs == expected)];
        let expected_cursor = if committed {
            let batch = &s.pending.as_ref().unwrap().1;
            if !batch.more {
                None
            } else {
                batch.last.clone().or_else(|| s.cursor.clone())
            }
        } else if matches!(i, Input::Revise) && s.revision == 0 {
            None
        } else {
            s.cursor.clone()
        };
        checks.push(check(
            "exact_atomic_cursor",
            n.state.cursor == expected_cursor,
        ));
        if !committed {
            checks.push(check("no_uncommitted_jobs", n.state.jobs == s.jobs));
        }
        if let Input::Candidate(id, observed) = i
            && let (Some((_, before)), Some((_, after))) = (&s.pending, &n.state.pending)
            && !before.blocked
            && before.last.as_deref().is_none_or(|last| last < *id)
            && !s.jobs.iter().any(|j| j == id)
        {
            let mut expected_jobs = before.queued.clone();
            if *observed == DispatchCandidate::Queue {
                expected_jobs.push((*id).into());
            }
            checks.push(check(
                "candidate_effect_identity",
                after.queued == expected_jobs,
            ));
            if *observed == DispatchCandidate::Full {
                checks.push(check(
                    "full_does_not_skip",
                    after.last == before.last && after.more && after.blocked,
                ));
            }
        }
        Ok(checks)
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "dispatch mismatch")
    }
}
impl Enumerate for Dispatch {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![
            Input::Begin(false),
            Input::Begin(true),
            Input::Commit,
            Input::Crash,
            Input::Revise,
        ];
        for id in ["a", "b"] {
            for observation in [
                DispatchCandidate::Queue,
                DispatchCandidate::Skip,
                DispatchCandidate::Full,
            ] {
                v.push(Input::Candidate(id, observation));
            }
        }
        Ok(v)
    }
}
#[test]
fn revision_crash_queue_pressure_and_exact_jobs() {
    let r = stateless::explore::enumerate(
        &Dispatch,
        stateless::explore::SearchConfig {
            max_states: 20000,
            max_transitions: 300000,
            max_depth: 100,
        },
    )
    .unwrap();
    assert!(r.failure.is_none(), "{:?}", r.failure);
    assert_eq!(r.skipped_checks, 0);
    assert_eq!(
        r.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!("{} states / {} transitions", r.states, r.transitions);
}
