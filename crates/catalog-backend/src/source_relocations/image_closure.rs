//! Fixed complete94 ACK closure for an already authenticated current native image.
//!
//! The caller retains the exact97/96 pair and its Motion image allocation
//! preflight before and after this read. The native image owner independently
//! proves its current inventory, bootstrap transition and complete succession
//! history. This module matches EVERY native succession effect to EVERY Motion94
//! admission/command/ACK, then interprets the full91/94 chain of each affected
//! source. It makes no independent claim about untouched native source bindings.
//! No saved description becomes a receipt, requester authority or execution grant.
use super::history;
use crate::{source_lifecycle::LifecycleProof, sources::SourceMapping};
use anyhow::{Context, ensure};
use catabolic_store::migration::offline_bootstrap::CurrentCatalogImage;
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

// Export-only cohorts, matching the native image owner. Its additional 16,384
// aggregate seven-native-table row ceiling is established by that owner, not
// inferred from header counts here. These do not change mutation limits.
const MAX_EFFECTS: usize = 256;
const MAX_AFFECTED_SOURCES: usize = 64;
const JOURNALS: [&str; 3] = [
    "catabolic_source_relocation_admissions",
    "catabolic_source_relocation_commands",
    "catabolic_source_relocation_acks",
];

/// Descriptive commitment only. No consumer may use this as caller authority,
/// as a native receipt, or as permission to execute or acknowledge a command.
#[derive(Debug)]
pub(crate) struct CurrentImageClosure {
    sha256: String,
    effect_count: usize,
    source_count: usize,
}
impl CurrentImageClosure {
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
    pub(crate) fn effect_count(&self) -> usize {
        self.effect_count
    }
    pub(crate) fn source_count(&self) -> usize {
        self.source_count
    }
}
fn check(cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "current image source closure cancelled"
    );
    Ok(())
}
fn count_closure(counts: [i64; 3], native_count: usize) -> anyhow::Result<()> {
    ensure!(
        native_count <= MAX_EFFECTS && counts.into_iter().all(|count| count == native_count as i64),
        "complete94 admission/command/ACK counts differ from native effects"
    );
    Ok(())
}
/// Each table is counted independently, before fetching strings or allocating a
/// joined record. The sentinel refuses oversized journals without truncating a
/// successful closure. Inner joins cannot hide an orphan or a pending command.
async fn journal_counts_in(
    db: &mut SqliteConnection,
    cancel: &AtomicBool,
) -> anyhow::Result<[i64; 3]> {
    let mut counts = [0; 3];
    for (index, table) in JOURNALS.iter().enumerate() {
        check(cancel)?;
        counts[index] = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM (SELECT 1 FROM {table} LIMIT ?1)"
        ))
        .bind((MAX_EFFECTS + 1) as i64)
        .fetch_one(&mut *db)
        .await?;
    }
    Ok(counts)
}
fn text<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("native source effect {key} missing"))
}
// Borrow the already-owned opaque image's descriptions; do not copy its large
// permanent effect Values or manufacture a public Value-based image verifier.
type NativeGroups<'a> = BTreeMap<(&'a str, &'a str), BTreeMap<i64, (&'a str, &'a Value)>>;
fn group_effects<'a>(
    effects: &'a [Value],
    catalog_id: &str,
    incarnation: &str,
    generation: i64,
    cancel: &AtomicBool,
) -> anyhow::Result<NativeGroups<'a>> {
    ensure!(
        effects.len() <= MAX_EFFECTS,
        "native source effects exceed export cohort"
    );
    let mut operations = BTreeSet::new();
    let mut groups: NativeGroups<'_> = BTreeMap::new();
    for effect in effects {
        check(cancel)?;
        let operation = text(effect, "command_operation_id")?;
        ensure!(
            text(effect, "catalog_id")? == catalog_id
                && text(effect, "incarnation")? == incarnation
                && text(effect, "command_host_namespace")? == crate::HOST_NAMESPACE
                && effect["committed_generation"]
                    .as_i64()
                    .is_some_and(|committed| (0..=generation).contains(&committed))
                && operations.insert(operation),
            "native source effect identity or operation coverage differs"
        );
        let key = (text(effect, "profile")?, text(effect, "source_id")?);
        ensure!(
            groups.contains_key(&key) || groups.len() < MAX_AFFECTED_SOURCES,
            "native affected sources exceed export cohort"
        );
        let revision = effect["revision"]
            .as_i64()
            .filter(|revision| *revision > 0)
            .context("native source effect revision missing")?;
        ensure!(
            groups
                .entry(key)
                .or_default()
                .insert(revision, (operation, effect))
                .is_none(),
            "duplicate native source effect revision"
        );
    }
    Ok(groups)
}

