use playscale_core::parts::{Clip, PartPin};
use playscale_core::playback_session::{BoundFile, Claims, PlaybackBinding, Route};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Case {
    multipart: bool,
    mutation: u8,
}
fn claims(multipart: bool) -> Claims {
    Claims {
        principal: "p".into(),
        profile: "p".into(),
        timeline: "t".into(),
        version: "v".into(),
        file: "a".into(),
        file_revision: "r".into(),
        route: Route::Transcode,
        audio: None,
        subtitle: None,
        clip: (!multipart).then_some(Clip {
            start_ms: 10,
            end_ms: 50,
        }),
        parts: if multipart {
            ["a", "b"]
                .into_iter()
                .map(|file| PartPin {
                    file: file.into(),
                    revision: "r".into(),
                    duration_ms: 100,
                    audio: None,
                    subtitle: None,
                    burn: None,
                })
                .collect()
        } else {
            vec![]
        },
        max_bitrate_bps: None,
        adaptive: false,
        expires_at: 100,
    }
}
fn binding(multipart: bool) -> PlaybackBinding {
    PlaybackBinding {
        version: "v".into(),
        timeline: "t".into(),
        files: (0..if multipart { 2 } else { 1 })
            .map(|i| BoundFile {
                file: if i == 0 { "a" } else { "b" }.into(),
                binding_revision: "r".into(),
                current_revision: "r".into(),
                library: "lib".into(),
                duration_ms: Some(100),
                part: i + 1,
                start_ms: (!multipart).then_some(10),
                end_ms: (!multipart).then_some(50),
            })
            .collect(),
    }
}
struct Bindings;
impl Model for Bindings {
    type State = ();
    type Input = Case;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.exact_playback_binding".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/playback_session.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<(), ModelError> {
        Ok(())
    }
    fn step(&self, _: &(), i: &Case) -> Result<Transition<(), bool>, ModelError> {
        let mut b = binding(i.multipart);
        let last = b.files.len() - 1;
        match i.mutation {
            0 => {}
            1 => b.timeline = "other".into(),
            2 => b.version = "other".into(),
            3 => b.files[0].file = "other".into(),
            4 => b.files[last].binding_revision = "new".into(),
            5 => b.files[last].part = 9,
            6 => b.files.push(b.files[last].clone()),
            7 => {
                b.files.pop();
            }
            8 => b.files[0].start_ms = Some(-1),
            9 => b.files[last].end_ms = Some(49),
            10 => b.files[last].duration_ms = Some(99),
            11 => b.files[last].library = "other".into(),
            12 => b.files[last].current_revision = "new".into(),
            _ => unreachable!(),
        }
        Ok(Transition::accepted(
            (),
            vec![b.matches(&claims(i.multipart))],
        ))
    }
    fn check_state(&self, _: &()) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &(),
        i: &Case,
        n: &TransitionRef<'_, (), bool>,
    ) -> Result<Vec<Check>, ModelError> {
        // A single interval does not pin the full duration/library; source revision
        // is separately checked by admission to preserve SourceChanged disclosure.
        let expected = i.mutation == 0 || !i.multipart && matches!(i.mutation, 10..=12);
        Ok(vec![if n.outputs[0] == expected {
            Check::passed("exact_binding")
        } else {
            Check::failed(
                "exact_binding",
                "changed binding accepted or valid binding lost",
            )
        }])
    }
}
impl Enumerate for Bindings {
    fn inputs(&self, _: &()) -> Result<Vec<Case>, ModelError> {
        Ok([false, true]
            .into_iter()
            .flat_map(|multipart| {
                (0..13).map(move |mutation| Case {
                    multipart,
                    mutation,
                })
            })
            .collect())
    }
}
#[test]
fn clips_and_every_ordered_part_are_revalidated() {
    let r = stateless::explore::enumerate(
        &Bindings,
        stateless::explore::SearchConfig {
            max_states: 10,
            max_transitions: 100,
            max_depth: 2,
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
        "binding: {} states / {} transitions",
        r.states, r.transitions
    );
}
#[test]
fn generated_scope_requires_registered_current_original_lineage() {
    use playscale_core::renditions::{LineageMembership, library_scope};
    for mask in 0..32 {
        let generated = mask & 1 != 0;
        let output = mask & 2 != 0;
        let source = mask & 4 != 0;
        let original = mask & 8 != 0;
        let present = mask & 16 != 0;
        let lineage = LineageMembership {
            registered_output_revision: if output { "r" } else { "stale" }.into(),
            registered_source_revision: "s".into(),
            current_source_revision: present.then(|| if source { "s" } else { "stale" }.into()),
            source_generated: present.then_some(!original),
            library: Some("source-library".into()),
        };
        let actual = library_scope(generated, "r", ["direct-library".into()], &[lineage]);
        let expected: std::collections::BTreeSet<String> = if !generated {
            vec!["direct-library".into()]
        } else if output && source && original && present {
            vec!["source-library".into()]
        } else {
            vec![]
        }
        .into_iter()
        .collect();
        assert_eq!(actual, expected, "mask {mask}");
    }
}
