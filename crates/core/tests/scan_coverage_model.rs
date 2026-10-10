//! Stateless model of guarded observation: filesystem mutations interleaved with
//! scans whose directory listings may fail. The model applies the production
//! reconciliation plan literally and checks it against the true filesystem.
use playscale_core::scan::{Assignment, Coverage, Existing, Observed, reconcile_scoped};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::{BTreeMap, BTreeSet};

const PATHS: [&str; 4] = ["a.mkv", "d1/b.mkv", "d1/c.mkv", "d2/b.mkv"];
const DIRS: [&str; 2] = ["d1", "d2"];

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Record {
    edition: String,
    path: String,
    revision: String,
    available: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    fs: BTreeMap<String, String>,
    /// Existing subdirectories; emptying one does not remove it.
    dirs: BTreeSet<String>,
    /// Source exclusions (directory prefixes).
    exclusions: Vec<String>,
    records: BTreeMap<String, Record>,
    editions: u32,
    files: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Write {
        path: String,
        revision: String,
    },
    Remove {
        path: String,
    },
    /// Remove a subdirectory with everything in it.
    RemoveDir(String),
    /// Set (or clear) the source's exclusion of a subdirectory.
    Exclude(Option<String>),
    /// Scan with one directory unreadable (or none).
    Scan {
        unreadable: Option<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Published {
        unavailable: Vec<String>,
        excluded: Vec<String>,
        assignments: Vec<Assignment>,
    },
}

struct Observation;
impl Model for Observation {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.scan_coverage".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/scan.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            fs: BTreeMap::from([
                ("a.mkv".into(), "r1".into()),
                ("d1/b.mkv".into(), "r2".into()),
            ]),
            dirs: ["d1".to_string()].into(),
            exclusions: vec![],
            records: BTreeMap::new(),
            editions: 0,
            files: 0,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let mut outputs = Vec::new();
        match input {
            Input::Write { path, revision } => {
                next.fs.insert(path.clone(), revision.clone());
                if !dir(path).is_empty() {
                    next.dirs.insert(dir(path).to_string());
                }
            }
            Input::Exclude(d) => {
                next.exclusions = d.iter().cloned().collect();
            }
            Input::RemoveDir(d) => {
                next.dirs.remove(d);
                next.fs.retain(|p, _| dir(p) != d);
            }
            Input::Remove { path } => {
                next.fs.remove(path);
            }
            Input::Scan { unreadable } => {
                let (found, coverage) = observe(s, unreadable.as_deref());
                let old: Vec<Existing> = s
                    .records
                    .iter()
                    .map(|(id, r)| Existing {
                        id: id.clone(),
                        edition: r.edition.clone(),
                        path: r.path.clone(),
                        revision: r.revision.clone(),
                    })
                    .collect();
                let (plan, excluded) = reconcile_scoped(&old, &found, &coverage, &s.exclusions);
                for id in plan.unavailable.iter().chain(&excluded) {
                    next.records.get_mut(id).unwrap().available = false;
                }
                let mut assigned: Vec<String> = Vec::new();
                for (file, assignment) in found.iter().zip(&plan.assignments) {
                    let (id, edition) = match assignment {
                        Assignment::OutOfScope => {
                            assigned.push(String::new());
                            continue;
                        }
                        Assignment::CopyOf { observation } => {
                            next.files += 1;
                            (format!("f{}", next.files), assigned[*observation].clone())
                        }
                        Assignment::Existing { id, edition } => (id.clone(), edition.clone()),
                        Assignment::Copy { edition } => {
                            next.files += 1;
                            (format!("f{}", next.files), edition.clone())
                        }
                        Assignment::New => {
                            next.files += 1;
                            next.editions += 1;
                            (format!("f{}", next.files), format!("e{}", next.editions))
                        }
                    };
                    assigned.push(edition.clone());
                    next.records.insert(
                        id,
                        Record {
                            edition,
                            path: file.path.clone(),
                            revision: file.revision.clone(),
                            available: true,
                        },
                    );
                }
                outputs.push(Effect::Published {
                    unavailable: plan.unavailable,
                    excluded,
                    assignments: plan.assignments,
                });
            }
        }
        Ok(Transition::accepted(next, outputs))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let mut paths = BTreeSet::new();
        let unique_paths = s
            .records
            .values()
            .filter(|r| r.available)
            .all(|r| paths.insert(r.path.as_str()));
        Ok(vec![check(
            "scan.one_available_occurrence_per_path",
            unique_paths,
        )])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let Input::Scan { unreadable } = input else {
            return Ok(vec![check(
                "scan.filesystem_change_alone_changes_nothing",
                after.records == before.records && next.outputs.is_empty(),
            )]);
        };
        let (found, coverage) = observe(before, unreadable.as_deref());
        // Only an existing directory can be unreadable.
        let unreadable = unreadable.as_ref().filter(|d| before.dirs.contains(*d));
        let Some(Effect::Published {
            unavailable,
            excluded,
            assignments,
        }) = next.outputs.first()
        else {
            return Ok(vec![check("scan.exact_effect", false)]);
        };
        let out = |path: &str| playscale_core::sources::excluded(path, &before.exclusions);
        let readable = |path: &str| unreadable.map(String::as_str) != Some(dir(path)) && !out(path);
        // Every known occurrence under an unreadable directory keeps its state.
        let retained = before
            .records
            .iter()
            .filter(|(_, r)| !readable(&r.path) && !out(&r.path))
            .all(|(id, r)| after.records.get(id) == Some(r));
        // Exclusion narrows scope: excluded records are out of scope (not
        // proven absent) and no excluded observation is cataloged.
        let scoped = excluded
            .iter()
            .all(|id| out(&before.records[id].path) && !unavailable.contains(id))
            && before
                .records
                .iter()
                .filter(|(_, r)| out(&r.path))
                .all(|(id, _)| excluded.contains(id) && !after.records[id].available);
        // Absence is inferred only for paths the attempt proved missing.
        let absence = unavailable.iter().all(|id| {
            let r = &before.records[id];
            !before.fs.contains_key(&r.path) && coverage.covers(&r.path)
        });
        // Every readable file is cataloged exactly once at its path, current.
        let truthful = before
            .fs
            .iter()
            .filter(|(p, _)| readable(p))
            .all(|(path, revision)| {
                after
                    .records
                    .values()
                    .filter(|r| r.available && &r.path == path)
                    .map(|r| &r.revision)
                    .eq([revision])
            });
        // Covered records that vanished are unavailable afterwards.
        let complete = before.records.iter().all(|(id, r)| {
            before.fs.contains_key(&r.path)
                || !coverage.covers(&r.path)
                || !after.records[id].available
                || found.iter().any(|f| f.path == after.records[id].path)
        });
        // Verified content held by one edition never creates a new work.
        let mut holders: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for r in before.records.values().filter(|r| !out(&r.path)) {
            holders.entry(&r.revision).or_default().insert(&r.edition);
        }
        let earlier_in_scope = |i: usize, revision: &str| {
            found[..i]
                .iter()
                .any(|g| g.revision == revision && !out(&g.path))
        };
        let classified = found
            .iter()
            .enumerate()
            .zip(assignments)
            .all(|((i, f), a)| match a {
                // New only for unknown content (first sighting) or ambiguous content.
                Assignment::OutOfScope => out(&f.path),
                Assignment::New => {
                    !out(&f.path)
                        && match holders.get(f.revision.as_str()) {
                            Some(e) => e.len() > 1,
                            None => !earlier_in_scope(i, &f.revision),
                        }
                }
                Assignment::CopyOf { observation } => {
                    !holders.contains_key(f.revision.as_str())
                        && *observation < i
                        && found[*observation].revision == f.revision
                }
                Assignment::Copy { edition } => {
                    holders
                        .get(f.revision.as_str())
                        .map(|e| e.iter().copied().collect::<Vec<_>>())
                        == Some(vec![edition.as_str()])
                }
                Assignment::Existing { id, .. } => before.records.contains_key(id),
            });
        let new_editions = assignments
            .iter()
            .filter(|a| matches!(a, Assignment::New))
            .count() as u32;
        Ok(vec![
            check("scan.unvisited_retain_state", retained),
            check("scan.exclusion_is_not_absence", scoped),
            check("scan.absence_requires_coverage", absence),
            check("scan.observed_files_cataloged", truthful),
            check("scan.covered_absence_recorded", complete),
            check("scan.copy_is_occurrence_not_work", classified),
            check(
                "scan.exact_effect",
                next.outputs.len() == 1
                    && assignments.len() == found.len()
                    && after.editions == before.editions + new_editions,
            ),
        ])
    }
}

