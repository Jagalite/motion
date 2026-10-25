//! Two migration IDs, all expected revisions/epochs 0..=5, invalid identities,
//! duplicate events and stale events. The production transition is the model.
use playscale_core::catalog_source::{Error, Input, Mode, State};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
struct Freeze;
fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(name)
    } else {
        Check::failed(name, "source freeze invariant")
    }
}
impl Model for Freeze {
    type State = State;
    type Input = Input;
    type Output = Result<bool, Error>;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.catalog-source-freeze".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/catalog_source.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State::default())
    }
    fn step(
        &self,
        state: &State,
        input: &Input,
    ) -> Result<Transition<State, Self::Output>, ModelError> {
        Ok(match state.transition(input) {
            Ok(next) => {
                let changed = next != *state;
                Transition::accepted(next, vec![Ok(changed)])
            }
            Err(e) => Transition::accepted(state.clone(), vec![Err(e)]),
        })
    }
    fn check_state(&self, state: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            check("valid", state.valid()),
            check(
                "bounded_history",
                state.revision <= 4 && state.used_migrations.len() <= 2,
            ),
            check(
                "writes_only_live",
                state.writes_allowed() == matches!(state.mode, Mode::Live),
            ),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Self::Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let (permitted, replay) = match input {
            Input::Freeze {
                migration_id,
                expected_revision,
            } => {
                let same = matches!(&before.mode,Mode::Frozen{migration_id:id} if id==migration_id)
                    && before.used_migrations.get(migration_id)
                        == expected_revision.checked_add(1).as_ref();
                (
                    same || (!migration_id.is_empty()
                        && matches!(before.mode, Mode::Live)
                        && *expected_revision == before.revision
                        && !before.used_migrations.contains_key(migration_id)),
                    same,
                )
            }
            Input::Abandon { freeze_revision } => (
                matches!(before.mode, Mode::Frozen { .. })
                    && before
                        .active_freeze()
                        .is_some_and(|(_, r)| r == *freeze_revision),
                false,
            ),
            Input::Seal { freeze_revision } => {
                let valid = before
                    .active_freeze()
                    .is_some_and(|(_, r)| r == *freeze_revision);
                (valid, valid && matches!(before.mode, Mode::Sealed { .. }))
            }
        };
        let actual = next.outputs.first().copied();
        let exact_admission = if permitted {
            actual == Some(Ok(!replay))
        } else {
            matches!(actual, Some(Err(_)))
        };
        let target = if !permitted || replay {
            next.state == before
        } else {
            match input {
                Input::Freeze { migration_id, .. } => {
                    matches!(&next.state.mode,Mode::Frozen{migration_id:id} if id==migration_id)
                        && next.state.used_migrations.get(migration_id)
                            == Some(&(before.revision + 1))
                }
                Input::Abandon { .. } => matches!(next.state.mode, Mode::Live),
                Input::Seal { .. } => {
                    matches!(next.state.mode, Mode::Sealed { .. })
                        && next.state.active_freeze() == before.active_freeze()
                }
            }
        };
        Ok(vec![
            check(
                "exact_admission_and_effect_count",
                exact_admission && next.outputs.len() == 1,
            ),
            check("exact_target", target),
            check(
                "revision_only_on_change",
                next.state.revision == before.revision + u64::from(permitted && !replay),
            ),
            check(
                "identity_never_rebound",
                before
                    .used_migrations
                    .iter()
                    .all(|(id, r)| next.state.used_migrations.get(id) == Some(r)),
            ),
            check(
                "sealed_never_reopens",
                !matches!(before.mode, Mode::Sealed { .. }) || next.state == before,
            ),
        ])
    }
}
impl Enumerate for Freeze {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut out = vec![];
        for revision in 0..=5 {
            for id in ["", "a", "b"] {
                out.push(Input::Freeze {
                    migration_id: id.into(),
                    expected_revision: revision,
                });
            }
            out.push(Input::Abandon {
                freeze_revision: revision,
            });
            out.push(Input::Seal {
                freeze_revision: revision,
            });
        }
        Ok(out)
    }
}
#[test]
fn source_freeze_interleavings_use_production_core() {
    let report = stateless::explore::enumerate(&Freeze, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Source freeze: {} states, {} transitions; two migration IDs, epochs/revisions 0..5, stale/duplicate/invalid events; five transition properties",
        report.states, report.transitions
    );
}
#[test]
fn source_freeze_overflow_and_malformed_state_cannot_open_writes() {
    let state = State {
        revision: i64::MAX as u64,
        ..Default::default()
    };
    assert_eq!(
        state.transition(&Input::Freeze {
            migration_id: "a".into(),
            expected_revision: state.revision
        }),
        Err(Error::Exhausted)
    );
    let invalid = State {
        mode: Mode::Frozen {
            migration_id: "missing".into(),
        },
        ..Default::default()
    };
    assert_eq!(
        invalid.transition(&Input::Abandon { freeze_revision: 0 }),
        Err(Error::InvalidState)
    );
}
