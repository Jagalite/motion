//! The committed subtitle identity and complete-copy reporting share the same
//! production decisions as the download writer transaction.
use playscale_core::downloads::*;
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct World {
    artifact: SubtitleArtifact,
    revision: u64,
    cancelled: bool,
    report: Option<TransferReport>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Publish(SubtitleArtifact),
    Cancel,
    Report(TransferReport),
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Artifact(SubtitlePublication),
    Report(Result<TransferDecision, TransferRefusal>),
}
struct Subtitles;
fn size(a: &SubtitleArtifact) -> Option<u64> {
    match a {
        SubtitleArtifact::Ready { size, .. } => Some(*size),
        _ => None,
    }
}
impl Model for Subtitles {
    type State = World;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.subtitle.transfer".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/downloads.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<World, ModelError> {
        Ok(World {
            artifact: SubtitleArtifact::Pending,
            revision: 1,
            cancelled: false,
            report: None,
        })
    }
    fn step(&self, w: &World, i: &Input) -> Result<Transition<World, Effect>, ModelError> {
        let mut n = w.clone();
        let mut e = vec![];
        match i {
            Input::Publish(incoming) => {
                let decision = publish_subtitle(!w.cancelled, &w.artifact, incoming);
                if decision == SubtitlePublication::Record {
                    n.artifact = incoming.clone();
                    n.revision += 1;
                }
                e.push(Effect::Artifact(decision));
            }
            Input::Cancel if !w.cancelled => {
                n.cancelled = true;
                n.revision += 1;
            }
            Input::Report(report) => {
                let decision = accept_transfer_with_artifacts(
                    w.report.as_ref(),
                    report,
                    w.revision,
                    100,
                    true,
                    size(&w.artifact),
                    w.cancelled,
                );
                if decision == Ok(TransferDecision::Record) {
                    n.report = Some(*report);
                }
                e.push(Effect::Report(decision));
            }
            _ => {}
        }
        Ok(Transition::accepted(n, e))
    }
    fn check_state(&self, _: &World) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        w: &World,
        i: &Input,
        n: &TransitionRef<'_, World, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match i {
            Input::Publish(incoming) => {
                let decision = if w.cancelled {
                    SubtitlePublication::Cancelled
                } else if w.artifact == SubtitleArtifact::Pending {
                    SubtitlePublication::Record
                } else if &w.artifact == incoming || incoming == &SubtitleArtifact::Failed {
                    SubtitlePublication::Unchanged
                } else {
                    SubtitlePublication::Changed
                };
                vec![Effect::Artifact(decision)]
            }
            Input::Report(r) => {
                let total = 100 + size(&w.artifact).unwrap_or(0);
                let decision =
                    if r.state == TransferState::VerifiedLocal && size(&w.artifact).is_none() {
                        Err(TransferRefusal::ArtifactsUnavailable)
                    } else if r.download_revision != w.revision {
                        Err(TransferRefusal::DownloadChanged)
                    } else if r.bytes_received > total
                        || (r.state == TransferState::VerifiedLocal && r.bytes_received != total)
                        || (r.state == TransferState::Removed && r.bytes_received != 0)
                    {
                        Err(TransferRefusal::InvalidProgress)
                    } else if let Some(old) = w.report.filter(|old| old.sequence >= r.sequence) {
                        if old.sequence > r.sequence {
                            Err(TransferRefusal::Stale)
                        } else if old == *r {
                            Ok(TransferDecision::Unchanged)
                        } else {
                            Err(TransferRefusal::Conflict)
                        }
                    } else if w.cancelled && r.state != TransferState::Removed {
                        Err(TransferRefusal::Ended)
                    } else {
                        Ok(TransferDecision::Record)
                    };
                vec![Effect::Report(decision)]
            }
            _ => vec![],
        };
        let immutable = w.artifact == SubtitleArtifact::Pending || n.state.artifact == w.artifact;
        Ok(vec![if n.outputs == expected && immutable {
            Check::passed("exact_artifact_and_complete_bytes")
        } else {
            Check::failed(
                "exact_artifact_and_complete_bytes",
                "changed identity or incomplete copy accepted",
            )
        }])
    }
}
impl Enumerate for Subtitles {
    fn inputs(&self, w: &World) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Cancel,
            Input::Publish(SubtitleArtifact::Failed),
            Input::Publish(SubtitleArtifact::Ready {
                digest: "a".into(),
                size: 5,
            }),
            Input::Publish(SubtitleArtifact::Ready {
                digest: "b".into(),
                size: 9,
            }),
        ];
        for sequence in [1, 2] {
            for revision in [1, w.revision] {
                for bytes in [0, 100, 105, 109] {
                    for state in [
                        TransferState::VerifiedLocal,
                        TransferState::Transferring,
                        TransferState::Removed,
                    ] {
                        inputs.push(Input::Report(TransferReport {
                            sequence,
                            download_revision: revision,
                            state,
                            bytes_received: bytes,
                        }));
                    }
                }
            }
        }
        Ok(inputs)
    }
}
#[test]
fn subtitle_commit_cancel_and_reordered_reports() {
    let r = stateless::explore::enumerate(
        &Subtitles,
        stateless::explore::SearchConfig {
            max_states: 20000,
            max_transitions: 1000000,
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
