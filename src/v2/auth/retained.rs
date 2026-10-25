//! Re-evaluate existing temporal authentication rules without SQL from a native
//! writer callback. This snapshot is not a grant and is valid only while its
//! caller retains the same exclusive Motion writer used to prepare it.
use super::*;

pub(crate) struct AuthenticationSnapshot {
    principal: Principal,
    permission: Permission,
    facts: Facts,
    family: Option<super::super::signin::RetainedFamily>,
}
enum Facts {
    Credential {
        credential: Credential,
        parent: Option<Credential>,
        device: Option<Device>,
    },
    // These top-level branches have no credential wall-clock predicates.
    // Optional signin family time rules are retained separately for every
    // device-backed principal, including ingress. Database-dependent scope
    // was re-derived under the retained writer.
    NonTemporal,
}
impl AuthenticationSnapshot {
    pub(crate) async fn prepare_in(
        db: &mut sqlx::SqliteConnection,
        caller: &Caller,
        permission: Permission,
        expected_principal: &Principal,
    ) -> Result<Self, Problem> {
        motion_catalog_backend::source_lifecycle::require_retained_writer_in(db)
            .await
            .map_err(Problem::internal)?;
        let principal = caller.reauthorize(db, Some(permission)).await?;
        if principal != *expected_principal {
            return Err(AccessError::CredentialRevoked.into());
        }
        let family = match &principal.device_id {
            Some(id) => super::super::signin::retained_device_lineage(db, id).await?,
            None => None,
        };
        Self::capture_authenticated(db, caller, permission, expected_principal, family).await
    }

    // Decoder shared only by the existing WRITE owner and the two distinct
    // backend-minted, lifetime-bound restricted authorization readers below.
    async fn capture_authenticated(
        db: &mut sqlx::SqliteConnection,
        caller: &Caller,
        permission: Permission,
        expected_principal: &Principal,
        family: Option<super::super::signin::RetainedFamily>,
    ) -> Result<Self, Problem> {
        let principal = caller.reauthorize(db, Some(permission)).await?;
        if principal != *expected_principal {
            return Err(AccessError::CredentialRevoked.into());
        }
        let facts = match &caller.source {
            CallerSource::Credential { hash, admitted } => {
                let stored = load_credential(db, hash)
                    .await?
                    .ok_or(AccessError::CredentialRevoked)?;
                if stored.credential != *admitted {
                    return Err(AccessError::CredentialRevoked.into());
                }
                let parent = match &stored.parent_hash {
                    Some(hash) => load_credential(db, hash).await?.map(|row| row.credential),
                    None => None,
                };
                // load_device also validates current user ownership and lineage.
                let device = load_device(db, &stored.credential.device_id)
                    .await?
                    .map(|row| row.device);
                Facts::Credential {
                    credential: stored.credential,
                    parent,
                    device,
                }
            }
            CallerSource::LocalOperator
            | CallerSource::TrustedIngress { .. }
            | CallerSource::Household { .. } => Facts::NonTemporal,
        };
        let snapshot = Self {
            principal,
            permission,
            facts,
            family,
        };
        snapshot.revalidate()?;
        Ok(snapshot)
    }

    pub(crate) fn principal(&self) -> &Principal {
        &self.principal
    }

    pub(crate) fn revalidate(&self) -> Result<(), Problem> {
        self.revalidate_at(crate::now())
    }

    fn revalidate_at(&self, now: i64) -> Result<(), Problem> {
        if self
            .family
            .as_ref()
            .is_some_and(|family| !family.valid_at(now))
        {
            return Err(AccessError::CredentialRevoked.into());
        }
        let current = match &self.facts {
            Facts::Credential {
                credential,
                parent,
                device,
            } => authenticate_caller(
                Authentication::Credential {
                    credential: Some(credential),
                    parent: parent.as_ref(),
                    device: device.as_ref(),
                },
                now,
            )?,
            Facts::NonTemporal => self.principal.clone(),
        };
        if current != self.principal {
            return Err(AccessError::CredentialRevoked.into());
        }
        if !current.allows(self.permission) {
            return Err(AccessError::Forbidden(self.permission).into());
        }
        Ok(())
    }
}

/// Deterministic interruption inside real preparation, after the actual
/// restricted reader has been validated and before caller policy is decoded.
/// No fixture authentication facts or connection substitute are accepted.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct PreparationObservation {
    pub(crate) entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) gate: Option<std::sync::Arc<tokio::sync::Notify>>,
    pub(crate) fail: bool,
    pub(crate) panic: bool,
}
#[cfg(test)]
impl PreparationObservation {
    async fn reached(&self) -> Result<(), Problem> {
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        assert!(!self.panic, "actual authentication preparation unwind");
        if self.fail {
            return Err(Problem::internal(anyhow::anyhow!(
                "actual authentication preparation injected failure"
            )));
        }
        Ok(())
    }
}

/// Actual current source authentication under its retained unmodified writer.
/// The retained constructor's token borrows its backend slot, never its reader
/// connection or acquisition frame. Dropping that preparation future therefore
/// ends only the borrow. The explicitly named legacy constructor below preserves
/// the older consuming-reader limitation and cannot use retained retirement.
pub(crate) struct CurrentSourceAuthentication<'owner> {
    reader: CurrentReader<'owner>,
    snapshot: AuthenticationSnapshot,
}

