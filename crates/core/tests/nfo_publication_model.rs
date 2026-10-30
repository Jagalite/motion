//! Two-work NFO publication and withdrawal interleavings using production plans.
use playscale_core::{
    matching::MatchState,
    nfo::{self, PublicationEffect, SidecarObservation, WorkFacts},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Work {
    document: Option<String>,
    external: Option<String>,
    revision: Option<i64>,
    origin: bool,
    pinned: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    works: [Work; 2],
    target: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Publish {
        observation: SidecarObservation,
        rollback: bool,
    },
    Move,
    Pin(usize),
    Unpin(usize),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Applied(Vec<PublicationEffect>),
    RolledBack,
    Observed,
}
struct Nfo;
fn name(n: usize) -> String {
    format!("work-{n}")
}
fn index(id: &str) -> usize {
    if id == "work-0" { 0 } else { 1 }
}
fn facts(s: &State, n: usize) -> WorkFacts {
    let w = &s.works[n];
    WorkFacts {
        item: name(n),
        match_state: if w.pinned {
            MatchState::Manual
        } else {
            MatchState::Matched
        },
        external_id: w.external.clone(),
        document: w.document.clone(),
        revision: w.revision,
    }
}

/// Independent expected effects for the two-work bounded domain.
fn expected(s: &State, observation: &SidecarObservation) -> Vec<PublicationEffect> {
    use PublicationEffect::*;
    if matches!(observation, SidecarObservation::Unusable) {
        return vec![];
    }
    let target = &s.works[s.target];
    if let SidecarObservation::Parsed { external_id, .. } = observation {
        if target.pinned && target.external.is_some() && target.external != *external_id {
            return vec![];
        }
    }
    let mut effects = Vec::new();
    let mut removed = [false; 2];
    for (n, work) in s.works.iter().enumerate() {
        let retained_target =
            matches!(observation, SidecarObservation::Parsed { .. }) && n == s.target;
        if work.origin && !retained_target && !(work.pinned && work.external.is_some()) {
            removed[n] = true;
            effects.extend([
                DeleteContribution { item: name(n) },
                ClearOrigin { item: name(n) },
                ProjectTitle { item: name(n) },
            ]);
        }
    }
    if let SidecarObservation::Parsed {
        document,
        external_id,
    } = observation
    {
        let occupied = external_id.as_ref().is_some_and(|id| {
            s.works
                .iter()
                .enumerate()
                .any(|(n, work)| n != s.target && !removed[n] && work.external.as_ref() == Some(id))
        });
        let external = if occupied { None } else { external_id.clone() };
        if target.document.as_ref() != Some(document) || target.external != external {
            effects.push(PutContribution {
                item: name(s.target),
                document: document.clone(),
                external_id: external,
                revision: target.revision.map_or(1, |r| r + 1),
            });
        }
        effects.extend([
            SetOrigin {
                item: name(s.target),
                file: "file".into(),
            },
            ProjectTitle {
                item: name(s.target),
            },
        ]);
    }
    effects
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "NFO publication invariant")
    }
}
impl Model for Nfo {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.nfo_publication".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/nfo.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        let w = Work {
            document: None,
            external: None,
            revision: None,
            origin: false,
            pinned: false,
        };
        Ok(State {
            works: [w.clone(), w],
            target: 0,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match i {
            Input::Move => {
                next.target = 1 - s.target;
                Effect::Observed
            }
            Input::Pin(n) => {
                next.works[*n].pinned = true;
                Effect::Observed
            }
            Input::Unpin(n) => {
                next.works[*n].pinned = false;
                Effect::Observed
            }
            Input::Publish {
                observation,
                rollback,
            } => {
                let owners: Vec<_> = (0..2)
                    .filter(|n| s.works[*n].origin)
                    .map(|n| facts(s, n))
                    .collect();
                let proposed = match observation {
                    SidecarObservation::Parsed { external_id, .. } => external_id.as_deref(),
                    _ => None,
                };
                let conflict = proposed
                    .and_then(|id| (0..2).find(|n| s.works[*n].external.as_deref() == Some(id)))
                    .map(name);
                let target = matches!(observation, SidecarObservation::Parsed { .. })
                    .then(|| facts(s, s.target));
                let effects = nfo::publication(
                    "file",
                    target.as_ref(),
                    &owners,
                    conflict.as_deref(),
                    observation,
                )
                .map_err(|e| ModelError::new(format!("{e:?}")))?;
                if *rollback {
                    Effect::RolledBack
                } else {
                    for e in &effects {
                        match e {
                            PublicationEffect::DeleteContribution { item } => {
                                let w = &mut next.works[index(item)];
                                w.document = None;
                                w.external = None;
                                w.revision = None;
                            }
                            PublicationEffect::ClearOrigin { item } => {
                                next.works[index(item)].origin = false
                            }
                            PublicationEffect::PutContribution {
                                item,
                                document,
                                external_id,
                                revision,
                            } => {
                                let w = &mut next.works[index(item)];
                                w.document = Some(document.clone());
                                w.external = external_id.clone();
                                w.revision = Some(*revision);
                            }
                            PublicationEffect::SetOrigin { item, .. } => {
                                next.works[index(item)].origin = true
                            }
                            PublicationEffect::ProjectTitle { .. } => {}
                        }
                    }
                    Effect::Applied(effects)
                }
            }
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let a = &s.works[0].external;
        let b = &s.works[1].external;
        Ok(vec![
            check(
                "nfo.external_identity_unique",
                a.is_none() || b.is_none() || a != b,
            ),
            check(
                "nfo.origin_has_contribution",
                s.works.iter().all(|w| !w.origin || w.document.is_some()),
            ),
            check(
                "nfo.document_revision_agree",
                s.works
                    .iter()
                    .all(|w| w.document.is_some() == w.revision.is_some()),
            ),
        ])
    }
    fn check_transition(
        &self,
        b: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let preserved = (0..2).all(|k| {
            !b.works[k].pinned
                || b.works[k].external.is_none()
                || b.works[k].external == n.state.works[k].external
        });
        let rollback = !matches!(i, Input::Publish { rollback: true, .. }) || n.state == b;
        let unusable = !matches!(
            i,
            Input::Publish {
                observation: SidecarObservation::Unusable,
                rollback: false
            }
        ) || (n.state == b && n.outputs == [Effect::Applied(vec![])]);
        let exact = match i {
            Input::Publish {
                observation,
                rollback: false,
            } => n.outputs == [Effect::Applied(expected(b, observation))],
            _ => true,
        };
        Ok(vec![
            check("nfo.manual_identity_is_preserved", preserved),
            check("nfo.rollback_has_no_partial_state", rollback),
            check("nfo.unusable_observation_is_noop", unusable),
            check("nfo.exact_ordered_effects", exact),
        ])
    }
}
impl Enumerate for Nfo {
    fn inputs(&self, state: &State) -> Result<Vec<Input>, ModelError> {
        let observations = [
            SidecarObservation::Unusable,
            SidecarObservation::Absent,
            SidecarObservation::Parsed {
                document: "A".into(),
                external_id: Some("id-a".into()),
            },
            SidecarObservation::Parsed {
                document: "B".into(),
                external_id: Some("id-b".into()),
            },
            SidecarObservation::Parsed {
                document: "A".into(),
                external_id: None,
            },
        ];
        let mut v = vec![
            Input::Move,
            Input::Pin(0),
            Input::Pin(1),
            Input::Unpin(0),
            Input::Unpin(1),
        ];
        for observation in observations {
            if matches!(observation, SidecarObservation::Parsed { .. })
                && state.works[state.target].revision.is_some_and(|r| r >= 3)
            {
                continue;
            }
            for rollback in [false, true] {
                v.push(Input::Publish {
                    observation: observation.clone(),
                    rollback,
                });
            }
        }
        Ok(v)
    }
}
impl ModelCodec for Nfo {
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
fn bounded_publication_moves_preserve_pins_and_identity() {
    let report = stateless::explore::enumerate(
        &Nfo,
        stateless::explore::SearchConfig {
            max_states: 100000,
            max_transitions: 2000000,
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
        "NFO publication: {} states, {} transitions; two works, one file, two identities, pins, unusable/absent/parsed observations and rollback",
        report.states, report.transitions
    );
}
