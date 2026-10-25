//! Descriptive source-history resolution for the future compound bootstrap owner.
//! Neither saved envelopes nor this result mint absence, attribution or execution
//! authority. Native provenance owners still verify every exact raw effect.
pub mod imported_absence;
pub mod imported_origins;
mod offline_evidence;
use anyhow::ensure;
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use catabolic_runtime::host::HostSession;
use catabolic_store::catalog_nfo_attribution::inventory::{BootstrapInventory, ReceiptCommitment};
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

type Key = (String, String, String);
type BorrowedKey<'a> = (&'a str, &'a str, &'a str);
fn borrowed_key(value: &CommandKey) -> BorrowedKey<'_> {
    (
        value.catalog_id(),
        value.host_namespace(),
        value.operation_id(),
    )
}
fn key(value: &CommandKey) -> Key {
    (
        value.catalog_id().to_owned(),
        value.host_namespace().to_owned(),
        value.operation_id().to_owned(),
    )
}
pub struct SourceHistoryReferences {
    inventory: BootstrapInventory,
    envelopes: BTreeMap<Key, CommandEnvelope>,
    unresolved: Vec<(CommandKey, &'static str)>,
    // Complete trusted archive, when needed, remains available to the later
    // baseline classifier. This is not the full-result VerifiedArchive token.
    _archive: Option<crate::imported_sources::archive::PreparedArchive>,
}
impl SourceHistoryReferences {
    pub fn inventory(&self) -> &BootstrapInventory {
        &self.inventory
    }
    pub fn envelope(&self, command: &CommandKey) -> Option<&CommandEnvelope> {
        self.envelopes.get(&key(command))
    }
    pub fn unresolved(&self) -> &[(CommandKey, &'static str)] {
        &self.unresolved
    }
}
fn matches_commit(envelope: &CommandEnvelope, receipt: &ReceiptCommitment) -> anyhow::Result<bool> {
    let ReceiptCommitment::Retained {
        payload_digest,
        admission_incarnation,
        contract_version,
        expected_generation,
        committed_generation,
    } = receipt
    else {
        return Ok(false);
    };
    ensure!(
        envelope.digest() == payload_digest
            && envelope.admission_incarnation() == admission_incarnation
            && *contract_version == 1
            && envelope.expected_generation() == *expected_generation
            && committed_generation >= expected_generation,
        "saved NFO envelope differs from permanent native commitment"
    );
    Ok(true)
}
fn request_reference<'a>(
    requested: &mut BTreeMap<BorrowedKey<'a>, (&'a CommandKey, &'a ReceiptCommitment)>,
    key_bytes: &mut usize,
    command: &'a CommandKey,
    receipt: &'a ReceiptCommitment,
) -> anyhow::Result<()> {
    if let Some((_, prior)) = requested.get(&borrowed_key(command)) {
        ensure!(
            *prior == receipt,
            "contradictory duplicate native receipt commitment"
        );
        return Ok(());
    }
    let next = key_bytes
        .checked_add(command.catalog_id().len())
        .and_then(|n| n.checked_add(command.host_namespace().len()))
        .and_then(|n| n.checked_add(command.operation_id().len()))
        .ok_or_else(|| anyhow::anyhow!("NFO reference key budget overflow"))?;
    ensure!(
        requested.len() < 10000 && next <= 2 * 1024 * 1024,
        "NFO reference keys exceed bounded cohort"
    );
    requested.insert(borrowed_key(command), (command, receipt));
    *key_bytes = next;
    Ok(())
}
/// Caller retains both the real Motion writer and admitted HostSession. Exact
/// metadata source, tag replacement and merge references share one cumulative
/// deduplicated index. Resolving a merge envelope does not prove merge lineage.
/// Retirement descriptors currently lack receipt commitments and are not resolved
/// here: an empty `unresolved` list is not complete bootstrap
/// provenance for retirement or unsupported file/tag/history cohorts.
pub async fn source_history_in(
    db: &mut SqliteConnection,
    host: &mut HostSession,
    expected_generation: i64,
) -> anyhow::Result<SourceHistoryReferences> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    let inventory = host.nfo_bootstrap_inventory(expected_generation)?;
    let mut journal = crate::command_attempts::historical_index_in(db).await?;
    let mut requested: BTreeMap<BorrowedKey<'_>, (&CommandKey, &ReceiptCommitment)> =
        BTreeMap::new();
    let mut key_bytes = 0usize;
    for subject in inventory.subjects().values() {
        for (_, step) in subject.history() {
            request_reference(
                &mut requested,
                &mut key_bytes,
                step.command(),
                step.receipt(),
            )?;
        }
        for (_, step) in subject.tag_history() {
            request_reference(
                &mut requested,
                &mut key_bytes,
                step.command(),
                step.receipt(),
            )?;
        }
    }
    for step in inventory.tag_merges().values() {
        request_reference(
            &mut requested,
            &mut key_bytes,
            step.command(),
            step.receipt(),
        )?;
    }
    let mut archive = None;
    let mut archive_checked = false;
    let mut activations = BTreeMap::new();
    let mut envelopes = BTreeMap::new();
    let mut unresolved = Vec::new();
    let mut envelope_bytes = 0usize;
    for (_, (command, receipt)) in requested {
        // Keys are borrowed during complete inventory admission; allocate one
        // owned output lookup only after its cumulative key budget was checked.
        let identity = key(command);
        let found = journal.resolve_ref(command)?;
        let envelope = if let Some(found) = found {
            Some(found.matched)
        } else {
            // Absence of all archive controls is a descriptive missing-reference
            // cohort. Any claimed malformed/incomplete archive still errors.
            if !archive_checked {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_cutover UNION ALL SELECT 1 FROM catabolic_cutover_sets UNION ALL SELECT 1 FROM catabolic_cutover_parts)").fetch_one(&mut *db).await?;
                if exists {
                    let prepared = crate::imported_sources::archive::prepare_in(db).await?;
                    for (_, raw) in prepared.activation_envelopes() {
                        let saved = crate::decode_envelope(raw)?;
                        ensure!(
                            activations.insert(key(saved.key()), saved).is_none(),
                            "ambiguous saved activation command"
                        );
                    }
                    archive = Some(prepared);
                }
                archive_checked = true;
            }
            activations.get(&identity)
        };
        let Some(envelope) = envelope else {
            unresolved.push((command.clone(), "missing exact historical envelope"));
            continue;
        };
        if !matches_commit(envelope, receipt)? {
            unresolved.push((command.clone(), "missing permanent native receipt"));
            continue;
        }
        ensure!(
            envelope.key() == command && command.catalog_id() == inventory.catalog_id(),
            "foreign source-history command"
        );
        envelope_bytes = envelope_bytes
            .checked_add(envelope.canonical_payload().len())
            .ok_or_else(|| anyhow::anyhow!("NFO envelope budget overflow"))?;
        ensure!(
            envelope_bytes <= 16 * 1024 * 1024,
            "NFO resolved envelopes exceed bounded cohort"
        );
        // Exactly one retained copy per unique key, after cumulative admission.
        envelopes.insert(identity, envelope.clone());
    }
    ensure!(
        host.catalog_generation()? == expected_generation,
        "NFO inventory generation changed during resolution"
    );
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    Ok(SourceHistoryReferences {
        inventory,
        envelopes,
        unresolved,
        _archive: archive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope() -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new("catalog", "motion", "operation").unwrap(),
            "incarnation",
            7,
            r#"{"operation":"fixture"}"#,
        )
        .unwrap()
    }
    #[test]
    fn shared_history_keys_deduplicate_but_do_not_hide_commitment_conflicts() {
        let envelope = envelope();
        let receipt = ReceiptCommitment::Retained {
            payload_digest: envelope.digest().clone(),
            admission_incarnation: envelope.admission_incarnation().into(),
            contract_version: 1,
            expected_generation: 7,
            committed_generation: 9,
        };
        let missing = ReceiptCommitment::Missing;
        let mut requested = BTreeMap::new();
        let mut bytes = 0;
        // A compound source/tag effect shares its one original envelope; every
        // repeated commitment is still compared, regardless of reference family.
        request_reference(&mut requested, &mut bytes, envelope.key(), &receipt).unwrap();
        let admitted = bytes;
        request_reference(&mut requested, &mut bytes, envelope.key(), &receipt).unwrap();
        assert_eq!(requested.len(), 1);
        assert_eq!(bytes, admitted);
        assert!(request_reference(&mut requested, &mut bytes, envelope.key(), &missing).is_err());
        assert_eq!(requested.len(), 1);
        assert_eq!(bytes, admitted);
        let mut full = BTreeMap::new();
        let mut bytes = 2 * 1024 * 1024;
        assert!(request_reference(&mut full, &mut bytes, envelope.key(), &receipt).is_err());
        assert!(full.is_empty());
        assert_eq!(bytes, 2 * 1024 * 1024);
    }
    #[test]
    fn permanent_commitment_matches_exact_saved_envelope_not_current_json() {
        let envelope = envelope();
        let receipt = |generation, incarnation: &str| ReceiptCommitment::Retained {
            payload_digest: envelope.digest().clone(),
            admission_incarnation: incarnation.into(),
            contract_version: 1,
            expected_generation: generation,
            committed_generation: 9,
        };
        assert!(matches_commit(&envelope, &receipt(7, "incarnation")).unwrap());
        assert!(matches_commit(&envelope, &receipt(8, "incarnation")).is_err());
        assert!(matches_commit(&envelope, &receipt(7, "foreign")).is_err());
        assert!(!matches_commit(&envelope, &ReceiptCommitment::Missing).unwrap());
        let changed = CommandEnvelope::new(
            1,
            envelope.key().clone(),
            "incarnation",
            7,
            r#"{"operation":"changed"}"#,
        )
        .unwrap();
        assert!(matches_commit(&changed, &receipt(7, "incarnation")).is_err());
    }
}
