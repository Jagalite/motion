//! Bounded production-policy checks. The search driver models pages in one
//! snapshot/order group; the resume driver models persistence success/failure.
//! Real SQL/filesystem tests establish adapter pagination and durable ordering.
use playscale_core::timeline_viewing::{self as core, NextCandidate, NextTimeline, ResumePolicy};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct SearchState {
    offset: usize,
    done: Option<NextTimeline>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Page(usize);
struct Search {
    visible: Option<usize>,
}
impl Model for Search {
    type State = SearchState;
    type Input = Page;
    type Output = NextTimeline;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.next_timeline_pages".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/timeline_viewing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<SearchState, ModelError> {
        Ok(SearchState {
            offset: 0,
            done: None,
        })
    }
    fn step(
        &self,
        s: &SearchState,
        i: &Page,
    ) -> Result<Transition<SearchState, NextTimeline>, ModelError> {
        if s.done.is_some() {
            return Ok(Transition::accepted(s.clone(), vec![]));
        }
        let end = (s.offset + i.0).min(52);
        let candidates: Vec<_> = (s.offset..end)
            .map(|n| NextCandidate {
                timeline: n.to_string(),
                readable: self.visible == Some(n),
            })
            .collect();
        let decision = core::next_timeline(&candidates, end == 52);
        let done = match &decision {
            NextTimeline::NeedMore => None,
            _ => Some(decision.clone()),
        };
        Ok(Transition::accepted(
            SearchState { offset: end, done },
            vec![decision],
        ))
    }
    fn check_state(&self, s: &SearchState) -> Result<Vec<Check>, ModelError> {
        let correct = match &s.done {
            Some(NextTimeline::Found(id)) => self.visible.is_some_and(|n| *id == n.to_string()),
            Some(NextTimeline::Exhausted) => self.visible.is_none() && s.offset == 52,
            Some(NextTimeline::NeedMore) => false,
            None => s.offset < 52 && self.visible.is_none_or(|n| n >= s.offset),
        };
        Ok(vec![check(
            "search.exact_first_readable_or_complete_exhaustion",
            correct,
        )])
    }
    fn check_transition(
        &self,
        s: &SearchState,
        _: &Page,
        n: &TransitionRef<'_, SearchState, NextTimeline>,
    ) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "search.progress_or_terminal",
            if s.done.is_some() {
                n.state == s && n.outputs.is_empty()
            } else {
                n.state.offset > s.offset && n.outputs.len() == 1
            },
        )])
    }
}
impl Enumerate for Search {
    fn inputs(&self, _: &SearchState) -> Result<Vec<Page>, ModelError> {
        Ok(vec![Page(1), Page(49), Page(50), Page(51)])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct ResumeState {
    checkpoint: u64,
    event: Option<(u64, bool)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ResumeInput {
    Observe(u64, bool),
    Compact(bool),
    Rebase(u64),
}
struct Resume;
fn open(s: &ResumeState) -> u64 {
    let (position, ended) = s.event.unwrap_or((s.checkpoint, false));
    core::resume_position(ResumePolicy::Offline, Some(position), ended, Some(10_000))
}
impl Model for Resume {
    type State = ResumeState;
    type Input = ResumeInput;
    type Output = u64;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.offline_resume_compaction".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/timeline_viewing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<ResumeState, ModelError> {
        Ok(ResumeState {
            checkpoint: 0,
            event: None,
        })
    }
    fn step(
        &self,
        s: &ResumeState,
        i: &ResumeInput,
    ) -> Result<Transition<ResumeState, u64>, ModelError> {
        let mut next = s.clone();
        match i {
            ResumeInput::Observe(position, ended) => next.event = Some((*position, *ended)),
            ResumeInput::Compact(true) => {
                // The manifest checkpoint must persist before discarding the
                // event. Failure leaves both durable inputs intact.
                next.checkpoint = open(s);
                next.event = None;
            }
            ResumeInput::Compact(false) => (),
            ResumeInput::Rebase(position) => {
                next.checkpoint = core::resume_position(
                    ResumePolicy::Offline,
                    Some(*position),
                    false,
                    Some(10_000),
                )
            }
        }
        let position = open(&next);
        Ok(Transition::accepted(next, vec![position]))
    }
    fn check_state(&self, s: &ResumeState) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "resume.valid_checkpoint",
            s.checkpoint <= 10_000,
        )])
    }
    fn check_transition(
        &self,
        s: &ResumeState,
        i: &ResumeInput,
        n: &TransitionRef<'_, ResumeState, u64>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match i {
            ResumeInput::Observe(position, ended) => {
                if *ended {
                    0
                } else {
                    *position
                }
            }
            ResumeInput::Compact(_) => open(s),
            ResumeInput::Rebase(position) => s.event.map_or(
                if *position <= 10_000 { *position } else { 0 },
                |(p, ended)| if ended { 0 } else { p },
            ),
        };
        Ok(vec![
            check(
                "resume.compaction_rewind_end_equivalence",
                n.outputs == [expected],
            ),
            check(
                "resume.failed_checkpoint_does_not_discard",
                !matches!(i, ResumeInput::Compact(false)) || n.state == s,
            ),
        ])
    }
}
impl Enumerate for Resume {
    fn inputs(&self, _: &ResumeState) -> Result<Vec<ResumeInput>, ModelError> {
        let mut inputs = vec![ResumeInput::Compact(false), ResumeInput::Compact(true)];
        for position in [0, 1, 9_000, 9_999, 10_000] {
            for ended in [false, true] {
                inputs.push(ResumeInput::Observe(position, ended));
            }
            inputs.push(ResumeInput::Rebase(position));
        }
        inputs.push(ResumeInput::Rebase(10_001));
        Ok(inputs)
    }
}
fn check(name: &'static str, correct: bool) -> Check {
    if correct {
        Check::passed(name)
    } else {
        Check::failed(name, "production-policy mismatch")
    }
}
#[test]
fn next_timeline_page_boundaries_and_hidden_exhaustion() {
    for visible in [None, Some(0), Some(48), Some(49), Some(50), Some(51)] {
        let report = stateless::explore::enumerate(
            &Search { visible },
            stateless::explore::SearchConfig {
                max_states: 100,
                max_transitions: 1000,
                max_depth: 60,
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
            "search {visible:?}: {} states / {} transitions",
            report.states, report.transitions
        );
    }
}
#[test]
fn offline_resume_is_unchanged_by_successful_or_failed_compaction() {
    let report = stateless::explore::enumerate(
        &Resume,
        stateless::explore::SearchConfig {
            max_states: 1000,
            max_transitions: 20_000,
            max_depth: 20,
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
        "resume: {} states / {} transitions",
        report.states, report.transitions
    );
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DeliveryInput {
    status: playscale_core::delivery::Status,
    foreign_owner: bool,
    file_present: bool,
    context: u8,
}
struct DeliveryAdmission;
impl Model for DeliveryAdmission {
    type State = ();
    type Input = DeliveryInput;
    type Output = Result<(), playscale_core::playback_session::ViewingAdmissionError>;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.viewing_delivery_context".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/playback_session.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<(), ModelError> {
        Ok(())
    }
    fn step(&self, _: &(), i: &DeliveryInput) -> Result<Transition<(), Self::Output>, ModelError> {
        use playscale_core::{
            access::{FileFacts, Principal},
            playback_session::{self as p, Owner, ViewingDelivery},
        };
        let principal = Principal::operator();
        let file = FileFacts {
            library_ids: ["library".into()].into(),
            revision: "revision".into(),
        };
        let delivery = ViewingDelivery {
            owner: Owner {
                principal: if i.foreign_owner {
                    "foreign".into()
                } else {
                    principal.id.clone()
                },
                profile: "profile".into(),
                timeline: "timeline".into(),
                file: "file".into(),
                file_revision: "revision".into(),
            },
            status: i.status,
            duration_ms: Some(10_000),
        };
        let existing = match i.context {
            0 => None,
            1 => Some(("profile", "timeline")),
            2 => Some(("other", "timeline")),
            _ => Some(("profile", "other")),
        };
        Ok(Transition::accepted(
            (),
            vec![p::admit_viewing_delivery(
                &delivery,
                &principal,
                i.file_present.then_some(&file),
                existing,
            )],
        ))
    }
    fn check_state(&self, _: &()) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &(),
        i: &DeliveryInput,
        n: &TransitionRef<'_, (), Self::Output>,
    ) -> Result<Vec<Check>, ModelError> {
        use playscale_core::{delivery::Status, playback_session::ViewingAdmissionError};
        let expected = if !matches!(
            i.status,
            Status::Starting | Status::Ready | Status::Transitioning
        ) || i.foreign_owner
            || !i.file_present
        {
            Err(ViewingAdmissionError::Unavailable)
        } else if i.context >= 2 {
            Err(ViewingAdmissionError::DifferentContext)
        } else {
            Ok(())
        };
        Ok(vec![check(
            "viewing_delivery.owner_phase_and_context",
            n.outputs == [expected],
        )])
    }
}
impl Enumerate for DeliveryAdmission {
    fn inputs(&self, _: &()) -> Result<Vec<DeliveryInput>, ModelError> {
        use playscale_core::delivery::Status;
        let mut out = vec![];
        for status in [
            Status::Starting,
            Status::Ready,
            Status::Transitioning,
            Status::Closing,
            Status::Closed,
            Status::Failed,
            Status::Interrupted,
        ] {
            for foreign_owner in [false, true] {
                for file_present in [false, true] {
                    for context in 0..4 {
                        out.push(DeliveryInput {
                            status,
                            foreign_owner,
                            file_present,
                            context,
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}
#[test]
fn viewing_delivery_admission_checks_phase_owner_and_rebind_context() {
    let report = stateless::explore::enumerate(
        &DeliveryAdmission,
        stateless::explore::SearchConfig {
            max_states: 2,
            max_transitions: 200,
            max_depth: 2,
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
        "delivery admission: {} states / {} transitions",
        report.states, report.transitions
    );
}