fn dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}
fn observe(s: &State, unreadable: Option<&str>) -> (Vec<Observed>, Coverage) {
    let unreadable = unreadable.filter(|d| s.dirs.contains(*d));
    let found =
        s.fs.iter()
            .filter(|(p, _)| Some(dir(p)) != unreadable)
            .map(|(path, revision)| Observed {
                path: path.clone(),
                revision: revision.clone(),
            })
            .collect();
    let mut complete: BTreeSet<String> = [String::new()].into();
    let mut incomplete = BTreeSet::new();
    for d in DIRS {
        // An absent directory is proven absent by the complete root listing.
        if !s.dirs.contains(d) {
            continue;
        }
        if Some(d) == unreadable {
            incomplete.insert(d.to_string());
        } else {
            complete.insert(d.to_string());
        }
    }
    (
        found,
        Coverage::Listed {
            complete,
            incomplete,
        },
    )
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "observation invariant violated")
    }
}

impl Enumerate for Observation {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![Input::Scan { unreadable: None }];
        for d in DIRS {
            inputs.push(Input::Scan {
                unreadable: Some(d.into()),
            });
        }
        for d in DIRS {
            inputs.push(Input::RemoveDir(d.into()));
        }
        inputs.push(Input::Exclude(None));
        inputs.push(Input::Exclude(Some("d2".into())));
        for path in PATHS {
            inputs.push(Input::Remove { path: path.into() });
            for revision in ["r1", "r2"] {
                inputs.push(Input::Write {
                    path: path.into(),
                    revision: revision.into(),
                });
            }
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Observation {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Observation {
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
fn seeded_observation_sequences_check_production_reconciliation() {
    let report = stateless::explore::fuzz(
        &Observation,
        stateless::explore::FuzzConfig {
            seed: 20261009,
            cases: 2000,
            max_steps: 40,
            max_transitions: 200_000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    println!(
        "Stateless fuzz: {} cases, {} transitions, {:?}; 4 paths x 2 revisions, one unreadable directory per scan, d2 exclusion toggled",
        report.cases, report.transitions, report.termination
    );
}

#[test]
fn unreadable_directory_hides_neither_files_nor_proves_their_absence() {
    let mut state = Observation.initial_state().unwrap();
    for input in [
        Input::Scan { unreadable: None },
        Input::Remove {
            path: "d1/b.mkv".into(),
        },
        Input::Scan {
            unreadable: Some("d1".into()),
        },
    ] {
        state = Observation.step(&state, &input).unwrap().state;
    }
    let b = state
        .records
        .values()
        .find(|r| r.path == "d1/b.mkv")
        .unwrap();
    assert!(
        b.available,
        "absence in an unreadable directory is unproven"
    );
    for input in [
        Input::Write {
            path: "d2/b.mkv".into(),
            revision: "r1".into(),
        },
        Input::Scan { unreadable: None },
        Input::RemoveDir("d2".into()),
        Input::Scan { unreadable: None },
    ] {
        state = Observation.step(&state, &input).unwrap().state;
    }
    // d1 is listed (empty): proven absent. d2 vanished: its parent listing proves it.
    for path in ["d1/b.mkv", "d2/b.mkv"] {
        let r = state.records.values().find(|r| r.path == path).unwrap();
        assert!(!r.available, "{path}");
    }
}

#[test]
fn identical_files_first_seen_together_are_one_work() {
    let mut state = Observation.initial_state().unwrap();
    for input in [
        Input::Write {
            path: "d2/b.mkv".into(),
            revision: "r2".into(),
        },
        Input::Scan { unreadable: None },
    ] {
        state = Observation.step(&state, &input).unwrap().state;
    }
    assert_eq!(state.editions, 2, "a.mkv and one work for both r2 copies");
}

#[test]
fn backup_copy_joins_the_existing_edition() {
    let mut state = Observation.initial_state().unwrap();
    for input in [
        Input::Scan { unreadable: None },
        Input::Write {
            path: "d2/b.mkv".into(),
            revision: "r2".into(),
        },
        Input::Scan { unreadable: None },
    ] {
        state = Observation.step(&state, &input).unwrap().state;
    }
    let editions: BTreeSet<&str> = state
        .records
        .values()
        .filter(|r| r.revision == "r2")
        .map(|r| r.edition.as_str())
        .collect();
    assert_eq!(editions.len(), 1);
    assert_eq!(state.editions, 2);
}
