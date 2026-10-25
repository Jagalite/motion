//! Effect-free occurrence/cursor policy for scan-only schedules.
//!
//! A claim is NOT an authorization grant. The adapter must reauthorize the
//! retained requester and prove the current complete library/source/lifecycle
//! scope under the same writer that admits the request and commits this cursor.
//! No saved schedule JSON can substitute for those current owner checks.
use crate::maintenance::{self, RunOutcome, Schedule};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub revision: u64,
    pub library_id: String,
    pub interval_seconds: i64,
    pub enabled: bool,
}

/// `due_at` identifies the occurrence. Capacity backoff changes only retry_at,
/// so a retry cannot accidentally allocate a different occurrence identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub definition_revision: u64,
    pub due_at: i64,
    pub retry_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    pub schedule_id: String,
    pub definition_revision: u64,
    pub due_at: i64,
}

/// Persist the full exact occurrence tuple, not a digest alone, as the unique
/// request/cursor transaction key. Keep its original row after later revisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub occurrence: Occurrence,
    pub library_id: String,
    pub expected_cursor: Cursor,
    next_due: i64,
    blocked_until: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Admitted,
    CapacityBlocked,
}

/// Exact scalar shape of the retained api_schedules row. Credential references
/// are private audit facts, never a serialized authorization grant or wire DTO.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedSchedule {
    pub id: String,
    pub revision: i64,
    pub library_id: String,
    pub task: String,
    pub interval_seconds: i64,
    pub enabled: bool,
    pub next_run: i64,
    pub requester_id: String,
    pub credential_hash: Option<String>,
    pub ingress_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdoptionDisposition {
    /// Still requires actual native library/identity proof before persistence.
    ScanCandidate {
        definition: Definition,
        cursor: Cursor,
    },
    /// Preserve and expose unsupported work, including its fanout cursor.
    PendingUnsupportedTask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedAdoption {
    pub original: RetainedSchedule,
    pub dispatch_after_item: Option<String>,
    pub disposition: AdoptionDisposition,
}

/// Pure translation only: never reads a database, checks a credential, advances
/// time, claims an occurrence, or decides that an existing native owner exists.
pub fn plan_adoption(
    original: RetainedSchedule,
    dispatch_after_item: Option<String>,
) -> Result<RetainedAdoption, &'static str> {
    let revision = u64::try_from(original.revision)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or("invalid_schedule_definition")?;
    if original.id.is_empty() || original.library_id.is_empty() || original.requester_id.is_empty()
    {
        return Err("invalid_schedule_definition");
    }
    if !(60..=31_536_000).contains(&original.interval_seconds) {
        return Err("invalid_interval");
    }
    let disposition = if original.task == "scan" {
        if dispatch_after_item.is_some() {
            return Err("unexpected_scan_dispatch_progress");
        }
        AdoptionDisposition::ScanCandidate {
            definition: Definition {
                id: original.id.clone(),
                revision,
                library_id: original.library_id.clone(),
                interval_seconds: original.interval_seconds,
                enabled: original.enabled,
            },
            cursor: Cursor {
                definition_revision: revision,
                due_at: original.next_run,
                retry_at: original.next_run,
            },
        }
    } else {
        AdoptionDisposition::PendingUnsupportedTask
    };
    Ok(RetainedAdoption {
        original,
        dispatch_after_item,
        disposition,
    })
}

impl Claim {
    /// Admitted is legal only alongside the exact durable request insertion (or
    /// matching existing occurrence receipt), in one writer transaction.
    /// An unknown commit outcome is reconciled by the occurrence, never by
    /// changing this key or advancing a cursor independently.
    pub fn cursor_after(&self, outcome: Outcome) -> Cursor {
        match outcome {
            Outcome::Admitted => Cursor {
                definition_revision: self.occurrence.definition_revision,
                due_at: self.next_due,
                retry_at: self.next_due,
            },
            Outcome::CapacityBlocked => Cursor {
                retry_at: self.blocked_until,
                ..self.expected_cursor.clone()
            },
        }
    }
}

