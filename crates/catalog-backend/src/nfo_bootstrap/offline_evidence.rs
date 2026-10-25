//! Owned retained inputs for the offline bootstrap adapter. Descriptions do not
//! authorize native effects; the native owner reconstructs every proof afresh.
use super::{
    SourceHistoryReferences,
    imported_origins::{ArchivedOrigin, ImportedOriginsDescription},
};
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::CommandEnvelope;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 10000;
#[derive(Default)]
struct Copies {
    bytes: usize,
    entries: usize,
}
impl Copies {
    fn charge(&mut self, bytes: usize) -> anyhow::Result<()> {
        let next = self
            .bytes
            .checked_add(bytes)
            .and_then(|n| n.checked_add(256))
            .context("offline evidence copy budget overflow")?;
        ensure!(
            self.entries < MAX_ENTRIES && next <= MAX_BYTES,
            "offline evidence copies exceed bounded cohort"
        );
        self.bytes = next;
        self.entries += 1;
        Ok(())
    }
}

/// Owns stable buffers so borrowed native input views are created lexically in
/// the guarded execution task, never transported as self-referential grants.
pub(crate) struct ImportedBootstrapEvidence {
    origins: ImportedOriginsDescription,
    references: SourceHistoryReferences,
    activations: Vec<CommandEnvelope>,
    later: BTreeMap<String, Vec<CommandEnvelope>>,
    no_origin: BTreeSet<String>,
}
impl ImportedBootstrapEvidence {
    /// The owned buffers remain in the retained execution task for the entire
    /// native call. The result is a borrowed description, never a verified grant.
    pub(crate) fn views(
        &self,
    ) -> anyhow::Result<Vec<catabolic_store::migration::offline_bootstrap::HostSourceEvidence<'_>>>
    {
        use catabolic_store::migration::offline_bootstrap::{
            HostBaselineEvidence, HostImportAudit, HostImportOrigin, HostImportPart,
            HostSourceEvidence,
        };
        let archive = self.origins.archive();
        let part = |index: usize| -> anyhow::Result<HostImportPart<'_>> {
            let entry = archive
                .entries()
                .get(index)
                .context("offline part missing")?;
            let activation = self
                .activations
                .get(index)
                .context("offline Activate missing")?;
            Ok(HostImportPart {
                manifest: &entry.snapshot().manifest,
                batches: &entry.snapshot().batches,
                activation,
            })
        };
        let audit = HostImportAudit {
            root_manifest_digest: archive.root_manifest_digest(),
            origin_audit_digest: self.origins.digest(),
            no_origin_subjects: &self.no_origin,
        };
        let count = self.origins.subjects().len();
        ensure!(
            count <= MAX_ENTRIES
                && count
                    .checked_mul(std::mem::size_of::<HostSourceEvidence<'_>>())
                    .is_some_and(|bytes| bytes <= MAX_BYTES),
            "offline borrowed view storage exceeds bound"
        );
        let mut views = Vec::with_capacity(count);
        for (subject, description) in self.origins.subjects() {
            let origin = match description.origin() {
                ArchivedOrigin::AuditedAbsent => HostImportOrigin::NoOrigin,
                ArchivedOrigin::Physical {
                    physical_part,
                    motion_file_id,
                    physical,
                    ..
                } => HostImportOrigin::Physical {
                    part: part(*physical_part)?,
                    motion_file_id,
                    physical,
                },
            };
            views.push(HostSourceEvidence {
                baseline: HostBaselineEvidence::Imported {
                    subject,
                    metadata: part(description.metadata_part())?,
                    audit,
                    origin,
                },
                later_commands: self
                    .later
                    .get(subject)
                    .context("offline ordered history missing")?,
            });
        }
        Ok(views)
    }

    pub(crate) fn source_generation(&self) -> i64 {
        self.references.inventory().generation()
    }
    pub(crate) fn assemble(
        origins: ImportedOriginsDescription,
        references: SourceHistoryReferences,
    ) -> anyhow::Result<Self> {
        let archive = origins.archive();
        let inventory = references.inventory();
        ensure!(
            inventory.catalog_id() == archive.identity().catalog_id
                && inventory.incarnation() == archive.identity().incarnation,
            "offline origin and history identity differ"
        );
        ensure!(
            references.unresolved().is_empty(),
            "offline history has unresolved references"
        );
        // This initial adapter deliberately supports existing imported sources
        // only. Native-created absence needs its own positive host audit.
        ensure!(
            origins.subjects().len() == inventory.subjects().len()
                && inventory
                    .subjects()
                    .keys()
                    .all(|subject| origins.subjects().contains_key(subject)),
            "offline imported evidence does not cover complete subject inventory"
        );
        let mut budget = Copies::default();
        let mut activations = Vec::new();
        for (_, raw) in archive.activation_envelopes() {
            budget.charge(raw.len())?;
            activations.push(crate::decode_envelope(raw)?);
        }
        ensure!(
            activations.len() == archive.entries().len(),
            "offline activation parts incomplete"
        );
        let mut later = BTreeMap::new();
        let mut no_origin = BTreeSet::new();
        for (subject, origin) in origins.subjects() {
            let description = inventory
                .subjects()
                .get(subject)
                .context("offline subject missing")?;
            let head = description
                .source()
                .context("offline subject has no existing NFO source")?;
            let first = description
                .first_revision()
                .context("offline source history missing")?;
            let activation = activations
                .get(origin.metadata_part())
                .context("offline metadata part missing")?;
            ensure!(
                first.revision() == 1
                    && first.command() == activation.key()
                    && references
                        .envelope(first.command())
                        .is_some_and(|saved| saved == activation),
                "offline source baseline differs from exact activated part"
            );
            budget.charge(subject.len())?;
            let mut commands = Vec::new();
            let mut revision = 1i64;
            for (record, step) in description.history() {
                if record != "document" || step.revision() == 1 {
                    continue;
                }
                revision = revision
                    .checked_add(1)
                    .context("offline source revision overflow")?;
                ensure!(step.revision() == revision, "offline source history gap");
                let command = references
                    .envelope(step.command())
                    .context("offline later envelope missing")?;
                budget.charge(command.canonical_payload().len())?;
                commands.push(command.clone());
            }
            ensure!(
                revision == head.revision(),
                "offline source history does not reach current head"
            );
            later.insert(subject.clone(), commands);
            if matches!(origin.origin(), ArchivedOrigin::AuditedAbsent) {
                budget.charge(subject.len())?;
                no_origin.insert(subject.clone());
            }
        }
        Ok(Self {
            origins,
            references,
            activations,
            later,
            no_origin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_copy_admission_is_cumulative_and_checked() {
        let mut budget = Copies::default();
        budget.charge(MAX_BYTES - 256).unwrap();
        assert!(budget.charge(0).is_err());
        assert!(Copies::default().charge(usize::MAX).is_err());
        let mut count = Copies {
            bytes: 0,
            entries: MAX_ENTRIES,
        };
        assert!(count.charge(0).is_err());
    }
}
