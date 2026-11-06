//! Same production import/retraction decisions under duplicate repair, manual
//! reset, session authority and rollback. One timeline, one canonical action.
use playscale_core::{
    identity::Attribution,
    timeline_viewing::{self as core, View},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    view: Option<View>,
    changed: bool,
    session: bool,
    timestamp_changed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Repair,
    RollbackRepair,
    Manual,
    Session,
    TimestampOnly,
}
struct Repair {
    exact: bool,
    backfill: bool,
}
fn legacy() -> View {
    View {
        position_ms: 20000,
        ..View::default()
    }
}
impl Repair {
    fn attribution(&self) -> Attribution {
        if self.exact {
            Attribution::Exact {
                timeline: "t".into(),
            }
        } else {
            Attribution::Orphaned
        }
    }
    fn repair(&self, s: &State) -> State {
        let mut next = s.clone();
        if let Some(view) = &s.view
            && core::retract_unattributed_legacy(
                &self.attribution(),
                view,
                &legacy(),
                s.session,
                i64::from(s.timestamp_changed),
                0,
            )
        {
            next.view = None;
        }
        if let Some(view) =
            core::import_legacy(&self.attribution(), next.view.as_ref(), 20000, false, None)
        {
            next.view = Some(view);
        }
        next
    }
}
impl Model for Repair {
    type State = State;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.legacy-import".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: include_str!("../src/timeline_viewing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            view: self.backfill.then(legacy),
            changed: false,
            session: false,
            timestamp_changed: false,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, bool>, ModelError> {
        let mut n = s.clone();
        match i {
            Input::Repair => n = self.repair(s),
            Input::RollbackRepair => {
                let _uncommitted = self.repair(s);
            }
            Input::Manual => {
                n.view = Some(View {
                    revision: 1,
                    manual_epoch: 1,
                    manual_watched: Some(false),
                    ..View::default()
                });
                n.changed = true;
            }
            Input::Session => {
                n.view = Some(View {
                    revision: 1,
                    session: Some("session".into()),
                    ..View::default()
                });
                n.changed = true;
                n.session = true;
            }
            Input::TimestampOnly => n.timestamp_changed = true,
        }
        Ok(Transition::accepted(n, vec![true]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let canonical = !s.changed || s.view.as_ref().is_some_and(|v| v.revision > 0);
        Ok(vec![if canonical {
            Check::passed("canonical_action_retains_revision")
        } else {
            Check::failed(
                "canonical_action_retains_revision",
                "canonical authority was lost",
            )
        }])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        t: &TransitionRef<'_, State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let ok = match i {
            Input::RollbackRepair => t.state == s,
            Input::Repair => {
                if s.changed || (s.timestamp_changed && s.view.is_some()) {
                    t.state == s
                } else if self.exact {
                    t.state.view == Some(legacy())
                } else {
                    t.state.view.is_none()
                }
            }
            _ => true,
        };
        Ok(vec![if ok {
            Check::passed("repair_preserves_authority_or_imports_only_exact")
        } else {
            Check::failed(
                "repair_preserves_authority_or_imports_only_exact",
                "invalid publication or lost canonical action",
            )
        }])
    }
}
impl Enumerate for Repair {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![Input::Repair, Input::RollbackRepair];
        if !s.changed {
            v.extend([Input::Manual, Input::Session]);
        }
        if s.view.is_some() && !s.timestamp_changed {
            v.push(Input::TimestampOnly);
        }
        Ok(v)
    }
}
impl ModelCodec for Repair {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<State, ModelError> {
        decode(v)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
        decode(v)
    }
    fn encode_output(&self, v: &bool) -> Result<Vec<u8>, ModelError> {
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
fn legacy_repair_interleavings() {
    for exact in [false, true] {
        for backfill in [false, true] {
            let r = stateless::explore::enumerate(
                &Repair { exact, backfill },
                stateless::explore::SearchConfig {
                    max_states: 100,
                    max_transitions: 1000,
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
            println!(
                "exact={exact} backfill={backfill}: {} states, {} transitions",
                r.states, r.transitions
            );
        }
    }
}

mod preparation {
    use super::*;
    use playscale_core::{jobs, processing};
    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct State {
        processing: processing::State,
        facts: processing::PreparationFacts,
        source_current: bool,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Input {
        Start,
        Cancel,
        Retry,
        Recover,
        Revoke,
        Disable,
        Hide,
        Rebind,
        RecipeChanged,
        SourceChanged,
        Finish,
        StaleFinish,
        RollbackFinish,
    }
    struct Preparation;
    impl Model for Preparation {
        type State = State;
        type Input = Input;
        type Output = u8;
        fn metadata(&self) -> ModelMetadata {
            ModelMetadata {
                name: "motion.preparation-publication".into(),
                model_version: 1,
                properties_version: 1,
                codec_version: 1,
                build: include_str!("../src/processing.rs").into(),
            }
        }
        fn initial_state(&self) -> Result<State, ModelError> {
            Ok(State {
                processing: processing::State {
                    job: jobs::Job {
                        phase: jobs::Phase::Queued,
                        attempt: 0,
                    },
                    source_revision: "source".into(),
                    published_output: None,
                },
                facts: processing::PreparationFacts {
                    requester_authorized: true,
                    source_enabled: true,
                    source_in_scope: true,
                    binding: processing::BindingEvidence {
                        timeline: "timeline".into(),
                        file: "file".into(),
                        revision: "source".into(),
                        candidates: vec![processing::WholeBinding {
                            version: "version".into(),
                            timeline: "timeline".into(),
                            file: "file".into(),
                            file_revision: "source".into(),
                            origin: "original".into(),
                            file_edition: "edition".into(),
                            timeline_edition: "edition".into(),
                            part: 1,
                            start_ms: None,
                            end_ms: None,
                            part_count: 1,
                        }],
                    },
                    recipe_unchanged: true,
                },
                source_current: true,
            })
        }
        fn step(&self, s: &State, i: &Input) -> Result<Transition<State, u8>, ModelError> {
            let mut n = s.clone();
            let mut publications = 0;
            match i {
                Input::Revoke => n.facts.requester_authorized = false,
                Input::Disable => n.facts.source_enabled = false,
                Input::Hide => n.facts.source_in_scope = false,
                Input::Rebind => n.facts.binding.candidates.clear(),
                Input::RecipeChanged => n.facts.recipe_unchanged = false,
                Input::SourceChanged => n.source_current = false,
                Input::Start | Input::Cancel | Input::Retry | Input::Recover => {
                    let input = match i {
                        Input::Start => jobs::Input::Start,
                        Input::Cancel => jobs::Input::Cancel,
                        Input::Retry => jobs::Input::Retry,
                        _ => jobs::Input::Recover,
                    };
                    n.processing.job = jobs::transition(&s.processing.job, input).0;
                }
                Input::Finish | Input::StaleFinish | Input::RollbackFinish => {
                    let result = processing::Completion {
                        attempt: if matches!(i, Input::StaleFinish) {
                            s.processing.job.attempt.saturating_sub(1)
                        } else {
                            s.processing.job.attempt
                        },
                        current_source_revision: s.source_current.then(|| "source".into()),
                        validated_output: Some("output".into()),
                    };
                    let (next, effects) =
                        processing::finish_preparation(&s.processing, &result, Some(&s.facts));
                    if !matches!(i, Input::RollbackFinish) {
                        n.processing = next;
                        publications = effects
                            .iter()
                            .filter(|e| matches!(e, jobs::Effect::Publish { .. }))
                            .count() as u8;
                    }
                }
            }
            Ok(Transition::accepted(n, vec![publications]))
        }
        fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
            Ok(vec![if s.processing.published_output.is_none()
                || s.processing.job.phase == jobs::Phase::Completed
            {
                Check::passed("published_output_is_terminal")
            } else {
                Check::failed(
                    "published_output_is_terminal",
                    "output published before terminal acknowledgement",
                )
            }])
        }
        fn check_transition(
            &self,
            s: &State,
            i: &Input,
            t: &TransitionRef<'_, State, u8>,
        ) -> Result<Vec<Check>, ModelError> {
            let expected = matches!(i, Input::Finish)
                && s.processing.published_output.is_none()
                && s.processing.job.phase == jobs::Phase::Running
                && s.source_current
                && s.facts.requester_authorized
                && s.facts.source_enabled
                && s.facts.source_in_scope
                && s.facts.binding.is_current()
                && s.facts.recipe_unchanged;
            let atomic = !matches!(i, Input::RollbackFinish) || t.state == s;
            let ok = t.outputs == [u8::from(expected)] && atomic;
            Ok(vec![if ok {
                Check::passed("exact_once_current_authorized_publication_and_rollback")
            } else {
                Check::failed(
                    "exact_once_current_authorized_publication_and_rollback",
                    "wrong publication identity/count or partial commit",
                )
            }])
        }
    }
    impl Enumerate for Preparation {
        fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
            let mut inputs = vec![
                Input::Cancel,
                Input::Recover,
                Input::Revoke,
                Input::Disable,
                Input::Hide,
                Input::Rebind,
                Input::RecipeChanged,
                Input::SourceChanged,
                Input::Finish,
                Input::StaleFinish,
                Input::RollbackFinish,
            ];
            if s.processing.job.attempt < 2 {
                inputs.extend([Input::Start, Input::Retry]);
            }
            Ok(inputs)
        }
    }
    impl ModelCodec for Preparation {
        fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
            encode(v)
        }
        fn decode_state(&self, v: &[u8]) -> Result<State, ModelError> {
            decode(v)
        }
        fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
            encode(v)
        }
        fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
            decode(v)
        }
        fn encode_output(&self, v: &u8) -> Result<Vec<u8>, ModelError> {
            encode(v)
        }
    }
    #[test]
    fn preparation_interleavings() {
        let report = stateless::explore::enumerate(
            &Preparation,
            stateless::explore::SearchConfig {
                max_states: 10000,
                max_transitions: 100000,
                max_depth: 30,
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
            "preparation: {} states, {} transitions",
            report.states, report.transitions
        );
    }
}