/// Legacy consuming seams are deliberately distinct and remain unqualified for
/// failure-owned reader custody. They share authentication policy, never the
/// retained retirement/accounting capabilities used by new connected paths.
enum CurrentReader<'owner> {
    Retained(motion_catalog_backend::motion_snapshot::RetainedAuthorizationReader<'owner>),
    Legacy(motion_catalog_backend::motion_snapshot::LegacyAuthorizationReader<'owner>),
}
impl CurrentReader<'_> {
    async fn authorize_connection(&mut self) -> anyhow::Result<&mut sqlx::SqliteConnection> {
        match self {
            Self::Retained(reader) => reader.authorize_connection().await,
            Self::Legacy(reader) => reader.authorize_connection().await,
        }
    }
    async fn verify(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Retained(reader) => reader.verify().await,
            Self::Legacy(reader) => reader.verify().await,
        }
    }
    async fn lineage(
        &mut self,
        id: &str,
    ) -> Result<Option<super::super::signin::RetainedFamily>, Problem> {
        match self {
            Self::Retained(reader) => {
                super::super::signin::source_authorization_lineage(reader, id).await
            }
            Self::Legacy(reader) => {
                super::super::signin::legacy_source_authorization_lineage(reader, id).await
            }
        }
    }
}
impl<'owner> CurrentSourceAuthentication<'owner> {
    pub(crate) async fn prepare(
        reader: motion_catalog_backend::motion_snapshot::RetainedAuthorizationReader<'owner>,
        caller: &Caller,
        permission: Permission,
    ) -> Result<Self, Problem> {
        Self::prepare_inner(
            CurrentReader::Retained(reader),
            caller,
            permission,
            #[cfg(test)]
            None,
        )
        .await
    }
    /// Explicit legacy-only compatibility. This does not claim failure-owned
    /// reader acquisition and cannot retire into a retained workspace.
    pub(crate) async fn prepare_legacy(
        reader: motion_catalog_backend::motion_snapshot::LegacyAuthorizationReader<'owner>,
        caller: &Caller,
        permission: Permission,
    ) -> Result<Self, Problem> {
        Self::prepare_inner(
            CurrentReader::Legacy(reader),
            caller,
            permission,
            #[cfg(test)]
            None,
        )
        .await
    }
    #[cfg(test)]
    pub(crate) async fn prepare_observed(
        reader: motion_catalog_backend::motion_snapshot::RetainedAuthorizationReader<'owner>,
        caller: &Caller,
        permission: Permission,
        observation: &PreparationObservation,
    ) -> Result<Self, Problem> {
        Self::prepare_inner(
            CurrentReader::Retained(reader),
            caller,
            permission,
            Some(observation),
        )
        .await
    }
    async fn prepare_inner(
        mut reader: CurrentReader<'owner>,
        caller: &Caller,
        permission: Permission,
        #[cfg(test)] observation: Option<&PreparationObservation>,
    ) -> Result<Self, Problem> {
        let db = reader
            .authorize_connection()
            .await
            .map_err(Problem::internal)?;
        #[cfg(test)]
        if let Some(observation) = observation {
            observation.reached().await?;
        }
        let principal = caller.reauthorize(db, Some(permission)).await?;
        let family = match &principal.device_id {
            Some(id) => reader.lineage(id).await?,
            None => None,
        };
        let db = reader
            .authorize_connection()
            .await
            .map_err(Problem::internal)?;
        let snapshot = AuthenticationSnapshot::capture_authenticated(
            db, caller, permission, &principal, family,
        )
        .await?;
        reader.verify().await.map_err(Problem::internal)?;
        snapshot.revalidate()?;
        Ok(Self { reader, snapshot })
    }
    pub(crate) fn principal(&self) -> &Principal {
        self.snapshot.principal()
    }
    pub(crate) async fn revalidate(&mut self) -> Result<(), Problem> {
        self.reader.verify().await.map_err(Problem::internal)?;
        self.snapshot.revalidate()
    }
    /// Measure both retained source connections without releasing their owner.
    /// This is resource accounting, not an authorization or reservation grant.
    pub(crate) async fn retained_allocations(&mut self) -> Result<u64, Problem> {
        match &mut self.reader {
            CurrentReader::Retained(reader) => reader
                .retained_allocations()
                .await
                .map_err(Problem::internal),
            // Descriptive observation is not retained retirement authority.
            CurrentReader::Legacy(reader) => reader
                .retained_allocations()
                .await
                .map_err(Problem::internal),
        }
    }
    /// Recheck time after other retained owners have been verified. This does
    /// not replace `revalidate`: callers must retain this token and separately
    /// verify its unchanged owner before relying on this no-IO temporal check.
    pub(crate) fn revalidate_temporal(&self) -> Result<(), Problem> {
        self.snapshot.revalidate()
    }

    /// Transfer only the readonly reader's actual close future into an already
    /// admitted, independently retained workspace slot. The original writer is
    /// still borrowed here. No connection getter or auth fact is extracted.
    pub(crate) fn begin_retained_close(
        &mut self,
        slot: &mut motion_catalog_backend::motion_snapshot::RetainedAuthorizationCloseSlot,
    ) -> Result<(), Problem> {
        match &mut self.reader {
            CurrentReader::Retained(reader) => {
                reader.begin_retained_close(slot).map_err(Problem::internal)
            }
            CurrentReader::Legacy(_) => Err(Problem::internal(anyhow::anyhow!(
                "legacy source authentication cannot enter retained retirement"
            ))),
        }
    }

    /// Destroy these real authentication facts and consume their backend borrow
    /// before the exact source writer may acknowledge its joined close. This
    /// cannot accept a different/old joined slot, mint auth, or revive a token.
    pub(crate) fn finish_retained_close(
        self,
        slot: &mut motion_catalog_backend::motion_snapshot::RetainedAuthorizationCloseSlot,
    ) -> Result<(), Problem> {
        let Self { reader, snapshot } = self;
        drop(snapshot);
        match reader {
            CurrentReader::Retained(reader) => reader
                .retire_authorization_close(slot)
                .map_err(Problem::internal),
            CurrentReader::Legacy(_) => Err(Problem::internal(anyhow::anyhow!(
                "legacy source authentication cannot acknowledge retained retirement"
            ))),
        }
    }

