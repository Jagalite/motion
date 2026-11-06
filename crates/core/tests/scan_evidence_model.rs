//! Revision-aware chapters and partial directory sidecar evidence.
use playscale_core::{
    components::{self, Roles, SidecarEffect, SidecarSubtitle},
    scan::{self, ChapterEffect},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    revision: String,
    chapters: Option<String>,
    sidecars: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    revision: String,
    chapters: Option<String>,
    complete: bool,
    seen: u8,
    fingerprint: String,
    rollback: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Applied {
        chapters: ChapterEffect,
        sidecars: Vec<SidecarEffect>,
    },
    RolledBack,
}
struct Evidence;
fn observations(i: &Input) -> BTreeMap<String, Vec<(String, String)>> {
    let mut list = vec![];
    for (bit, name) in [(1, "film.en.srt"), (2, "film.fr.srt")] {
        if i.seen & bit != 0 {
            list.push((name.into(), i.fingerprint.clone()));
        }
    }
    BTreeMap::from([("dir".into(), list)])
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "scan evidence invariant")
    }
}
impl Model for Evidence {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.scan_evidence".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}{}",
                include_str!("../src/scan.rs"),
                include_str!("../src/components.rs")
            ),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            revision: "a".into(),
            chapters: Some("old".into()),
            sidecars: BTreeMap::from([
                ("film.en.srt".into(), "old".into()),
                ("film.de.srt".into(), "old".into()),
            ]),
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let chapter = scan::chapter_evidence(Some(&s.revision), &i.revision, i.chapters.as_deref());
        let coverage = if i.complete {
            BTreeSet::from(["dir".into()])
        } else {
            BTreeSet::new()
        };
        let sidecars =
            components::sidecar_plan("file", "dir/film.mkv", &observations(i), &coverage);
        let mut next = s.clone();
        let effect = if i.rollback {
            Effect::RolledBack
        } else {
            next.revision = i.revision.clone();
            if let ChapterEffect::Set(value) = &chapter {
                next.chapters = value.clone();
            }
            for effect in &sidecars {
                match effect {
                    SidecarEffect::Clear { .. } => next.sidecars.clear(),
                    SidecarEffect::Upsert {
                        name, fingerprint, ..
                    } => {
                        next.sidecars.insert(name.clone(), fingerprint.clone());
                    }
                }
            }
            Effect::Applied {
                chapters: chapter,
                sidecars,
            }
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        b: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        if i.rollback {
            return Ok(vec![check(
                "evidence.rollback_noop",
                n.state == b && n.outputs == [Effect::RolledBack],
            )]);
        }
        let expected_chapters = if let Some(c) = &i.chapters {
            Some(c.clone())
        } else if b.revision == i.revision {
            b.chapters.clone()
        } else {
            None
        };
        let mut expected = if i.complete {
            BTreeMap::new()
        } else {
            b.sidecars.clone()
        };
        let mut expected_effects = Vec::new();
        if i.complete {
            expected_effects.push(SidecarEffect::Clear {
                file: "file".into(),
            });
        }
        for (bit, name, language) in [(1, "film.en.srt", "en"), (2, "film.fr.srt", "fr")] {
            if i.seen & bit != 0 {
                expected.insert(name.into(), i.fingerprint.clone());
                expected_effects.push(SidecarEffect::Upsert {
                    file: "file".into(),
                    name: name.into(),
                    fingerprint: i.fingerprint.clone(),
                    subtitle: SidecarSubtitle {
                        language: Some(language.into()),
                        roles: Roles::default(),
                        format: "srt".into(),
                    },
                });
            }
        }
        let expected_chapter_effect = if i.chapters.is_some() || b.revision != i.revision {
            ChapterEffect::Set(i.chapters.clone())
        } else {
            ChapterEffect::Retain
        };
        Ok(vec![
            check(
                "evidence.revision_fences_chapters",
                n.state.chapters == expected_chapters,
            ),
            check(
                "evidence.coverage_preserves_unseen_sidecars",
                n.state.sidecars == expected,
            ),
            check(
                "evidence.exact_effects",
                n.outputs
                    == [Effect::Applied {
                        chapters: expected_chapter_effect,
                        sidecars: expected_effects,
                    }],
            ),
        ])
    }
}
impl Enumerate for Evidence {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = Vec::new();
        for revision in ["a", "b"] {
            for chapters in [None, Some("[]"), Some("new")] {
                for complete in [false, true] {
                    for seen in 0..4 {
                        for fingerprint in ["v1", "v2"] {
                            for rollback in [false, true] {
                                inputs.push(Input {
                                    revision: revision.into(),
                                    chapters: chapters.map(str::to_owned),
                                    complete,
                                    seen,
                                    fingerprint: fingerprint.into(),
                                    rollback,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(inputs)
    }
}
impl ModelCodec for Evidence {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        dec(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        dec(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
}
fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn dec<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(b).map_err(|e| ModelError::new(e.to_string()))
}
#[test]
fn bounded_evidence_preserves_only_proven_data() {
    let report = stateless::explore::enumerate(
        &Evidence,
        stateless::explore::SearchConfig {
            max_states: 100000,
            max_transitions: 5000000,
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
        "Scan evidence: {} states, {} transitions; two revisions, absent/empty/new chapters, complete/partial coverage, two observed sidecars and rollback",
        report.states, report.transitions
    );
}
