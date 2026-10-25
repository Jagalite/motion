//! Portable pair description, never export, filesystem, or restore authority.
//!
//! A service must independently verify both copied databases and every digest
//! under its drain/owner custody before publishing this description. Restoring a
//! pair additionally requires native incarnation rotation and Motion reconciliation.
pub mod install;
pub mod publication;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIdentity {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: u64,
    pub schema_version: u32,
    pub profile: String,
}

/// Version one is deliberately database-only. It cannot claim that referenced
/// media, managed outputs, artwork, or credentials were included in the export.
/// Filenames are fixed by the format, not supplied as potentially unsafe paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairManifest {
    pub contract_version: u32,
    pub pair_id: String,
    pub native_identity: NativeIdentity,
    pub motion_schema_version: u32,
    pub motion_database: Artifact,
    pub native_database: Artifact,
    pub motion_migration_ledger_sha256: String,
    pub native_migration_ledger_sha256: String,
    /// Canonical complete Motion schema/typed-row inventory, including every
    /// control journal, pending command, attempt, cutover, upgrade and reservation.
    pub motion_control_journals_sha256: String,
    /// Canonical complete native schema/typed-row inventory, including every
    /// receipt/ownership row, not only successful or fully acknowledged receipts.
    pub native_control_journals_sha256: String,
    /// Canonical effective binding and append-only upgrade evidence in the copy.
    pub motion_binding_sha256: String,
}

pub const MOTION_DATABASE: &str = "motion.sqlite3";
pub const NATIVE_DATABASE: &str = "native.sqlite3";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    Contract,
    Identity,
    Schema,
    Digest,
    Inventory,
    PairMismatch,
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic())
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl PairManifest {
    /// Structural validation only. Caller supplies independently read copied-DB
    /// identities; passing manifest values back here is not verification.
    pub fn validate(
        &self,
        copied_native: &NativeIdentity,
        copied_motion_binding: &crate::catalog_backend::Identity,
    ) -> Result<(), Invalid> {
        if self.contract_version != 1 {
            return Err(Invalid::Contract);
        }
        if !identifier(&self.pair_id)
            || !identifier(&self.native_identity.catalog_id)
            || !identifier(&self.native_identity.incarnation)
            || !identifier(&self.native_identity.profile)
            || self.native_identity.generation > i64::MAX as u64
        {
            return Err(Invalid::Identity);
        }
        if self.native_identity.schema_version == 0 || self.motion_schema_version == 0 {
            return Err(Invalid::Schema);
        }
        // Durable Motion binding does not assert a catalog generation/profile.
        // Disposable projection heads may lag receipt-bearing native mutations.
        if &self.native_identity != copied_native
            || self.native_identity.catalog_id != copied_motion_binding.catalog_id
            || self.native_identity.incarnation != copied_motion_binding.incarnation
            || self.native_identity.schema_version != copied_motion_binding.schema_version
        {
            return Err(Invalid::PairMismatch);
        }
        for hash in [
            &self.motion_database.sha256,
            &self.native_database.sha256,
            &self.motion_migration_ledger_sha256,
            &self.native_migration_ledger_sha256,
            &self.motion_control_journals_sha256,
            &self.native_control_journals_sha256,
            &self.motion_binding_sha256,
        ] {
            if !digest(hash) {
                return Err(Invalid::Digest);
            }
        }
        if self.motion_database.bytes == 0
            || self.native_database.bytes == 0
            || self
                .motion_database
                .bytes
                .checked_add(self.native_database.bytes)
                .is_none()
        {
            return Err(Invalid::Inventory);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> PairManifest {
        let hash = "a".repeat(64);
        PairManifest {
            contract_version: 1,
            pair_id: "pair-1".into(),
            native_identity: NativeIdentity {
                catalog_id: "catalog".into(),
                incarnation: "incarnation".into(),
                generation: 7,
                schema_version: 40,
                profile: "motion".into(),
            },
            motion_schema_version: 92,
            motion_database: Artifact {
                bytes: 4096,
                sha256: hash.clone(),
            },
            native_database: Artifact {
                bytes: 8192,
                sha256: hash.clone(),
            },
            motion_migration_ledger_sha256: hash.clone(),
            native_migration_ledger_sha256: hash.clone(),
            motion_control_journals_sha256: hash.clone(),
            native_control_journals_sha256: hash.clone(),
            motion_binding_sha256: hash,
        }
    }
    fn binding(id: &NativeIdentity) -> crate::catalog_backend::Identity {
        crate::catalog_backend::Identity {
            catalog_id: id.catalog_id.clone(),
            incarnation: id.incarnation.clone(),
            schema_version: id.schema_version,
        }
    }
    #[test]
    fn exact_pair_and_bounded_inventory() {
        let mut m = manifest();
        let identity = m.native_identity.clone();
        assert_eq!(m.validate(&identity, &binding(&identity)), Ok(()));
        m.native_database.bytes = u64::MAX;
        assert_eq!(
            m.validate(&identity, &binding(&identity)),
            Err(Invalid::Inventory)
        );
        m.native_database.bytes = 0;
        assert_eq!(
            m.validate(&identity, &binding(&identity)),
            Err(Invalid::Inventory)
        );
    }
    #[test]
    fn mixed_identity_generation_or_profile_never_validates() {
        let m = manifest();
        for field in 0..5 {
            let mut other = m.native_identity.clone();
            match field {
                0 => other.catalog_id.push('x'),
                1 => other.incarnation.push('x'),
                2 => other.generation += 1,
                3 => other.schema_version += 1,
                _ => other.profile.push('x'),
            }
            assert_eq!(
                m.validate(&other, &binding(&m.native_identity)),
                Err(Invalid::PairMismatch)
            );
            if field < 2 || field == 3 {
                assert_eq!(
                    m.validate(&m.native_identity, &binding(&other)),
                    Err(Invalid::PairMismatch)
                );
            } else {
                // No manufactured generation/profile assertion is added to the
                // frozen binding, even when a disposable cache is behind.
                assert_eq!(m.validate(&m.native_identity, &binding(&other)), Ok(()));
            }
        }
    }
    #[test]
    fn unknown_contract_bad_digest_and_extra_fields_refuse() {
        let mut m = manifest();
        let identity = m.native_identity.clone();
        m.motion_binding_sha256 = "A".repeat(64);
        assert_eq!(
            m.validate(&identity, &binding(&identity)),
            Err(Invalid::Digest)
        );
        m.contract_version = 2;
        assert_eq!(
            m.validate(&identity, &binding(&identity)),
            Err(Invalid::Contract)
        );
        let mut value = serde_json::to_value(manifest()).unwrap();
        value["restore_authorized"] = serde_json::json!(true);
        assert!(serde_json::from_value::<PairManifest>(value).is_err());
    }
}
