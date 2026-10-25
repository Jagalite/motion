//! Durable source-write admission during catalog migration. The adapter commits
//! each state and its guard installation atomically before acknowledging it.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub revision: u64,
    pub mode: Mode,
    pub used_migrations: BTreeMap<String, u64>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mode {
    #[default]
    Live,
    Frozen {
        migration_id: String,
    },
    Sealed {
        migration_id: String,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Freeze {
        migration_id: String,
        expected_revision: u64,
    },
    Abandon {
        freeze_revision: u64,
    },
    Seal {
        freeze_revision: u64,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Error {
    InvalidState,
    InvalidIdentity,
    RevisionConflict,
    IdentityReused,
    NotFrozen,
    Sealed,
    Exhausted,
}
impl State {
    pub fn writes_allowed(&self) -> bool {
        matches!(self.mode, Mode::Live)
    }
    pub fn active_freeze(&self) -> Option<(&str, u64)> {
        match &self.mode {
            Mode::Live => None,
            Mode::Frozen { migration_id } | Mode::Sealed { migration_id } => self
                .used_migrations
                .get(migration_id)
                .map(|r| (migration_id.as_str(), *r)),
        }
    }
    pub fn valid(&self) -> bool {
        self.revision <= i64::MAX as u64
            && self.used_migrations.len() <= 1024
            && self
                .used_migrations
                .iter()
                .all(|(id, r)| valid_id(id) && *r > 0 && *r <= self.revision)
            && self
                .used_migrations
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.used_migrations.len()
            && (matches!(self.mode, Mode::Live) || self.active_freeze().is_some())
    }
    pub fn transition(&self, input: &Input) -> Result<Self, Error> {
        if !self.valid() {
            return Err(Error::InvalidState);
        }
        let mut next = self.clone();
        match input {
            Input::Freeze {
                migration_id,
                expected_revision,
            } => {
                if !valid_id(migration_id) {
                    return Err(Error::InvalidIdentity);
                }
                if matches!(&self.mode,Mode::Frozen{migration_id:id} if id==migration_id)
                    && expected_revision.checked_add(1)
                        == self.used_migrations.get(migration_id).copied()
                {
                    return Ok(self.clone());
                }
                if *expected_revision != self.revision {
                    return Err(Error::RevisionConflict);
                }
                if !self.writes_allowed() {
                    return Err(Error::NotFrozen);
                }
                if self.used_migrations.contains_key(migration_id) {
                    return Err(Error::IdentityReused);
                }
                if self.used_migrations.len() == 1024 {
                    return Err(Error::Exhausted);
                }
                let revision = self
                    .revision
                    .checked_add(1)
                    .filter(|r| *r <= i64::MAX as u64)
                    .ok_or(Error::Exhausted)?;
                next.used_migrations.insert(migration_id.clone(), revision);
                next.mode = Mode::Frozen {
                    migration_id: migration_id.clone(),
                };
            }
            Input::Abandon { freeze_revision } => {
                if matches!(self.mode, Mode::Sealed { .. }) {
                    return Err(Error::Sealed);
                }
                if self.active_freeze().map(|(_, r)| r) != Some(*freeze_revision) {
                    return Err(Error::NotFrozen);
                }
                next.mode = Mode::Live;
            }
            Input::Seal { freeze_revision } => {
                let Some((id, revision)) = self.active_freeze() else {
                    return Err(Error::NotFrozen);
                };
                if revision != *freeze_revision {
                    return Err(Error::RevisionConflict);
                }
                if matches!(self.mode, Mode::Sealed { .. }) {
                    return Ok(self.clone());
                }
                next.mode = Mode::Sealed {
                    migration_id: id.into(),
                };
            }
        }
        next.revision = self
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(Error::Exhausted)?;
        Ok(next)
    }
}
fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
}