pub fn claim(
    definition: &Definition,
    cursor: &Cursor,
    now: i64,
) -> Result<Option<Claim>, &'static str> {
    if definition.id.is_empty() || definition.library_id.is_empty() || definition.revision == 0 {
        return Err("invalid_schedule_definition");
    }
    if definition.revision != cursor.definition_revision {
        return Err("schedule_revision_conflict");
    }
    if cursor.retry_at < cursor.due_at {
        return Err("invalid_schedule_cursor");
    }
    let schedule = Schedule {
        next_run: cursor.due_at,
        interval: definition.interval_seconds,
    };
    // Preserve the established interval and overflow policy, even for disabled
    // definitions. Disabled state never authorizes an occurrence.
    let next = schedule.claim(now)?;
    if !definition.enabled || cursor.retry_at > now {
        return Ok(None);
    }
    let Some(next) = next else {
        return Ok(None);
    };
    Ok(Some(Claim {
        occurrence: Occurrence {
            schedule_id: definition.id.clone(),
            definition_revision: definition.revision,
            due_at: cursor.due_at,
        },
        library_id: definition.library_id.clone(),
        expected_cursor: cursor.clone(),
        next_due: next.next_run,
        blocked_until: maintenance::next_run(cursor.due_at, &next, now, RunOutcome::Blocked),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Definition, Cursor) {
        (
            Definition {
                id: "schedule-a".into(),
                revision: 7,
                library_id: "library-a".into(),
                interval_seconds: 600,
                enabled: true,
            },
            Cursor {
                definition_revision: 7,
                due_at: 100,
                retry_at: 100,
            },
        )
    }

    #[test]
    fn adoption_preserves_due_revision_actor_and_disabled_state_exactly() {
        let original = RetainedSchedule {
            id: "retained-id".into(),
            revision: 19,
            library_id: "retained-library".into(),
            task: "scan".into(),
            interval_seconds: 600,
            enabled: false,
            next_run: -100,
            requester_id: "retained-requester".into(),
            credential_hash: Some("retained-hash".into()),
            ingress_id: Some("retained-ingress".into()),
        };
        let plan = plan_adoption(original.clone(), None).unwrap();
        assert_eq!(plan.original, original);
        let AdoptionDisposition::ScanCandidate { definition, cursor } = plan.disposition else {
            panic!("scan candidate required")
        };
        assert_eq!(definition.id, original.id);
        assert_eq!(definition.library_id, original.library_id);
        assert_eq!(definition.revision, 19);
        assert!(!definition.enabled);
        assert_eq!(
            cursor,
            Cursor {
                definition_revision: 19,
                due_at: -100,
                retry_at: -100
            }
        );
        for progress in ["", "last-item"] {
            assert_eq!(
                plan_adoption(original.clone(), Some(progress.into())),
                Err("unexpected_scan_dispatch_progress")
            );
        }
        for task in ["metadata_refresh", "preview", "optimize", "future-task"] {
            let retained = RetainedSchedule {
                task: task.into(),
                ..original.clone()
            };
            let pending = plan_adoption(retained.clone(), Some("last-item".into())).unwrap();
            assert_eq!(pending.original, retained);
            assert_eq!(pending.dispatch_after_item.as_deref(), Some("last-item"));
            assert_eq!(
                pending.disposition,
                AdoptionDisposition::PendingUnsupportedTask
            );
        }
    }

    #[test]
    fn exact_occurrence_survives_backoff_and_serialized_restart() {
        let (definition, cursor) = fixture();
        let first = claim(&definition, &cursor, 100).unwrap().unwrap();
        let blocked = first.cursor_after(Outcome::CapacityBlocked);
        assert_eq!(blocked.due_at, 100);
        assert_eq!(blocked.retry_at, 400);
        assert!(claim(&definition, &blocked, 399).unwrap().is_none());
        let restored: Cursor =
            serde_json::from_str(&serde_json::to_string(&blocked).unwrap()).unwrap();
        let retry = claim(&definition, &restored, 400).unwrap().unwrap();
        assert_eq!(first.occurrence, retry.occurrence);
        assert_eq!(retry.expected_cursor, restored);
        let admitted = retry.cursor_after(Outcome::Admitted);
        assert_eq!(admitted.due_at, 1000);
        assert_eq!(admitted.retry_at, 1000);
        assert!(claim(&definition, &admitted, 400).unwrap().is_none());
        assert_ne!(
            claim(&definition, &admitted, 1000)
                .unwrap()
                .unwrap()
                .occurrence,
            first.occurrence
        );
    }

    #[test]
    fn definition_changes_cannot_reuse_stale_cursor_or_occurrence() {
        let (mut definition, cursor) = fixture();
        let original = claim(&definition, &cursor, 100).unwrap().unwrap();
        definition.revision += 1;
        definition.library_id = "library-b".into();
        assert_eq!(
            claim(&definition, &cursor, 100),
            Err("schedule_revision_conflict")
        );
        let replaced = Cursor {
            definition_revision: 8,
            ..cursor
        };
        let next = claim(&definition, &replaced, 100).unwrap().unwrap();
        assert_ne!(original.occurrence, next.occurrence);
        assert_eq!(next.library_id, "library-b");
    }

    #[test]
    fn disabled_future_invalid_and_exhausted_schedules_never_claim() {
        let (mut definition, mut cursor) = fixture();
        assert!(claim(&definition, &cursor, 99).unwrap().is_none());
        definition.enabled = false;
        assert!(claim(&definition, &cursor, 100).unwrap().is_none());
        definition.enabled = true;
        definition.interval_seconds = 59;
        assert_eq!(claim(&definition, &cursor, 100), Err("invalid_interval"));
        definition.interval_seconds = 60;
        cursor.retry_at = 99;
        assert_eq!(
            claim(&definition, &cursor, 100),
            Err("invalid_schedule_cursor")
        );
        cursor.due_at = i64::MAX;
        cursor.retry_at = i64::MAX;
        assert_eq!(
            claim(&definition, &cursor, i64::MAX),
            Err("schedule_exhausted")
        );
    }
}
