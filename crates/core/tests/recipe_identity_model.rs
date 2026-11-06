//! Real encoder-input changes fence completion of an already admitted recipe.
use playscale_core::{
    jobs::{Effect, Job, Phase},
    processing::{self as core, BindingEvidence, WholeBinding},
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    current: u8,
    job: core::State,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Change(u8),
    Complete { attempt: u32, commit: bool },
    Cancel,
}
struct Identity;
fn identity(change: u8) -> String {
    let mut profile = serde_json::json!({"version":1,"codec":"h264","max_width":1280,"max_height":720,"video_bitrate":2_000_000,"audio_bitrate":128_000,"frame_rate":null});
    match change {
        1 => profile["codec"] = serde_json::json!("hevc"),
        2 => profile["max_width"] = serde_json::json!(1920),
        3 => profile["max_height"] = serde_json::json!(1080),
        4 => profile["video_bitrate"] = serde_json::json!(4_000_000),
        5 => profile["audio_bitrate"] = serde_json::json!(192_000),
        6 => profile["frame_rate"] = serde_json::json!({"numerator":24,"denominator":1}),
        _ => {}
    }
    core::recipe_identity(
        "video_profile",
        if change == 9 {
            "new-semantics"
        } else {
            "base-semantics"
        },
        if change == 7 {
            "videotoolbox"
        } else {
            "software"
        },
        Some(profile),
        change == 8,
    )
    .unwrap()
}
impl Model for Identity {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.recipe_identity_publication".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/processing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            current: 0,
            job: core::State {
                job: Job {
                    phase: Phase::Running,
                    attempt: 2,
                },
                source_revision: "r".into(),
                published_output: None,
            },
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut n = s.clone();
        let mut effects = vec![];
        match i {
            Input::Change(value) => n.current = *value,
            Input::Cancel => {
                n.job.job = playscale_core::jobs::transition(
                    &n.job.job,
                    playscale_core::jobs::Input::Cancel,
                )
                .0
            }
            Input::Complete { attempt, commit } => {
                let facts = core::PreparationFacts {
                    requester_authorized: true,
                    source_enabled: true,
                    source_in_scope: true,
                    recipe_unchanged: identity(n.current) == identity(0),
                    binding: BindingEvidence {
                        timeline: "t".into(),
                        file: "f".into(),
                        revision: "r".into(),
                        candidates: vec![WholeBinding {
                            version: "v".into(),
                            timeline: "t".into(),
                            file: "f".into(),
                            file_revision: "r".into(),
                            origin: "original".into(),
                            file_edition: "e".into(),
                            timeline_edition: "e".into(),
                            part: 1,
                            start_ms: None,
                            end_ms: None,
                            part_count: 1,
                        }],
                    },
                };
                let (result, out) = core::finish_preparation(
                    &s.job,
                    &core::Completion {
                        attempt: *attempt,
                        current_source_revision: Some("r".into()),
                        validated_output: Some("output".into()),
                    },
                    Some(&facts),
                );
                if *commit {
                    n.job = result;
                    effects = out;
                }
            }
        }
        Ok(Transition::accepted(n, effects))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = matches!(
            i,
            Input::Complete {
                attempt: 2,
                commit: true
            }
        ) && s.current == 0
            && s.job.job.phase == Phase::Running;
        let actual = n
            .outputs
            .iter()
            .any(|e| matches!(e, Effect::Publish { .. }));
        Ok(vec![
            if actual == expected {
                Check::passed("recipe.all_encoder_inputs_fence_publication")
            } else {
                Check::failed(
                    "recipe.all_encoder_inputs_fence_publication",
                    "wrong publication",
                )
            },
            if !matches!(i, Input::Complete { commit: false, .. }) || n.state == s {
                Check::passed("recipe.failed_commit_keeps_receipt")
            } else {
                Check::failed(
                    "recipe.failed_commit_keeps_receipt",
                    "failed commit changed state",
                )
            },
        ])
    }
}
impl Enumerate for Identity {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok((0..10)
            .map(Input::Change)
            .chain([
                Input::Cancel,
                Input::Complete {
                    attempt: 1,
                    commit: true,
                },
                Input::Complete {
                    attempt: 2,
                    commit: false,
                },
                Input::Complete {
                    attempt: 2,
                    commit: true,
                },
            ])
            .collect())
    }
}
#[test]
fn every_encoder_input_change_fences_stale_publication() {
    let r = stateless::explore::enumerate(
        &Identity,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 3000,
            max_depth: 10,
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
        "recipe identity: {} states / {} transitions",
        r.states, r.transitions
    );
}