    /// Standalone retained phases keep their real close in FreshWriter rather
    /// than a connected workspace. Destruction of the auth facts happens before
    /// creating a close waiter, and only the exact worker ACK releases its claim.
    pub(crate) fn retire_inline(
        self,
    ) -> impl std::future::Future<Output = Result<(), Problem>> + 'owner {
        let Self { reader, snapshot } = self;
        drop(snapshot);
        let close = match reader {
            CurrentReader::Retained(reader) => Ok(reader.retire_inline()),
            CurrentReader::Legacy(_) => Err(Problem::internal(anyhow::anyhow!(
                "legacy source authentication cannot acknowledge inline retained retirement"
            ))),
        };
        async move { close?.await.map_err(Problem::internal) }
    }
}

/// Source94's independently verified WRITE owner. This is deliberately not a
/// TargetAuthorizationReader: that token's contract is a restricted readonly
/// connection. No connection or credential facts escape this fixed adapter.
pub(crate) struct RelocationTargetAuthentication<'owner> {
    writer: motion_catalog_backend::RelocationTargetWriter<'owner>,
    snapshot: AuthenticationSnapshot,
}
impl<'owner> RelocationTargetAuthentication<'owner> {
    pub(crate) async fn prepare(
        mut writer: motion_catalog_backend::RelocationTargetWriter<'owner>,
        caller: &Caller,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, Problem> {
        let db = writer
            .authorize_connection(cancel)
            .await
            .map_err(Problem::internal)?;
        let principal = caller
            .reauthorize(db, Some(Permission::SourcesManage))
            .await?;
        let snapshot =
            AuthenticationSnapshot::prepare_in(db, caller, Permission::SourcesManage, &principal)
                .await?;
        writer.verify(cancel).await.map_err(Problem::internal)?;
        snapshot.revalidate()?;
        Ok(Self { writer, snapshot })
    }
    /// Derive the historical requester only from this current target Caller
    /// check. Bound actual capacities before any clone; this DTO is never a
    /// substitute for either retained authentication token.
    pub(crate) fn relocation_requester(
        &self,
        caller: &Caller,
        deadline: std::time::Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<motion_catalog_backend::SourceRelocationRequester> {
        let principal = self.snapshot.principal();
        let mut bytes = 0u64;
        let mut entries = 0usize;
        let mut charge = |capacity: usize, node: bool| -> anyhow::Result<()> {
            anyhow::ensure!(
                !cancel.load(std::sync::atomic::Ordering::Acquire)
                    && std::time::Instant::now() < deadline,
                "source94 requester measurement cancelled or expired"
            );
            entries += 1;
            anyhow::ensure!(
                entries <= 4096,
                "source94 requester traversal exceeded admission"
            );
            bytes = bytes
                .checked_add(capacity as u64)
                .and_then(|n| n.checked_add(if node { 512 } else { 0 }))
                .ok_or_else(|| anyhow::anyhow!("source94 requester storage overflow"))?;
            anyhow::ensure!(
                bytes <= Caller::RETAINED_STORAGE_LIMIT,
                "source94 requester exceeds named copied-caller share"
            );
            Ok(())
        };
        charge(principal.id.capacity(), false)?;
        if let Some(id) = &principal.device_id {
            charge(id.capacity(), false)?;
        }
        for set in [
            &principal.grant.profile_ids,
            &principal.policy.library_ids,
            &principal.policy.allowed_ratings,
            &principal.policy.blocked_labels,
        ] {
            anyhow::ensure!(
                set.len() <= 4096,
                "source94 requester set exceeds bounded traversal"
            );
            for value in set {
                charge(value.capacity(), true)?;
            }
        }
        for _ in &principal.grant.permissions {
            charge(0, true)?;
        }
        if let Some((hash, _)) = caller.credential() {
            charge(hash.len(), false)?;
        }
        if let Some(id) = caller.ingress_device_id() {
            charge(id.len(), false)?;
        }
        // Separate ID copy plus accepted principal coexist with the snapshot.
        charge(principal.id.capacity(), false)?;
        motion_catalog_backend::SourceRelocationRequester::from_current_host(
            principal.id.clone(),
            caller.credential().map(|(hash, _)| hash.to_owned()),
            caller.ingress_device_id().map(str::to_owned),
            principal.clone(),
        )
    }

    /// Refresh BOTH policy/lineage facts and exact owner checks after the last
    /// insertion/postcheck await. The original immutable source writer prevents
    /// source-row changes; its real reader still has to verify before each seam.
    pub(crate) async fn refresh(
        &mut self,
        caller: &Caller,
        current: &mut CurrentSourceAuthentication<'_>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.writer
            .verify(cancel)
            .await
            .map_err(Problem::internal)?;
        let db = self
            .writer
            .authorize_connection(cancel)
            .await
            .map_err(Problem::internal)?;
        self.snapshot = AuthenticationSnapshot::prepare_in(
            db,
            caller,
            Permission::SourcesManage,
            self.snapshot.principal(),
        )
        .await?;
        current.revalidate().await?;
        let source = current.retained_allocations().await?;
        let target = self
            .writer
            .retained_allocations(cancel)
            .await
            .map_err(Problem::internal)?;
        // This is the SAME previously admitted named authorization cohort,
        // never a new H claim, replacement workspace, or per-effect64MiB grant.
        let measured = source.checked_add(target).ok_or_else(|| {
            Problem::internal(anyhow::anyhow!(
                "source94 authentication accounting overflow"
            ))
        })?;
        if measured > 64 * 1024 * 1024 {
            return Err(Problem::internal(anyhow::anyhow!(
                "source94 actual authorization cohort exceeds original admission"
            )));
        }
        self.writer
            .verify(cancel)
            .await
            .map_err(Problem::internal)?;
        current.revalidate().await?;
        self.snapshot.revalidate()
    }
    pub(crate) async fn prepare_intent(
        &mut self,
        requester: &motion_catalog_backend::SourceRelocationRequester,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        self.writer.prepare_intent(requester, cancel).await
    }
    pub(crate) async fn prepare_ack(
        &mut self,
        requester: &motion_catalog_backend::SourceRelocationRequester,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        self.writer.prepare_ack(requester, cancel).await
    }
    pub(crate) fn needs_insert(&self) -> bool {
        self.writer.needs_insert()
    }
    pub(crate) async fn finish_saved(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        self.writer.finish_saved(cancel).await
    }
    pub(crate) async fn preflight_execute(
        &mut self,
        requester: &motion_catalog_backend::SourceRelocationRequester,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        self.writer.preflight_execute(requester, cancel).await
    }
    pub(crate) fn execute(
        &mut self,
        current: &CurrentSourceAuthentication<'_>,
        deadline: std::time::Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        relocation94_fence(&self.snapshot, current, deadline, cancel)?;
        self.writer.execute_authorized(cancel)
    }
    pub(crate) async fn observe_receipt(
        &mut self,
        requester: &motion_catalog_backend::SourceRelocationRequester,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        self.writer.observe_receipt(requester, cancel).await
    }
    pub(crate) async fn finish_dispatch(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        self.writer.finish_dispatch(cancel).await
    }
    pub(crate) async fn insert_intent(
        &mut self,
        current: &CurrentSourceAuthentication<'_>,
        deadline: std::time::Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        use std::{future::Future, task::Poll};
        let Self { writer, snapshot } = self;
        let work = writer.insert_intent(cancel);
        tokio::pin!(work);
        std::future::poll_fn(|cx| {
            if let Err(error) = relocation94_fence(snapshot, current, deadline, cancel) {
                return Poll::Ready(Err(error));
            }
            // Both real tokens live through this first effect-capable poll.
            work.as_mut().poll(cx)
        })
        .await
    }
    pub(crate) async fn insert_ack(
        &mut self,
        current: &CurrentSourceAuthentication<'_>,
        deadline: std::time::Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        use std::{future::Future, task::Poll};
        let Self { writer, snapshot } = self;
        let work = writer.insert_ack(cancel);
        tokio::pin!(work);
        std::future::poll_fn(|cx| {
            if let Err(error) = relocation94_fence(snapshot, current, deadline, cancel) {
                return Poll::Ready(Err(error));
            }
            work.as_mut().poll(cx)
        })
        .await
    }
    pub(crate) async fn commit(
        &mut self,
        current: &CurrentSourceAuthentication<'_>,
        deadline: std::time::Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        std::future::poll_fn(|cx| {
            if let Err(error) = relocation94_fence(&self.snapshot, current, deadline, cancel) {
                return std::task::Poll::Ready(Err(error));
            }
            // Future construction is NOT authorization. The backend installs and
            // first-polls its one retained COMMIT only inside this fenced call.
            // A subsequent error leaves that actual runner in the operation.
            self.writer.poll_commit(cx, cancel)
        })
        .await
    }
}
fn relocation94_fence(
    target: &AuthenticationSnapshot,
    current: &CurrentSourceAuthentication<'_>,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cancel.load(std::sync::atomic::Ordering::Acquire),
        "source94 cancelled before effect invocation"
    );
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "source94 original deadline expired before effect invocation"
    );
    target
        .revalidate()
        .map_err(|e| anyhow::anyhow!("source94 target authorization: {e:?}"))?;
    current
        .revalidate_temporal()
        .map_err(|e| anyhow::anyhow!("source94 current authorization: {e:?}"))?;
    Ok(())
}

