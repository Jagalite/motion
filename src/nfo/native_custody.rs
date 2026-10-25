//! Motion-side filesystem custody, not native mutation or byte authority.
//!
//! Preparation runs before entering the synchronous native command. The exclusive
//! borrow keeps the already acquired Motion writer alive and inaccessible until
//! this token is dropped. Terminal callbacks do no SQL and open no native session.
//! Callers still independently prove current curation (including expiry), complete
//! original-file selection, native occurrence/lineage, and prior-origin closure.
use super::{
    Sidecar,
    native_observation::{self, Observation},
};
use anyhow::ensure;
use motion_catalog_backend::{
    source_lifecycle,
    sources::{self, SourceMapping},
};
use sqlx::SqliteConnection;
use std::path::Path;

pub(crate) struct SelectedSidecar<'a> {
    pub mapping: &'a SourceMapping,
    pub media_relative: &'a Path,
}

pub(crate) struct RetainedSidecars<'a> {
    // Never expose the connection: doing so would allow policy changes while the
    // caller retained a token purporting to fence them.
    _writer: &'a mut SqliteConnection,
    observations: Vec<Observation>,
    lifecycles: Vec<source_lifecycle::LifecycleProof>,
}
pub(crate) struct AuthenticatedSidecars<'a> {
    custody: RetainedSidecars<'a>,
    authentication: crate::v2::auth::retained::AuthenticationSnapshot,
}
impl<'a> AuthenticatedSidecars<'a> {
    /// The supplied principal is the exact principal used for the independent
    /// complete-membership curation check, not an authority reconstructed here.
    pub(crate) async fn prepare_authorized_in(
        writer: &'a mut SqliteConnection,
        selected: &[SelectedSidecar<'_>],
        read_budget: &mut usize,
        caller: &crate::v2::auth::Caller,
        permission: playscale_core::access::Permission,
        curated_principal: &playscale_core::access::Principal,
    ) -> anyhow::Result<Self> {
        let authentication = crate::v2::auth::retained::AuthenticationSnapshot::prepare_in(
            writer,
            caller,
            permission,
            curated_principal,
        )
        .await
        .map_err(|error| anyhow::anyhow!("NFO authentication refused: {error:?}"))?;
        let custody = RetainedSidecars::prepare_in(writer, selected, read_budget).await?;
        let retained = Self {
            custody,
            authentication,
        };
        retained.revalidate()?;
        Ok(retained)
    }

    pub(crate) fn sidecars(&self) -> impl ExactSizeIterator<Item = &Sidecar> {
        self.custody.sidecars()
    }
    pub(crate) fn principal(&self) -> &playscale_core::access::Principal {
        self.authentication.principal()
    }
    pub(crate) fn lifecycles(&self) -> &[source_lifecycle::LifecycleProof] {
        self.custody.lifecycles()
    }
    pub(crate) fn revalidate(&self) -> anyhow::Result<()> {
        self.authentication
            .revalidate()
            .map_err(|error| anyhow::anyhow!("NFO authentication expired or changed: {error:?}"))?;
        self.custody.revalidate()?;
        // Filesystem revalidation may take time; check temporal admission again
        // before returning to the native terminal publication boundary.
        self.authentication
            .revalidate()
            .map_err(|error| anyhow::anyhow!("NFO authentication expired or changed: {error:?}"))?;
        Ok(())
    }
}
impl<'a> RetainedSidecars<'a> {
    pub(crate) async fn prepare_in(
        writer: &'a mut SqliteConnection,
        selected: &[SelectedSidecar<'_>],
        read_budget: &mut usize,
    ) -> anyhow::Result<Self> {
        ensure!(
            *read_budget <= super::SCAN_BUDGET,
            "NFO read budget exceeds existing scan bound"
        );
        ensure!(
            selected.len() <= catabolic_core::catalog_metadata::MAX_METADATA_JSON_NODES,
            "NFO sidecar selection exceeds owner node budget"
        );
        source_lifecycle::require_retained_writer_in(writer).await?;
        let mut observations = Vec::new();
        let mut lifecycles = Vec::new();
        for selected in selected {
            let mapping = selected.mapping;
            sources::check_mapping_in(writer, mapping).await?;
            let lifecycle =
                source_lifecycle::require_active_in(writer, &mapping.library_id).await?;
            ensure!(
                lifecycle.catalog_id == mapping.catalog.catalog_id
                    && lifecycle.incarnation == mapping.catalog.incarnation,
                "NFO source lifecycle belongs to another native catalog"
            );
            let observation = native_observation::observe(
                Path::new(&mapping.motion_root),
                selected.media_relative,
                &mapping.motion_root_identity,
                read_budget,
            )?;
            // No native writer is entered yet. SQL checks occur only here,
            // never from the synchronous terminal descriptor callback.
            sources::check_mapping_in(writer, mapping).await?;
            ensure!(
                source_lifecycle::require_active_in(writer, &mapping.library_id).await?
                    == lifecycle,
                "NFO source lifecycle changed during observation"
            );
            observations.push(observation);
            lifecycles.push(lifecycle);
        }
        source_lifecycle::require_retained_writer_in(writer).await?;
        let retained = Self {
            _writer: writer,
            observations,
            lifecycles,
        };
        retained.revalidate()?;
        Ok(retained)
    }

    pub(crate) fn sidecars(&self) -> impl ExactSizeIterator<Item = &Sidecar> {
        self.observations.iter().map(Observation::sidecar)
    }

    /// Descriptive facts checked while the exclusively borrowed writer fences
    /// lifecycle mutation. These do not establish scan enablement or curation.
    pub(crate) fn lifecycles(&self) -> &[source_lifecycle::LifecycleProof] {
        &self.lifecycles
    }

    pub(crate) fn revalidate(&self) -> anyhow::Result<()> {
        for observation in &self.observations {
            observation.revalidate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    async fn custody_requires_actual_writer_not_merely_open_transaction() {
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE fence(id INTEGER)")
            .execute(&mut db)
            .await
            .unwrap();
        let mut budget = super::super::SCAN_BUDGET;
        assert!(
            RetainedSidecars::prepare_in(&mut db, &[], &mut budget)
                .await
                .is_err()
        );
        sqlx::query("BEGIN DEFERRED")
            .execute(&mut db)
            .await
            .unwrap();
        assert!(
            RetainedSidecars::prepare_in(&mut db, &[], &mut budget)
                .await
                .is_err()
        );
        let _: i64 = sqlx::query_scalar("SELECT count(*) FROM fence")
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert!(
            RetainedSidecars::prepare_in(&mut db, &[], &mut budget)
                .await
                .is_err()
        );
        sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut db)
            .await
            .unwrap();
        let retained = RetainedSidecars::prepare_in(&mut db, &[], &mut budget)
            .await
            .unwrap();
        assert_eq!(retained.sidecars().len(), 0);
        assert!(retained.lifecycles().is_empty());
        retained.revalidate().unwrap();
        drop(retained);
        // Connection access and ACK/rollback become available only after drop.
        sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        assert!(
            RetainedSidecars::prepare_in(&mut db, &[], &mut budget)
                .await
                .is_err()
        );
    }
}
