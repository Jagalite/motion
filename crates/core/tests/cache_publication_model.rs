//! Core publication identity and exact reference-release effects. File bytes,
//! rename/fsync and manifest replacement are modeled adapter acknowledgements;
//! the host store suite separately tests those real filesystem operations.
use playscale_core::offline::{CacheBlob, CacheEntryIdentity, CachePublication, cache_releases};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    entries: [Option<u8>; 2],
    blobs: [u8; 2],
    staged: Option<(u8, u8, bool)>,
    removed: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Stage(u8, u8, bool),
    Publish(u8, u8, bool),
    Remove(u8, bool),
    Corrupt(u8),
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Decision(CachePublication),
    Release(String),
}
fn entry(download: u8, blob: u8) -> CacheEntryIdentity {
    CacheEntryIdentity {
        download: download.to_string(),
        media: CacheBlob {
            digest: blob.to_string(),
            size: 7,
        },
        artwork: None,
        subtitles: None,
    }
}
fn manifest(s: &State) -> Vec<CacheEntryIdentity> {
    s.entries
        .iter()
        .enumerate()
        .filter_map(|(d, b)| b.map(|b| entry(d as u8, b)))
        .collect()
}
struct Cache;
impl Model for Cache {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.cache.publication".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/offline.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            entries: [None; 2],
            blobs: [0; 2],
            staged: None,
            removed: 0,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut n = s.clone();
        let mut effects = vec![];
        match *i {
            Input::Stage(d, b, valid) => n.staged = Some((d, b, valid)),
            Input::Corrupt(b) => n.blobs[b as usize] = 1,
            Input::Remove(d, committed) => {
                n.removed |= 1 << d;
                if committed {
                    n.entries[d as usize] = None;
                    effects.extend(
                        cache_releases(&manifest(s), &manifest(&n))
                            .into_iter()
                            .map(Effect::Release),
                    );
                }
            }
            Input::Publish(d, b, committed) => {
                let staged = s.staged.filter(|(download, _, _)| *download == d);
                let removals = (0..2)
                    .filter(|d| s.removed & (1 << d) != 0)
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>();
                let decision = entry(d, b).publication(
                    &manifest(s),
                    &removals,
                    staged.is_some(),
                    s.blobs[b as usize] != 0,
                    true,
                );
                effects.push(Effect::Decision(decision));
                match decision {
                    CachePublication::AlreadyPublished => n.staged = None,
                    CachePublication::Publish | CachePublication::VerifyRecovery => {
                        // Observed verification effect, not an assumption that existence proves
                        // integrity. A successful rename can precede a failed manifest commit.
                        let verified = staged
                            .map(|(_, blob, valid)| blob == b && valid)
                            .unwrap_or(s.blobs[b as usize] == 2);
                        if verified {
                            n.blobs[b as usize] = 2;
                            n.staged = None;
                            if committed {
                                n.entries[d as usize] = Some(b);
                                effects.extend(
                                    cache_releases(&manifest(s), &manifest(&n))
                                        .into_iter()
                                        .map(Effect::Release),
                                );
                            }
                        } else if staged.is_some() {
                            n.staged = None;
                        }
                    }
                    _ => {}
                }
            }
        }
        for effect in &effects {
            if let Effect::Release(blob) = effect {
                n.blobs[blob.parse::<usize>().unwrap()] = 0;
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
        let mut expected = vec![];
        if let Input::Publish(d, b, _) = *i {
            let decision = if s.removed & (1 << d) != 0 {
                CachePublication::Removed
            } else if s.entries[d as usize] == Some(b) {
                CachePublication::AlreadyPublished
            } else if s.staged.is_some_and(|(download, _, _)| download == d) {
                CachePublication::Publish
            } else if s.blobs[b as usize] != 0 {
                CachePublication::VerifyRecovery
            } else {
                CachePublication::RetryTransfer
            };
            expected.push(Effect::Decision(decision));
        }
        // Independent exact reference-set oracle, including shared ownership and
        // removal/commit failures. Unrelated orphan blobs must not be swept here.
        for b in 0..2 {
            if s.entries.contains(&Some(b)) && !n.state.entries.contains(&Some(b)) {
                expected.push(Effect::Release(b.to_string()));
            }
        }
        Ok(vec![if n.outputs == expected {
            Check::passed("exact_publication_and_release")
        } else {
            Check::failed(
                "exact_publication_and_release",
                "wrong identity or premature release",
            )
        }])
    }
}
impl Enumerate for Cache {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![];
        for d in 0..2 {
            for b in 0..2 {
                for result in [false, true] {
                    v.push(Input::Stage(d, b, result));
                    v.push(Input::Publish(d, b, result));
                }
            }
            for committed in [false, true] {
                v.push(Input::Remove(d, committed));
            }
            v.push(Input::Corrupt(d));
        }
        Ok(v)
    }
}
#[test]
fn publication_commit_failure_shared_ownership_and_corrupt_recovery() {
    let r = stateless::explore::enumerate(
        &Cache,
        stateless::explore::SearchConfig {
            max_states: 20000,
            max_transitions: 500000,
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