/// Distinct historical target policy under an exact backend-owned97/96 phase.
/// This is never evidence of current live-source permission. In the retained
/// backend owner variants the reader is only a borrow, so the exact
/// phase retains acquisition/connection custody if prepare or later work is
/// unpolled, fails or unwinds. Legacy target variants retain their prior
/// consuming-reader limitations and are not covered by that custody claim.
pub(crate) struct TargetAuthentication<'owner> {
    reader: motion_catalog_backend::TargetAuthorizationReader<'owner>,
    snapshot: AuthenticationSnapshot,
}
impl<'owner> TargetAuthentication<'owner> {
    pub(crate) async fn prepare(
        reader: motion_catalog_backend::TargetAuthorizationReader<'owner>,
        caller: &Caller,
        permission: Permission,
    ) -> Result<Self, Problem> {
        Self::prepare_inner(
            reader,
            caller,
            permission,
            #[cfg(test)]
            None,
        )
        .await
    }
    #[cfg(test)]
    pub(crate) async fn prepare_observed(
        reader: motion_catalog_backend::TargetAuthorizationReader<'owner>,
        caller: &Caller,
        permission: Permission,
        observation: &PreparationObservation,
    ) -> Result<Self, Problem> {
        Self::prepare_inner(reader, caller, permission, Some(observation)).await
    }
    async fn prepare_inner(
        mut reader: motion_catalog_backend::TargetAuthorizationReader<'owner>,
        caller: &Caller,
        permission: Permission,
        #[cfg(test)] observation: Option<&PreparationObservation>,
    ) -> Result<Self, Problem> {
        let db = reader
            .authorize_connection()
            .await
            .map_err(Problem::internal)?;
        #[cfg(test)]
        if let Some(observation) = observation {
            observation.reached().await?;
        }
        let principal = caller.reauthorize(db, Some(permission)).await?;
        let family = match &principal.device_id {
            Some(id) => super::super::signin::target_authorization_lineage(&mut reader, id).await?,
            None => None,
        };
        let db = reader
            .authorize_connection()
            .await
            .map_err(Problem::internal)?;
        let snapshot = AuthenticationSnapshot::capture_authenticated(
            db, caller, permission, &principal, family,
        )
        .await?;
        reader.verify().await.map_err(Problem::internal)?;
        snapshot.revalidate()?;
        Ok(Self { reader, snapshot })
    }
    pub(crate) fn principal(&self) -> &Principal {
        self.snapshot.principal()
    }
    pub(crate) async fn revalidate(&mut self) -> Result<(), Problem> {
        self.reader.verify().await.map_err(Problem::internal)?;
        self.snapshot.revalidate()
    }
    /// Purpose-bound retained93→97 close. The independently retained backend
    /// owns the actual close future before this borrowed waiter is returned.
    /// Preserve the genuine token across full preflight, DDL and COMMIT fences.
    pub(crate) fn close_retained_control_reader(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), Problem>> + '_ {
        let waiter = self.reader.close_retained_control_reader();
        async move { waiter.await.map_err(Problem::internal) }
    }
    pub(crate) fn preflight_retained_control(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .preflight_retained_control(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn apply_retained_control(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .apply_retained_control(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn commit_retained_control(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .commit_retained_control(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn finish_retained_control(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .finish_retained_control(cancel)
            .map_err(Problem::internal)
    }
    /// Release and join only this retained-native-intent reader. The backend
    /// installs the actual SQLx close future in its independently retained
    /// operation before this method returns; this waiter only borrows it. Keep
    /// this real authentication token for the fixed synchronous continuations.
    /// This must not become `async fn`: an unpolled waiter is terminal too.
    pub(crate) fn close_retained_native_intent_reader(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), Problem>> + '_ {
        let waiter = self.reader.close_retained_native_intent_reader();
        async move { waiter.await.map_err(Problem::internal) }
    }
    /// Full owner/reference/allocation scans happen after the last await and
    /// before the coordinator's final target/current temporal checks.
    pub(crate) fn preflight_retained_native_intent(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .preflight_retained_native_intent(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn lock_retained_native_source(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .lock_retained_native_source(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn prepare_locked_native_intent(
        &mut self,
        source: &std::fs::File,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .prepare_locked_native_intent(source, cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn prepare_retained_native_intent(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .prepare_retained_native_intent(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn insert_retained_native_intent(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .insert_retained_native_intent(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn commit_retained_native_intent(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .commit_retained_native_intent(cancel)
            .map_err(Problem::internal)
    }
    /// A distinct, freshly authenticated target reader for retained native apply.
    /// The backend owns its actual close future before this borrowed waiter is
    /// returned. Prior intent-phase Joined readers cannot use these methods.
    pub(crate) fn close_retained_native_apply_reader(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), Problem>> + '_ {
        let waiter = self.reader.close_retained_native_apply_reader();
        async move { waiter.await.map_err(Problem::internal) }
    }
    pub(crate) fn preflight_retained_native_apply(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .preflight_retained_native_apply(cancel)
            .map_err(Problem::internal)
    }
    pub(crate) fn apply_retained_native(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .apply_retained_native(cancel)
            .map_err(Problem::internal)
    }

    /// A freshly authenticated, purpose-bound command/execute/ACK reader. Its
    /// actual close future is retained by the backend before returning a waiter.
    pub(crate) fn close_retained_native_bootstrap_reader(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), Problem>> + '_ {
        let waiter = self.reader.close_retained_native_bootstrap_reader();
        async move { waiter.await.map_err(Problem::internal) }
    }

    pub(crate) fn preflight_retained_native_bootstrap(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .preflight_retained_native_bootstrap(cancel)
            .map_err(Problem::internal)
    }

    pub(crate) fn insert_retained_native_bootstrap_command(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .insert_retained_native_bootstrap_command(cancel)
            .map_err(Problem::internal)
    }

    pub(crate) fn commit_retained_native_bootstrap_command(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .commit_retained_native_bootstrap_command(cancel)
            .map_err(Problem::internal)
    }

    pub(crate) fn execute_retained_native_bootstrap(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .execute_retained_native_bootstrap(cancel)
            .map_err(Problem::internal)
    }

    pub(crate) fn insert_retained_native_bootstrap_ack(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .insert_retained_native_bootstrap_ack(cancel)
            .map_err(Problem::internal)
    }

    pub(crate) fn commit_retained_native_bootstrap_ack(
        &mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), Problem> {
        self.reader
            .commit_retained_native_bootstrap_ack(cancel)
            .map_err(Problem::internal)
    }
    /// Recheck time after other retained owners have been verified. This does
    /// not replace `revalidate`: callers must retain this token and separately
    /// verify its unchanged owner before relying on this no-IO temporal check.
    pub(crate) fn revalidate_temporal(&self) -> Result<(), Problem> {
        self.snapshot.revalidate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(credential_expiry: i64, device_expiry: i64) -> AuthenticationSnapshot {
        let mut device = Device::approved(
            "device".into(),
            Grant {
                profile_ids: BTreeSet::new(),
                permissions: [Permission::CatalogWrite].into(),
            },
        );
        device.credential_expires_at = Some(device_expiry);
        let credential = Credential {
            device_id: device.id.clone(),
            kind: CredentialKind::Device,
            generation: device.generation,
            expires_at: credential_expiry,
        };
        let principal = authenticate_caller(
            Authentication::Credential {
                credential: Some(&credential),
                parent: None,
                device: Some(&device),
            },
            10,
        )
        .unwrap();
        AuthenticationSnapshot {
            principal,
            permission: Permission::CatalogWrite,
            family: None,
            facts: Facts::Credential {
                credential,
                parent: None,
                device: Some(device),
            },
        }
    }
    #[test]
    fn both_expiry_boundaries_are_rechecked_without_changing_retained_scope() {
        for snapshot in [snapshot(20, 30), snapshot(30, 20)] {
            let principal = snapshot.principal().clone();
            snapshot.revalidate_at(19).unwrap();
            assert!(snapshot.revalidate_at(20).is_err());
            assert!(snapshot.revalidate_at(21).is_err());
            assert_eq!(snapshot.principal(), &principal);
        }
    }
    #[test]
    fn retained_scope_cannot_be_replaced_or_permission_broadened() {
        let mut changed = snapshot(20, 30);
        changed.principal.policy_revision += 1;
        assert!(changed.revalidate_at(19).is_err());
        let mut broader = snapshot(20, 30);
        broader.permission = Permission::SystemAdmin;
        assert!(broader.revalidate_at(19).is_err());
    }
    #[tokio::test]
    async fn actual_loaded_credential_expires_while_retained_writer_blocks_policy_changes() {
        use sqlx::{Connection, sqlite::SqliteConnectOptions};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("motion.sqlite");
        let db = crate::db::connect(&path).await.unwrap();
        let expiry = crate::now().checked_add(300).unwrap();
        let mut device = Device::approved(
            "retained-auth-device".into(),
            Grant {
                profile_ids: BTreeSet::new(),
                permissions: [Permission::CatalogWrite].into(),
            },
        );
        device.credential_expires_at = Some(expiry + 100);
        let credential = Credential {
            device_id: device.id.clone(),
            kind: CredentialKind::Device,
            generation: device.generation,
            expires_at: expiry,
        };
        let mut setup = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        insert_device(
            &mut setup,
            &DeviceRow {
                device: device.clone(),
                name: "test".into(),
                client_name: "test".into(),
                user_id: None,
            },
        )
        .await
        .unwrap();
        let hash = "a".repeat(64);
        insert_credential(&mut setup, &hash, &credential, None, None)
            .await
            .unwrap();
        setup.commit().await.unwrap();
        let principal = authenticate_caller(
            Authentication::Credential {
                credential: Some(&credential),
                parent: None,
                device: Some(&device),
            },
            crate::now(),
        )
        .unwrap();
        let caller = Caller {
            principal: principal.clone(),
            csrf: None,
            source: CallerSource::Credential {
                hash,
                admitted: credential,
            },
        };
        let mut writer = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        let mut stale_scope = principal.clone();
        stale_scope.policy_revision += 1;
        assert!(
            AuthenticationSnapshot::prepare_in(
                &mut writer,
                &caller,
                Permission::CatalogWrite,
                &stale_scope
            )
            .await
            .is_err()
        );
        let snapshot = AuthenticationSnapshot::prepare_in(
            &mut writer,
            &caller,
            Permission::CatalogWrite,
            &principal,
        )
        .await
        .unwrap();
        let mut budget = crate::nfo::SCAN_BUDGET;
        let custody =
            crate::nfo::native_custody::RetainedSidecars::prepare_in(&mut writer, &[], &mut budget)
                .await
                .unwrap();
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .busy_timeout(std::time::Duration::ZERO);
        let mut concurrent = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        let error = sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
            .bind(&device.id)
            .execute(&mut concurrent)
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("5")
        );
        custody.revalidate().unwrap();
        snapshot.revalidate_at(expiry - 1).unwrap();
        // Controlled time invokes the same terminal method as revalidate(),
        // without sleeps or relying on a scheduler crossing the expiry boundary.
        let expired = snapshot.revalidate_at(expiry).unwrap_err();
        assert_eq!(expired.code, AccessError::CredentialExpired.code());
        drop(custody);
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        assert_eq!(before, after);
        assert!(
            !load_device(&mut writer, &device.id)
                .await
                .unwrap()
                .unwrap()
                .device
                .revoked
        );
        writer.rollback().await.unwrap();
        sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
            .bind(&device.id)
            .execute(&mut concurrent)
            .await
            .unwrap();
        concurrent.close().await.unwrap();
        let mut recheck = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert_eq!(
            caller
                .reauthorize(&mut recheck, Some(Permission::CatalogWrite))
                .await
                .unwrap_err()
                .code,
            AccessError::CredentialRevoked.code()
        );
        recheck.rollback().await.unwrap();
        db.close().await;
    }
    #[tokio::test]
    async fn actual_joined_family_expiry_revokes_credential_and_ingress_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::db::connect(&directory.path().join("motion.sqlite"))
            .await
            .unwrap();
        let now = crate::now();
        let family_expiry = now.checked_add(300).unwrap();
        let mut device = Device::approved(
            "family-device".into(),
            Grant {
                profile_ids: BTreeSet::new(),
                permissions: [Permission::CatalogWrite].into(),
            },
        );
        device.credential_expires_at = Some(family_expiry + 100);
        let credential = Credential {
            device_id: device.id.clone(),
            kind: CredentialKind::Device,
            generation: device.generation,
            expires_at: family_expiry + 100,
        };
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        insert_device(
            &mut tx,
            &DeviceRow {
                device: device.clone(),
                name: "family".into(),
                client_name: "test".into(),
                user_id: None,
            },
        )
        .await
        .unwrap();
        let hash = "b".repeat(64);
        insert_credential(&mut tx, &hash, &credential, None, None)
            .await
            .unwrap();
        let permissions = json(&device.grant.permissions);
        let policy = json(&device.policy);
        // Real joined owner rows: the production loader validates user, identity,
        // provider, reviewed epoch and family revision/expiry together.
        sqlx::query("INSERT INTO access_users(id,name,revision,disabled,profile_ids,permissions,policy,legacy,created_at) VALUES('family-user','test',1,0,'[]',?,?,0,?)")
            .bind(&permissions).bind(&policy).bind(now).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO signin_providers(id,revision,name,issuer,client_id,enabled) VALUES('family-provider',1,'test','https://issuer.invalid','client',1)")
            .execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO signin_identities(id,user_id,kind,provider_id,issuer,subject,enabled,revision,profile_ids,permissions,policy,created_at) VALUES('family-identity','family-user','oidc','family-provider','https://issuer.invalid','subject',1,1,'[]',?,?,?)")
            .bind(&permissions).bind(&policy).bind(now).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO signin_families(id,device_id,user_id,identity_id,identity_revision,method_revision,epoch,issued_at,authenticated_at,expires_at,parent_hash,session_hash) SELECT 'family',?,'family-user','family-identity',1,1,restore_epoch,?,?,?,'parent','session' FROM server_identity")
            .bind(&device.id).bind(now).bind(now).bind(family_expiry).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        for ingress in [false, true] {
            let principal = authenticate_caller(
                if ingress {
                    Authentication::TrustedIngress {
                        device_id: &device.id,
                        device: Some(&device),
                    }
                } else {
                    Authentication::Credential {
                        credential: Some(&credential),
                        parent: None,
                        device: Some(&device),
                    }
                },
                now,
            )
            .unwrap();
            let caller = Caller {
                principal: principal.clone(),
                csrf: None,
                source: if ingress {
                    CallerSource::TrustedIngress {
                        device_id: device.id.clone(),
                    }
                } else {
                    CallerSource::Credential {
                        hash: hash.clone(),
                        admitted: credential.clone(),
                    }
                },
            };
            let mut writer = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let retained = AuthenticationSnapshot::prepare_in(
                &mut writer,
                &caller,
                Permission::CatalogWrite,
                &principal,
            )
            .await
            .unwrap();
            assert!(retained.family.is_some());
            retained.revalidate_at(family_expiry - 1).unwrap();
            assert_eq!(
                retained.revalidate_at(family_expiry).unwrap_err().code,
                AccessError::CredentialRevoked.code()
            );
            // Direct core credentials still live: denial is the exact existing
            // joined family rule, including for otherwise timeless ingress.
            authenticate_caller(
                Authentication::Credential {
                    credential: Some(&credential),
                    parent: None,
                    device: Some(&device),
                },
                family_expiry,
            )
            .unwrap();
            writer.rollback().await.unwrap();
        }
        db.close().await;
    }
}

#[cfg(test)]
use crate as playscale_app;
#[cfg(test)]
#[path = "../../../tests/support/native_application_fixture.rs"]
mod source_auth_fixture;
#[cfg(test)]
mod source_reader_tests {
    use super::*;
    use axum::{body::Body, extract::FromRequestParts, http::Request};
    use source_auth_fixture::{Fixture, OPERATOR};

    #[tokio::test]
    async fn current_source_reader_accepts_current_caller_then_refuses_preacquisition_revocation() {
        let mut f = Fixture::new().await;
        let (device, token) = f.pair(&["default"], &["sources:manage"]).await;
        f.cutover().await;
        let (mut parts, _) = Request::builder()
            .uri("/api/v2/sources")
            .header("host", "127.0.0.1:8787")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
            .into_parts();
        let caller = Caller::from_request_parts(&mut parts, &f.app)
            .await
            .unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut writer =
            motion_catalog_backend::motion_snapshot::FreshWriter::begin(&f.app.db, &cancel)
                .await
                .unwrap();
        let reader = writer.authorization_reader().await.unwrap();
        let mut current =
            CurrentSourceAuthentication::prepare(reader, &caller, Permission::SourcesManage)
                .await
                .unwrap();
        assert_eq!(current.principal(), &caller.principal);
        current.revalidate().await.unwrap();
        current.retire_inline().await.unwrap();
        writer.close().await.unwrap();
        drop(writer);
        let operator = format!("Bearer {OPERATOR}");
        let path = format!("/api/v2/devices/{device}");
        let descriptor = f
            .call("GET", &path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(descriptor.status, StatusCode::OK);
        let revoked = f
            .call(
                "DELETE",
                &path,
                None,
                &[
                    ("authorization", &operator),
                    ("if-match", descriptor.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(revoked.status, StatusCode::NO_CONTENT);
        let mut writer =
            motion_catalog_backend::motion_snapshot::FreshWriter::begin(&f.app.db, &cancel)
                .await
                .unwrap();
        let reader = writer.authorization_reader().await.unwrap();
        let denied =
            match CurrentSourceAuthentication::prepare(reader, &caller, Permission::SourcesManage)
                .await
            {
                Ok(_) => panic!("historical ingress caller regained revoked source permission"),
                Err(error) => error,
            };
        assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
        assert!(writer.retains_authorization_reader());
        assert!(writer.authorization_reader().await.is_err());
        writer.close().await.unwrap();
        drop(writer);
        f.close().await;
    }
}

/// Genuine test credential for the stopped-copy prepared97 expiry regression.
/// Uses ordinary public pairing and access-token derivation, never auth-row
/// edits, fabricated Caller fields, or a changed clock/TTL contract.
#[cfg(test)]
pub(crate) async fn actual_expiring_admin(app: &App) -> (Caller, i64) {
    let (caller, expiry, _) = actual_admin(app, true).await;
    (caller, expiry)
}

/// The genuine five-fence Source94 fixture needs finite initial headroom for
/// its full export/97/96 prerequisites. This is a public API request, not a
/// production lifetime change or a renewal of an already captured credential.
/// Keep actual_expiring_admin's existing 60-second default for all other tests.
#[cfg(test)]
pub(crate) async fn actual_expiring_admin_for_source94(app: &App) -> (Caller, i64) {
    let (caller, expiry, _) = actual_admin_with_ttl(app, true, 100).await;
    (caller, expiry)
}

/// Dedicated Native-capture expiry-cause fixture. The genuine120s credential
/// is issued once through the public API before stop/export. Existing60s and
/// source94100s fixture defaults remain unchanged; this never renews a token.
#[cfg(test)]
pub(crate) async fn actual_expiring_admin_for_native_capture(app: &App) -> (Caller, i64) {
    let (caller, expiry, _) = actual_admin_with_ttl(app, true, 120).await;
    (caller, expiry)
}

#[cfg(test)]
pub(crate) async fn actual_paired_admin(app: &App) -> Caller {
    actual_admin(app, false).await.0
}

#[cfg(test)]
pub(crate) async fn actual_paired_admin_with_bearer(app: &App) -> (Caller, String) {
    let (caller, _, bearer) = actual_admin(app, false).await;
    (caller, bearer)
}

#[cfg(test)]
async fn actual_admin(app: &App, expiring: bool) -> (Caller, i64, String) {
    actual_admin_with_ttl(app, expiring, 60).await
}

#[cfg(test)]
async fn actual_admin_with_ttl(app: &App, expiring: bool, ttl_seconds: i64) -> (Caller, i64, String) {
    use axum::body::Body as AxumBody;
    use serde_json::{Value, json};
    use tower::ServiceExt;
    async fn post(
        app: &App,
        path: &str,
        body: Value,
        bearer: Option<&str>,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("host", app.authority.as_str())
            .header("content-type", "application/json");
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = crate::api::router(app.clone(), None)
            .oneshot(request.body(AxumBody::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!(
                "non-JSON auth response: {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, body)
    }
    let (status, pairing) = post(
        app,
        "/api/v2/auth/pairings",
        json!({"device_name":"Prepared expiry owner","client_name":"actual-pair-test"}),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{pairing:?}");
    let id = pairing["id"].as_str().unwrap();
    let (status, approval) = post(
        app,
        &format!("/api/v2/auth/pairings/{id}/approve"),
        json!({"user_code":pairing["user_code"],"profile_ids":[],"permissions":["system:admin"]}),
        Some(app.admin_token.as_str()),
        Some(&format!("prepared-expiry-approve-{id}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approval:?}");
    let (status, claimed) = post(
        app,
        &format!("/api/v2/auth/pairings/{id}/claim"),
        json!({"device_code":pairing["device_code"]}),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{claimed:?}");
    let parent = claimed["access_token"].as_str().unwrap();
    let token = if expiring {
        let (status, issued) = post(
            app,
            "/api/v2/auth/access-tokens",
            json!({"ttl_seconds":ttl_seconds}),
            Some(parent),
            Some(&format!("prepared-expiry-child-{id}")),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        issued["access_token"].as_str().unwrap().to_owned()
    } else {
        parent.to_owned()
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let caller = super::resolve(app, &Method::GET, &headers).await.unwrap();
    let expiry = caller.credential().unwrap().1.expires_at;
    assert!(
        crate::now() < expiry,
        "real credential expired during issuance"
    );
    (caller, expiry, token)
}

/// Fixed fixture use of the existing policy owners, never raw auth-row writes.
/// The caller separately retains the genuine converted actor/pair writer.
#[cfg(test)]
pub(crate) async fn deny_target_source_management_in(
    db: &mut sqlx::SqliteConnection,
    reserve_admin: &Caller,
    requester: &Caller,
) -> Result<(), Problem> {
    motion_catalog_backend::source_lifecycle::require_retained_writer_in(db)
        .await
        .map_err(Problem::internal)?;
    let admin = reserve_admin
        .reauthorize(db, Some(Permission::SystemAdmin))
        .await?;
    let id = requester
        .principal
        .device_id
        .as_deref()
        .ok_or_else(Problem::not_found)?;
    let row = load_device(db, id).await?.ok_or_else(Problem::not_found)?;
    if !row.device.policy.library_ids.is_empty() || admin.device_id.as_deref() == Some(id) {
        return Err(Problem::invalid(
            "fixture_policy_cohort",
            "A distinct reserve admin and unrestricted target device are required",
        ));
    }
    let next = playscale_core::access::replace_policy(
        &row.device,
        row.device.revision,
        Default::default(),
        row.device.policy.clone(),
        &Default::default(),
    )?
    .ok_or_else(|| {
        Problem::invalid(
            "fixture_policy_unchanged",
            "Expected an actual permission withdrawal",
        )
    })?;
    // This only removes permissions; no administrator addition/fresh-proof
    // exemption or named native-library validation is being introduced.
    super::super::access_management::guard_admin_change(db, &[(row.device.clone(), next.clone())])
        .await?;
    super::super::access_management::check_device_ceiling(db, &row, &next).await?;
    update_device(db, &row.device, &next).await?;
    super::super::signin::audit(db, &admin.id, id, "device.policy", "ok").await?;
    Ok(())
}

// Fixed actual-service capture acquisition, not a generic callback grant.
#[path = "retained/capture_ingress.rs"]
mod capture_ingress;
