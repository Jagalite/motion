use playscale_core::identity::{
    Aggregate, Binding, Edition, Equivalence, Id, IdentityError, MergePlan, MergeRequest, Origin,
    Resolved, SplitRequest, Timeline, Version, Work, apply_merge, apply_split, commit_merge,
    merged_aliases, plan_merge, plan_split, resolve,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    works: BTreeMap<Id, Aggregate>,
    aliases: BTreeMap<Id, Id>,
    pending: Option<MergePlan>,
    next: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    /// Preview and immediately commit; `stale` reviews an older revision.
    Merge {
        source: Id,
        target: Id,
        stale: bool,
    },
    /// Preview only; the plan is committed by a later `CommitPending`.
    Preview {
        source: Id,
        target: Id,
    },
    CommitPending,
    Split {
        item: Id,
        versions: Vec<Id>,
        stale: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Merged { target: Id, retired: Vec<Id> },
    Split { from: Id, new_item: Id },
    Previewed,
    Rejected(IdentityError),
}

fn version(id: &str, file: &str, revision: &str) -> Version {
    Version {
        id: id.into(),
        origin: Origin::Original,
        equivalence: Equivalence::Declared,
        bindings: vec![Binding {
            file_id: file.into(),
            revision: revision.into(),
            part: 1,
            start_ms: None,
            end_ms: None,
        }],
    }
}
fn work(id: &str, external: Option<&str>, editions: Vec<Edition>) -> (Id, Aggregate) {
    (
        id.into(),
        Aggregate {
            work: Work {
                id: id.into(),
                revision: 1,
                external_ids: external
                    .map(|v| ("tmdb:movie".to_string(), v.to_string()))
                    .into_iter()
                    .collect(),
            },
            editions,
        },
    )
}
fn edition(id: &str, timelines: Vec<(&str, Vec<Version>)>) -> Edition {
    Edition {
        id: id.into(),
        label: id.into(),
        timelines: timelines
            .into_iter()
            .map(|(id, versions)| Timeline {
                id: id.into(),
                versions,
            })
            .collect(),
    }
}

struct Identity;
impl Model for Identity {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.catalog_identity".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/identity.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        // Two cuts and two encodes of one work, a duplicate work with the same
        // provider identity, and an unrelated work whose identity conflicts.
        Ok(State {
            works: BTreeMap::from([
                work(
                    "w1",
                    Some("1"),
                    vec![
                        edition(
                            "theatrical",
                            vec![(
                                "t1",
                                vec![version("v1", "f1", "a"), version("v2", "f2", "b")],
                            )],
                        ),
                        edition("directors", vec![("t2", vec![version("v3", "f3", "c")])]),
                    ],
                ),
                work(
                    "w2",
                    Some("1"),
                    vec![edition(
                        "copy",
                        vec![("t3", vec![version("v4", "f4", "a")])],
                    )],
                ),
                work(
                    "w3",
                    Some("2"),
                    vec![edition(
                        "other",
                        vec![("t4", vec![version("v5", "f5", "d")])],
                    )],
                ),
            ]),
            aliases: BTreeMap::new(),
            pending: None,
            next: 0,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match input {
            Input::Merge {
                source,
                target,
                stale,
            } => {
                let request = request(s, source, target, *stale);
                match plan_merge(&s.works, &s.aliases, &request)
                    .and_then(|plan| commit_merge(&plan, &s.works, &s.aliases))
                {
                    Ok(plan) => {
                        merge(&mut next, &plan);
                        Effect::Merged {
                            target: plan.target,
                            retired: plan.retired,
                        }
                    }
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Preview { source, target } => {
                match plan_merge(&s.works, &s.aliases, &request(s, source, target, false)) {
                    Ok(plan) => {
                        next.pending = Some(plan);
                        Effect::Previewed
                    }
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::CommitPending => match &s.pending {
                None => Effect::Rejected(IdentityError::EmptySelection),
                Some(plan) => {
                    next.pending = None;
                    match commit_merge(plan, &s.works, &s.aliases) {
                        Ok(plan) => {
                            merge(&mut next, &plan);
                            Effect::Merged {
                                target: plan.target,
                                retired: plan.retired,
                            }
                        }
                        Err(e) => Effect::Rejected(e),
                    }
                }
            },
            Input::Split {
                item,
                versions,
                stale,
            } => match s.works.get(item) {
                None => Effect::Rejected(IdentityError::UnknownItem(item.clone())),
                Some(aggregate) => {
                    let mut counter = s.next;
                    let mut ids = || {
                        counter += 1;
                        format!("n{counter}")
                    };
                    let request = SplitRequest {
                        item: item.clone(),
                        versions: versions.clone(),
                        new_title: "Split".into(),
                        expected_revision: aggregate.work.revision - u64::from(*stale),
                    };
                    match plan_split(aggregate, &request, &mut ids) {
                        Ok(plan) => {
                            next.next = counter;
                            apply_split(&mut next.works, &plan);
                            Effect::Split {
                                from: plan.item,
                                new_item: plan.new_item,
                            }
                        }
                        Err(e) => Effect::Rejected(e),
                    }
                }
            },
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let live: BTreeSet<Id> = s.works.keys().cloned().collect();
        let resolves = s.aliases.iter().all(|(from, to)| {
            !live.contains(from)
                && live.contains(to)
                && resolve(&live, &s.aliases, from)
                    == Ok(Resolved::Alias {
                        from: from.clone(),
                        to: to.clone(),
                    })
        });
        let one_value_per_namespace = s.works.values().all(|w| {
            let namespaces: BTreeSet<_> = w.work.external_ids.iter().map(|(n, _)| n).collect();
            namespaces.len() == w.work.external_ids.len()
        });
        Ok(vec![
            check("catalog.aliases_resolve", resolves),
            check("catalog.external_identity_unique", one_value_per_namespace),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let rejected = matches!(next.outputs, [Effect::Rejected(_)]);
        let unchanged_catalog = after.works == before.works && after.aliases == before.aliases;
        let before_timelines = timelines(before);
        let after_timelines = timelines(after);
        let stale = matches!(
            input,
            Input::Merge { stale: true, .. } | Input::Split { stale: true, .. }
        );
        let expected_effect = match next.outputs {
            [Effect::Merged { target: t, retired }] => {
                let (source, target) = match input {
                    Input::Merge { source, target, .. } => (source.clone(), target.clone()),
                    Input::CommitPending => match &before.pending {
                        Some(plan) => (plan.retired[0].clone(), plan.target.clone()),
                        None => return Ok(vec![check("catalog.exact_effect", false)]),
                    },
                    _ => return Ok(vec![check("catalog.exact_effect", false)]),
                };
                t == &target
                    && retired == &vec![source.clone()]
                    && !after.works.contains_key(&source)
                    && after.aliases.get(&source) == Some(&target)
                    && after.works[&target].work.revision == before.works[&target].work.revision + 1
            }
            [Effect::Split { from, new_item }] => {
                matches!(input, Input::Split { item, .. } if item == from)
                    && !before.works.contains_key(new_item)
                    && after.works.contains_key(new_item)
                    && after.works[from].work.revision == before.works[from].work.revision + 1
            }
            [Effect::Previewed] => {
                matches!(input, Input::Preview { .. })
                    && unchanged_catalog
                    && after.pending.is_some()
            }
            [Effect::Rejected(_)] => unchanged_catalog,
            _ => false,
        };
        Ok(vec![
            check("catalog.exact_effect", expected_effect),
            check(
                "catalog.originals_immutable",
                bindings(before) == bindings(after),
            ),
            check(
                "catalog.no_implicit_timeline_merge",
                after_timelines.iter().all(|(id, versions)| {
                    before_timelines
                        .get(id)
                        .is_none_or(|old| versions.is_subset(old))
                }) && (!matches!(next.outputs, [Effect::Merged { .. }])
                    || after_timelines == before_timelines),
            ),
            check(
                "catalog.timeline_ids_preserved",
                before_timelines
                    .keys()
                    .all(|id| after_timelines.contains_key(id)),
            ),
            check("catalog.stale_rejected", !stale || rejected),
        ])
    }
}

fn request(s: &State, source: &Id, target: &Id, stale: bool) -> MergeRequest {
    let revision = |id: &Id| s.works.get(id).map(|w| w.work.revision);
    MergeRequest {
        sources: vec![source.clone()],
        target: target.clone(),
        expected: [source, target]
            .into_iter()
            .filter_map(|id| revision(id).map(|r| (id.clone(), r)))
            .map(|(id, r)| (id.clone(), if stale && &id == target { r - 1 } else { r }))
            .collect(),
    }
}
fn merge(state: &mut State, plan: &MergePlan) {
    state.aliases = merged_aliases(&state.aliases, plan);
    apply_merge(&mut state.works, plan);
}
fn bindings(s: &State) -> BTreeMap<(Id, String), usize> {
    let mut all = BTreeMap::new();
    for binding in s
        .works
        .values()
        .flat_map(|w| &w.editions)
        .flat_map(|e| &e.timelines)
        .flat_map(|t| &t.versions)
        .flat_map(|v| &v.bindings)
    {
        *all.entry((binding.file_id.clone(), binding.revision.clone()))
            .or_default() += 1;
    }
    all
}
fn timelines(s: &State) -> BTreeMap<Id, BTreeSet<Id>> {
    s.works
        .values()
        .flat_map(|w| &w.editions)
        .flat_map(|e| &e.timelines)
        .map(|t| {
            (
                t.id.clone(),
                t.versions.iter().map(|v| v.id.clone()).collect(),
            )
        })
        .collect()
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "catalog identity invariant violated")
    }
}

impl Enumerate for Identity {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        // Every ordered pair of known (live or retired) works, so retired IDs and
        // self-merges are exercised as rejected inputs.
        let mut ids: BTreeSet<Id> = s.works.keys().cloned().collect();
        ids.extend(s.aliases.keys().cloned());
        let mut inputs = vec![Input::CommitPending];
        for source in &ids {
            for target in &ids {
                let live = s.works.contains_key(source) && s.works.contains_key(target);
                for stale in [false, true].into_iter().filter(|stale| live || !stale) {
                    inputs.push(Input::Merge {
                        source: source.clone(),
                        target: target.clone(),
                        stale,
                    });
                }
                if live && s.pending.is_none() {
                    inputs.push(Input::Preview {
                        source: source.clone(),
                        target: target.clone(),
                    });
                }
            }
        }
        // One split per path keeps the graph finite while covering split-before-
        // merge, split-between-preview-and-commit, and split-after-merge orders.
        if s.next == 0 {
            for (id, work) in &s.works {
                let versions: Vec<Id> = work
                    .editions
                    .iter()
                    .flat_map(|e| &e.timelines)
                    .flat_map(|t| &t.versions)
                    .map(|v| v.id.clone())
                    .collect();
                for v in &versions {
                    for stale in [false, true] {
                        inputs.push(Input::Split {
                            item: id.clone(),
                            versions: vec![v.clone()],
                            stale,
                        });
                    }
                }
                if versions.len() >= 2 {
                    inputs.push(Input::Split {
                        item: id.clone(),
                        versions: versions[..2].to_vec(),
                        stale: false,
                    });
                }
            }
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Identity {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Identity {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
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
fn bounded_identity_graph_checks_production_decisions() {
    let report = stateless::explore::enumerate(&Identity, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    assert!(report.transitions > 1000, "{}", report.transitions);
    println!(
        "Stateless: {} states, {} edges; 3 works, <=1 split, one pending preview, stale/retired/self merges",
        report.states, report.transitions
    );
}

#[test]
fn preview_then_intervening_split_rejects_commit_without_partial_apply() {
    let inputs = vec![
        Input::Preview {
            source: "w2".into(),
            target: "w1".into(),
        },
        Input::Split {
            item: "w1".into(),
            versions: vec!["v3".into()],
            stale: false,
        },
        Input::CommitPending,
    ];
    let trace =
        stateless::execution::record(&Identity, inputs.clone(), Default::default(), 10).unwrap();
    let replay = stateless::execution::replay(&Identity, &trace, Default::default()).unwrap();
    assert_eq!(replay.outcome, stateless::execution::ReplayOutcome::Exact);
    let mut state = Identity.initial_state().unwrap();
    let mut last = Vec::new();
    for input in &inputs {
        let transition = Identity.step(&state, input).unwrap();
        state = transition.state;
        last = transition.outputs;
    }
    assert_eq!(
        last,
        vec![Effect::Rejected(IdentityError::StaleRevision("w1".into()))]
    );
    assert!(state.works.contains_key("w2"));
    assert!(state.aliases.is_empty());
}

#[test]
fn seeded_identity_sequences() {
    let report = stateless::explore::fuzz(
        &Identity,
        stateless::explore::FuzzConfig {
            seed: 20261009,
            cases: 300,
            max_steps: 30,
            max_transitions: 20000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
}
