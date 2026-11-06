//! Bounded production backup + local-ticket transition interleavings.
use playscale_core::{
    maintenance::{BackupAvailability as B, BackupRetentionInput as I},
    offline::LocalPlayback as P,
};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    backup: B,
    playback: P,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Expire,
    Failed,
    Removed,
    Open(&'static str),
    Snapshot(&'static str, &'static str, bool),
    Close(&'static str),
}
struct Lifecycle;
impl Model for Lifecycle {
    type State = State;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.offline.maintenance".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/offline.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            backup: B::Ready,
            playback: P::Closed,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, bool>, ModelError> {
        let mut n = s.clone();
        match i {
            Input::Expire => n.backup = s.backup.transition(I::Expire),
            Input::Failed => n.backup = s.backup.transition(I::RemovalFailed),
            Input::Removed => n.backup = s.backup.transition(I::DurableAbsence),
            Input::Open(d) => n.playback = s.playback.begin(d),
            Input::Snapshot(d, ticket, ok) => n.playback = s.playback.snapshot(d, ticket, *ok),
            Input::Close(t) => n.playback = s.playback.retire(t),
        }
        let ready = n.backup == B::Ready;
        Ok(Transition::accepted(n, vec![ready]))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let backup = match (s.backup, i) {
            (B::Ready, Input::Expire) => B::Withdrawn,
            (B::Withdrawn, Input::Removed) => B::Reclaimed,
            _ => s.backup,
        };
        let playback = match i {
            Input::Open(d) => P::Opening((*d).into()),
            Input::Snapshot(d, ticket, ok) if s.playback == P::Opening((*d).into()) => {
                if *ok {
                    P::Open {
                        download: (*d).into(),
                        ticket: (*ticket).into(),
                    }
                } else {
                    P::Closed
                }
            }
            Input::Close(t) if matches!(&s.playback,P::Open{ticket,..} if ticket==t) => P::Closed,
            _ => s.playback.clone(),
        };
        let good = n.state.backup == backup
            && n.state.playback == playback
            && n.outputs == [backup == B::Ready];
        Ok(vec![if good {
            Check::passed("exact_effects")
        } else {
            Check::failed("exact_effects", "stale ticket or premature reclaim")
        }])
    }
}
impl Enumerate for Lifecycle {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Expire,
            Input::Failed,
            Input::Removed,
            Input::Open("a"),
            Input::Open("b"),
            Input::Snapshot("a", "old", true),
            Input::Snapshot("a", "new", true),
            Input::Snapshot("a", "old", false),
            Input::Snapshot("b", "old", true),
            Input::Snapshot("b", "new", true),
            Input::Snapshot("b", "new", false),
            Input::Close("old"),
            Input::Close("new"),
        ])
    }
}
#[test]
fn all_bounded_interleavings() {
    let r = stateless::explore::enumerate(
        &Lifecycle,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 2000,
            max_depth: 20,
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

#[test]
fn same_download_reopen_rejects_old_close() {
    let model = Lifecycle;
    let mut state = model.initial_state().unwrap();
    for input in [
        Input::Open("a"),
        Input::Snapshot("a", "old", true),
        Input::Open("a"),
        Input::Snapshot("a", "new", true),
        Input::Close("old"),
    ] {
        let next = model.step(&state, &input).unwrap();
        assert!(
            model
                .check_transition(&state, &input, &next.as_ref())
                .unwrap()
                .iter()
                .all(|c| matches!(c.status, stateless::CheckStatus::Passed))
        );
        state = next.state;
    }
    assert!(state.playback.admits("new"));
    assert!(!state.playback.admits("old"));
}
