//! Exact source-revision pins survive catalog changes without substitution.
use playscale_core::{
    components::{Component, Kind, Occurrence, Roles},
    processing::{
        self as core, PreparationSelection, PreparationStream, SelectedStreams, SelectionRefusal,
    },
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    revision_current: bool,
    sidecar: bool,
    bitmap: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Revision(bool),
    Sidecar(bool),
    Bitmap(bool),
    Select(u8),
}
struct Selection;
impl Model for Selection {
    type State = State;
    type Input = Input;
    type Output = Result<SelectedStreams, SelectionRefusal>;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.preparation_selection".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/processing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            revision_current: true,
            sidecar: false,
            bitmap: false,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Self::Output>, ModelError> {
        let mut n = s.clone();
        match i {
            Input::Revision(v) => n.revision_current = *v,
            Input::Sidecar(v) => n.sidecar = *v,
            Input::Bitmap(v) => n.bitmap = *v,
            Input::Select(_) => {}
        }
        let Input::Select(case) = i else {
            return Ok(Transition::accepted(n, vec![]));
        };
        let streams = vec![
            PreparationStream {
                index: 3,
                kind: "audio".into(),
                codec: "aac".into(),
            },
            PreparationStream {
                index: 5,
                kind: "audio".into(),
                codec: "aac".into(),
            },
            PreparationStream {
                index: 8,
                kind: "subtitle".into(),
                codec: if n.bitmap {
                    "hdmv_pgs_subtitle"
                } else {
                    "subrip"
                }
                .into(),
            },
        ];
        let components = vec![("audio", Kind::Audio, 5), ("subtitle", Kind::Subtitle, 8)]
            .into_iter()
            .map(|(id, kind, index)| Component {
                id: id.into(),
                kind,
                language: None,
                title: None,
                roles: Roles::default(),
                ordinal: 0,
                occurrences: vec![Occurrence {
                    version_id: "v".into(),
                    file_id: "f".into(),
                    file_revision: if n.revision_current { "r" } else { "old" }.into(),
                    track_index: if n.sidecar && kind == Kind::Subtitle {
                        None
                    } else {
                        Some(index)
                    },
                    sidecar: if n.sidecar && kind == Kind::Subtitle {
                        Some("text.srt".into())
                    } else {
                        None
                    },
                    default: false,
                }],
            })
            .collect::<Vec<_>>();
        let mut request = PreparationSelection::default();
        match case {
            0 => {}
            1 => request.audio_component = Some("audio".into()),
            2 => {
                request.audio_component = Some("audio".into());
                request.audio_track = Some("a0".into());
            }
            3 => {
                request.audio_component = Some("audio".into());
                request.audio_track = Some("a1".into());
            }
            4 => request.subtitle_component = Some("subtitle".into()),
            5 => request.subtitle_track = Some("s0".into()),
            _ => request.audio_track = Some("a2".into()),
        };
        let output = core::preparation_selection(&request, &streams, &components, "v", "f", "r");
        Ok(Transition::accepted(n, vec![output]))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Self::Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let Input::Select(case) = i else {
            return Ok(vec![]);
        };
        let s = n.state;
        let expected = match case {
            0 => Ok(SelectedStreams {
                audio: None,
                subtitle: None,
            }),
            1..=3 if !s.revision_current => Err(SelectionRefusal::ComponentUnavailable),
            1 | 3 => Ok(SelectedStreams {
                audio: Some(1),
                subtitle: None,
            }),
            2 => Err(SelectionRefusal::ConflictingPins),
            4 if !s.revision_current => Err(SelectionRefusal::ComponentUnavailable),
            4 if s.sidecar => Err(SelectionRefusal::UnsupportedSubtitle),
            4 | 5 if s.bitmap => Err(SelectionRefusal::UnsupportedSubtitle),
            4 | 5 => Ok(SelectedStreams {
                audio: None,
                subtitle: Some(0),
            }),
            _ => Err(SelectionRefusal::UnknownTrack),
        };
        Ok(vec![if n.outputs == [expected] {
            Check::passed("selection.exact_revision_and_codec")
        } else {
            Check::failed(
                "selection.exact_revision_and_codec",
                format!("case {case}: {:?}", n.outputs),
            )
        }])
    }
}
impl Enumerate for Selection {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Revision(false),
            Input::Revision(true),
            Input::Sidecar(false),
            Input::Sidecar(true),
            Input::Bitmap(false),
            Input::Bitmap(true),
        ]
        .into_iter()
        .chain((0..7).map(Input::Select))
        .collect())
    }
}
#[test]
fn preparation_selection_exhausts_revision_and_pin_changes() {
    let r = stateless::explore::enumerate(
        &Selection,
        stateless::explore::SearchConfig {
            max_states: 32,
            max_transitions: 1000,
            max_depth: 5,
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
        "preparation selection: {} states / {} transitions",
        r.states, r.transitions
    );
}
