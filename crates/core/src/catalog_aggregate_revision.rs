//! Monotonic exposed work revisions over complete, principal-independent native
//! facts. This is an issuance policy, not an authorization or completeness proof.
//! The adapter must obtain the *whole* dependency set from the retained owner,
//! under the Motion writer fence, and commit issuance before exposing its token.
//! Native metadata/source CAS remains a separate exact owner precondition.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Work,
    Edition,
    Timeline,
    Version,
    Binding,
    Occurrence,
    Source,
    LibraryDefinition,
    LibraryMembership,
    Relationship,
    Retirement,
}

/// content_digest covers the owner's canonical relevant facts, including
/// explicit absence/retirement/availability. It is never a filtered DTO digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub kind: DependencyKind,
    pub id: String,
    pub native_revision: Option<u64>,
    pub content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompleteWorkSnapshot {
    pub catalog_id: String,
    pub incarnation: String,
    pub work_id: String,
    pub native_generation: i64,
    /// Durable host capture order, assigned under the Motion writer only after
    /// a fresh complete retained-owner observation. Catalog generation alone
    /// does not necessarily order arbitrary selection membership facts. Cached/filtered reads cannot
    /// mint this watermark; the adapter persists it with issuance.
    pub observation_epoch: u64,
    /// Existing receipt-derived Motion/native work-head bridge. It must never
    /// be replaced by the aggregate token when admitting a native metadata edit.
    pub metadata_revision: u64,
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedRevision {
    pub contract_version: u32,
    pub catalog_id: String,
    pub incarnation: String,
    pub work_id: String,
    pub motion_baseline: u64,
    pub revision: u64,
    pub dependency_digest: String,
    pub native_generation: i64,
    pub observation_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    InvalidFacts,
    IdentityChanged,
    BaselineChanged,
    StaleSnapshot,
    InconsistentSnapshot,
    Exhausted,
}

fn nonempty(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}
fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Generation establishes freshness but is deliberately excluded from the
/// dependency digest. Saved-query preparation alone must not change item ETags.
pub fn dependency_digest(snapshot: &CompleteWorkSnapshot) -> Result<String, Refusal> {
    if !nonempty(&snapshot.catalog_id)
        || !nonempty(&snapshot.incarnation)
        || !nonempty(&snapshot.work_id)
        || snapshot.native_generation < 0
        || snapshot.observation_epoch == 0
        || snapshot.metadata_revision == 0
        || snapshot.dependencies.is_empty()
        || snapshot.dependencies.len() > 10_000
    {
        return Err(Refusal::InvalidFacts);
    }
    let mut facts = snapshot.dependencies.clone();
    facts.sort_by(|a, b| (a.kind, &a.id).cmp(&(b.kind, &b.id)));
    let mut work = 0;
    for (index, fact) in facts.iter().enumerate() {
        if !nonempty(&fact.id)
            || !sha256(&fact.content_digest)
            || fact.native_revision == Some(0)
            || index > 0 && (facts[index - 1].kind, &facts[index - 1].id) == (fact.kind, &fact.id)
        {
            return Err(Refusal::InvalidFacts);
        }
        if fact.kind == DependencyKind::Work {
            if fact.id != snapshot.work_id || fact.native_revision.is_none() {
                return Err(Refusal::InvalidFacts);
            }
            work += 1;
        }
    }
    if work != 1 {
        return Err(Refusal::InvalidFacts);
    }
    let canonical = serde_json::json!({"contract_version":1,"catalog_id":snapshot.catalog_id,
        "incarnation":snapshot.incarnation,"work_id":snapshot.work_id,
        "metadata_revision":snapshot.metadata_revision,"dependencies":facts});
    let bytes = serde_json::to_vec(&canonical).map_err(|_| Refusal::InvalidFacts)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(Refusal::InvalidFacts);
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// The adapter atomically persists this exact state/proof under its writer
/// reservation. A missing disposable cache never permits resetting prior state.
pub fn issue(
    prior: Option<&IssuedRevision>,
    motion_baseline: u64,
    snapshot: &CompleteWorkSnapshot,
) -> Result<IssuedRevision, Refusal> {
    let digest = dependency_digest(snapshot)?;
    if motion_baseline == 0 || snapshot.metadata_revision < motion_baseline {
        return Err(Refusal::InvalidFacts);
    }
    let revision = if let Some(prior) = prior {
        if prior.contract_version != 1
            || prior.catalog_id != snapshot.catalog_id
            || prior.incarnation != snapshot.incarnation
            || prior.work_id != snapshot.work_id
        {
            return Err(Refusal::IdentityChanged);
        }
        if prior.motion_baseline != motion_baseline {
            return Err(Refusal::BaselineChanged);
        }
        if prior.revision < prior.motion_baseline
            || !sha256(&prior.dependency_digest)
            || prior.native_generation < 0
            || prior.observation_epoch == 0
        {
            return Err(Refusal::InvalidFacts);
        }
        if snapshot.native_generation < prior.native_generation
            || snapshot.observation_epoch < prior.observation_epoch
        {
            return Err(Refusal::StaleSnapshot);
        }
        if digest == prior.dependency_digest {
            if prior.revision < snapshot.metadata_revision {
                return Err(Refusal::InconsistentSnapshot);
            }
            prior.revision
        } else {
            if snapshot.observation_epoch == prior.observation_epoch {
                return Err(Refusal::InconsistentSnapshot);
            }
            prior
                .revision
                .checked_add(1)
                .ok_or(Refusal::Exhausted)?
                .max(snapshot.metadata_revision)
        }
    } else {
        snapshot.metadata_revision
    };
    Ok(IssuedRevision {
        contract_version: 1,
        catalog_id: snapshot.catalog_id.clone(),
        incarnation: snapshot.incarnation.clone(),
        work_id: snapshot.work_id.clone(),
        motion_baseline,
        revision,
        dependency_digest: digest,
        native_generation: snapshot.native_generation,
        observation_epoch: snapshot.observation_epoch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> CompleteWorkSnapshot {
        CompleteWorkSnapshot {
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            work_id: "work".into(),
            native_generation: 10,
            observation_epoch: 1,
            metadata_revision: 9,
            dependencies: vec![
                Dependency {
                    kind: DependencyKind::Work,
                    id: "work".into(),
                    native_revision: Some(1),
                    content_digest: "a".repeat(64),
                },
                Dependency {
                    kind: DependencyKind::Timeline,
                    id: "timeline".into(),
                    native_revision: Some(1),
                    content_digest: "b".repeat(64),
                },
            ],
        }
    }
    #[test]
    fn issuance_is_monotonic_dependency_bound_and_query_generation_independent() {
        let mut facts = snapshot();
        let first = issue(None, 9, &facts).unwrap();
        assert_eq!(first.revision, 9);
        facts.native_generation += 1;
        facts.observation_epoch += 1;
        facts.dependencies.reverse();
        let same = issue(Some(&first), 9, &facts).unwrap();
        assert_eq!(same.revision, 9);
        assert_eq!(same.dependency_digest, first.dependency_digest);
        // Arbitrary selection dependencies may change without catalog generation.
        facts.observation_epoch += 1;
        facts
            .dependencies
            .iter_mut()
            .find(|d| d.kind == DependencyKind::Timeline)
            .unwrap()
            .native_revision = Some(2);
        let child = issue(Some(&same), 9, &facts).unwrap();
        assert_eq!(child.revision, 10);
        facts.native_generation += 1;
        facts.observation_epoch += 1;
        facts.metadata_revision = 10;
        facts
            .dependencies
            .iter_mut()
            .find(|d| d.kind == DependencyKind::Work)
            .unwrap()
            .native_revision = Some(2);
        let edited = issue(Some(&child), 9, &facts).unwrap();
        assert_eq!(edited.revision, 11);
        assert_eq!(
            issue(Some(&edited), 9, &facts).unwrap(),
            edited,
            "restart/rebuild must replay issuance"
        );
        facts.native_generation += 1;
        facts.observation_epoch += 1;
        facts.metadata_revision = 30;
        assert_eq!(issue(Some(&edited), 9, &facts).unwrap().revision, 30);
    }
    #[test]
    fn stale_identity_incomplete_duplicate_and_same_watermark_changes_refuse() {
        let mut facts = snapshot();
        let first = issue(None, 9, &facts).unwrap();
        facts.native_generation -= 1;
        assert_eq!(issue(Some(&first), 9, &facts), Err(Refusal::StaleSnapshot));
        facts = snapshot();
        facts.dependencies[1].content_digest = "c".repeat(64);
        assert_eq!(
            issue(Some(&first), 9, &facts),
            Err(Refusal::InconsistentSnapshot)
        );
        facts = snapshot();
        facts.incarnation = "restored".into();
        assert_eq!(
            issue(Some(&first), 9, &facts),
            Err(Refusal::IdentityChanged)
        );
        facts = snapshot();
        assert_eq!(
            issue(Some(&first), 8, &facts),
            Err(Refusal::BaselineChanged)
        );
        facts.dependencies.push(facts.dependencies[0].clone());
        assert_eq!(dependency_digest(&facts), Err(Refusal::InvalidFacts));
        facts = snapshot();
        facts.dependencies.remove(0);
        assert_eq!(dependency_digest(&facts), Err(Refusal::InvalidFacts));
    }
    #[test]
    fn exhausted_revision_never_wraps_or_resets() {
        let mut facts = snapshot();
        let mut prior = issue(None, 9, &facts).unwrap();
        prior.revision = u64::MAX;
        facts.native_generation += 1;
        facts.observation_epoch += 1;
        facts.dependencies[1].native_revision = Some(2);
        assert_eq!(issue(Some(&prior), 9, &facts), Err(Refusal::Exhausted));
    }
}