/// Read the fixed complete94 closure under the caller's actual retained WRITE.
/// The opaque native image is mandatory; arbitrary JSON cannot enter here.
/// The caller independently verifies the exact97/96 pair before and after this
/// function and retains the same writer through capture/publication. Imported
/// baseline provenance remains the existing lifecycle reader's responsibility.
pub(crate) async fn verify_in(
    db: &mut SqliteConnection,
    image: &CurrentCatalogImage,
    cancel: &AtomicBool,
) -> anyhow::Result<CurrentImageClosure> {
    check(cancel)?;
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    count_closure(
        journal_counts_in(db, cancel).await?,
        image.source_effects().len(),
    )?;
    let groups = group_effects(
        image.source_effects(),
        image.catalog_id(),
        image.incarnation(),
        image.generation(),
        cancel,
    )?;
    let source_count = groups.len();
    let mut motion_sources = BTreeSet::new();
    let mut hop_commitments = Vec::with_capacity(image.source_effects().len());
    let mut current_commitments = Vec::with_capacity(source_count);
    for ((profile, native_source), hops) in groups {
        check(cancel)?;
        let mut latest: Option<LifecycleProof> = None;
        let mut expected_revision = 1i64;
        for (revision, (operation, effect)) in hops {
            check(cancel)?;
            ensure!(
                revision == expected_revision,
                "native source effect revision coverage differs"
            );
            expected_revision = expected_revision
                .checked_add(1)
                .context("native source revision exhausted")?;
            // This shared loader precharges the COMPLETE joined record before
            // fetching any retained strings and repeats its <=4MiB predicate.
            let record = history::record_in(db, operation).await?;
            let predecessor: LifecycleProof = history::motion_json(&record.predecessor_json)?;
            let old: SourceMapping = history::motion_json(&record.old_mapping_json)?;
            let successor: LifecycleProof = history::motion_json(&record.successor_json)?;
            ensure!(
                history::motion_json::<Value>(&record.native_effect_json)? == *effect,
                "Motion ACK differs from entire native permanent effect"
            );
            if let Some(prior) = &latest {
                ensure!(
                    predecessor == *prior,
                    "native effect order differs from Motion lifecycle order"
                );
            }
            // Count exact operation association independently of the history
            // join. The full chain reader below validates the actual row values.
            let linked: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM catabolic_source_lifecycle \
                 WHERE source_id=?1 AND epoch=?2 AND operation_id=?3 AND state='active'",
            )
            .bind(&successor.source_id)
            .bind(successor.epoch)
            .bind(operation)
            .fetch_one(&mut *db)
            .await?;
            ensure!(
                linked == 1,
                "native source effect has no exact lifecycle ACK"
            );
            let commitment = history::motion_hash(&json!({
                "operation_id": operation,
                "admission_digest": record.admission_digest,
                "command_digest": record.command_digest,
                "native_effect_digest": record.native_effect_digest,
                "predecessor": predecessor,
                "successor": successor,
            }))?;
            // Reuse all existing canonical admission, exact saved envelope,
            // historical requester, native effect/result and deterministic transition
            // semantics. No earlier hop is incorrectly required to be current.
            history::validate(operation, &predecessor, &old, &successor, record)?;
            hop_commitments.push(commitment);
            latest = Some(successor);
        }
        let latest = latest.context("native source history is empty")?;
        ensure!(
            motion_sources.insert(latest.source_id.clone()),
            "one Motion source names multiple native source prefixes"
        );
        check(cancel)?;
        let (current, mapping) =
            crate::source_lifecycle::current_with_mapping_in(db, &latest.source_id).await?;
        // Withdrawn/tombstoned heads retain their mapping. Compare the terminal
        // mapping, not the entire last active proof, so valid91 withdrawals are
        // preserved without requiring every historical94 mapping to be current.
        ensure!(
            current.source_id == latest.source_id
                && current.catalog_id == image.catalog_id()
                && current.incarnation == image.incarnation()
                && current.mapping_digest == latest.mapping_digest
                && current.binding_revision == latest.binding_revision
                && current.epoch >= latest.epoch
                && mapping.library_id == latest.source_id
                && mapping.profile == profile
                && mapping.native_source == native_source
                && crate::source_lifecycle::mapping_digest(&mapping)? == current.mapping_digest,
            "current Motion mapping differs from terminal native source effect"
        );
        current_commitments.push(history::motion_hash(&json!({
            "profile": profile,
            "native_source": native_source,
            "current": current,
        }))?);
    }
    check(cancel)?;
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    let sha256 = history::motion_hash(&json!({
        "contract_version": 1,
        "kind": "motion94-current-image-source-closure",
        "catalog_id": image.catalog_id(),
        "incarnation": image.incarnation(),
        "generation": image.generation(),
        "current_inventory_sha256": image.current_inventory_sha256(),
        "transition_digest": image.transition_digest(),
        "effect_count": image.source_effects().len(),
        "source_count": source_count,
        "hops": hop_commitments,
        "current_sources": current_commitments,
    }))?;
    Ok(CurrentImageClosure {
        sha256,
        effect_count: image.source_effects().len(),
        source_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure cohort checks, not native-image authentication. These descriptions
    // cannot construct CurrentCatalogImage or exercise a successful public ACK.
    fn descriptive_effect(operation: &str, source: &str, revision: i64) -> Value {
        json!({
            "catalog_id": "catalog",
            "incarnation": "incarnation",
            "command_host_namespace": crate::HOST_NAMESPACE,
            "command_operation_id": operation,
            "committed_generation": 7,
            "profile": "default",
            "source_id": source,
            "revision": revision,
        })
    }
    #[test]
    fn independent_counts_refuse_missing_extra_pending_and_sentinel_rows() {
        assert!(count_closure([0, 0, 0], 0).is_ok());
        assert!(count_closure([256, 256, 256], 256).is_ok());
        for counts in [[2, 2, 1], [2, 1, 2], [1, 2, 2], [3, 2, 2], [257; 3]] {
            assert!(count_closure(counts, 2).is_err());
        }
        assert!(count_closure([257; 3], 257).is_err());
    }
    #[test]
    fn borrowed_grouping_refuses_duplicate_operations_revisions_and_foreign_identity() {
        let cancel = AtomicBool::new(false);
        let first = descriptive_effect("first", "source", 1);
        let second = descriptive_effect("second", "source", 2);
        let values = [first.clone(), second];
        let grouped = group_effects(&values, "catalog", "incarnation", 7, &cancel).unwrap();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[&("default", "source")].len(), 2);
        assert!(group_effects(&values, "other", "incarnation", 7, &cancel).is_err());
        assert!(group_effects(&values, "catalog", "other", 7, &cancel).is_err());
        assert!(group_effects(&values, "catalog", "incarnation", 6, &cancel).is_err());
        for duplicate in [
            descriptive_effect("first", "another-source", 1),
            descriptive_effect("different-operation", "source", 1),
        ] {
            assert!(
                group_effects(
                    &[first.clone(), duplicate],
                    "catalog",
                    "incarnation",
                    7,
                    &cancel
                )
                .is_err()
            );
        }
        cancel.store(true, Ordering::Release);
        assert!(group_effects(&values, "catalog", "incarnation", 7, &cancel).is_err());
    }
    #[test]
    fn borrowed_grouping_refuses_export_cohort_overflow() {
        let cancel = AtomicBool::new(false);
        let sources: Vec<_> = (0..=MAX_AFFECTED_SOURCES)
            .map(|index| descriptive_effect(&format!("op-{index}"), &format!("source-{index}"), 1))
            .collect();
        assert!(
            group_effects(
                &sources[..MAX_AFFECTED_SOURCES],
                "catalog",
                "incarnation",
                7,
                &cancel
            )
            .is_ok()
        );
        assert!(group_effects(&sources, "catalog", "incarnation", 7, &cancel).is_err());
        let records: Vec<_> = (0..=MAX_EFFECTS)
            .map(|index| descriptive_effect(&format!("op-{index}"), "source", index as i64 + 1))
            .collect();
        assert!(
            group_effects(
                &records[..MAX_EFFECTS],
                "catalog",
                "incarnation",
                7,
                &cancel
            )
            .is_ok()
        );
        assert!(group_effects(&records, "catalog", "incarnation", 7, &cancel).is_err());
    }
    #[tokio::test]
    async fn journal_preflight_counts_orphans_without_a_join() {
        // Minimal SQL fixture exercises only count preflight, not native image
        // or schema admission. No success effect, receipt or ACK is fabricated.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut db = pool.acquire().await.unwrap();
        for table in JOURNALS {
            sqlx::query(&format!("CREATE TABLE {table}(operation_id TEXT)"))
                .execute(&mut *db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO catabolic_source_relocation_commands VALUES('orphan')")
            .execute(&mut *db)
            .await
            .unwrap();
        let counts = journal_counts_in(&mut db, &AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(counts, [0, 1, 0]);
        assert!(count_closure(counts, 0).is_err());
    }
}
