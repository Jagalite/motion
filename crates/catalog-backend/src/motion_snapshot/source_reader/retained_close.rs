//! Independently retained settlement of the exact current-source reader owner.
//! The whole resource slot transfers synchronously, including FD, pathname and
//! pending connect future. Neither an auth token nor a join waiter owns them.
use super::{FreshWriter, RestrictedReaderOwner, RetainedAuthorizationReader};
use anyhow::ensure;
use std::sync::Arc;

// Conservative logical allocation share for one private Arc<()> identity. This
// is not a claim about allocator resident bytes or a new workspace grant.
pub(super) const SOURCE_IDENTITY_RESERVE: u64 = 64;

#[derive(Default)]
pub struct RetainedAuthorizationCloseSlot {
    owner: Option<RestrictedReaderOwner>,
    claim: Option<Arc<()>>,
}

impl RetainedAuthorizationCloseSlot {
    /// This inline slot already contains the moved owner. The connect and close
    /// frames coexist and therefore have separate mandatory fixed shares.
    pub const fn storage_reservation() -> u64 {
        std::mem::size_of::<Self>() as u64 + RestrictedReaderOwner::storage_reservation()
            - std::mem::size_of::<RestrictedReaderOwner>() as u64
            + SOURCE_IDENTITY_RESERVE
    }

    pub fn retains_close(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(RestrictedReaderOwner::retains_close)
    }

    /// Exact physical slot presence also covers cleanup while connect is still
    /// pending and no close box can yet exist. Not acknowledgment or authority.
    pub fn retains_reader_owner(&self) -> bool {
        self.owner.is_some()
    }

    /// Only a real successful SQLx worker-close acknowledgment.
    pub fn is_joined(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(RestrictedReaderOwner::close_joined)
    }

    /// Terminal cleanup may also prove no connect future was ever started.
    /// That known absence is deliberately NOT a worker-close acknowledgment.
    pub fn is_settled(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(RestrictedReaderOwner::is_settled)
    }

    /// Resume only the stored connect/close operation. Dropping this waiter
    /// leaves the exact underlying future in this slot. Connect failure/panic
    /// remains terminal unresolved closure, never a reconstruction or retry.
    pub async fn join(&mut self) -> anyhow::Result<()> {
        self.owner
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("retained authorization close has not begun"))?
            .join_close()
            .await
    }
}

impl FreshWriter {
    /// Cleanup is available even if acquisition/prepare returned no token.
    /// All custody moves into the admitted workspace before starting close.
    pub fn begin_retained_authorization_close(
        &mut self,
        slot: &mut RetainedAuthorizationCloseSlot,
    ) -> anyhow::Result<()> {
        ensure!(
            slot.owner.is_none() && slot.claim.is_none(),
            "retained authorization close slot is already occupied"
        );
        ensure!(
            !self.authorization.transferred
                && !self.authorization.settled
                && self.authorization.claim.is_some()
                && self.authorization.reader.is_claimed(),
            "retained authorization reader is unclaimed, transferred or settled"
        );
        slot.claim = self.authorization.claim.clone();
        slot.owner = Some(std::mem::take(&mut self.authorization.reader));
        self.authorization.transferred = true;
        // Any begin failure leaves the whole original resource owner here.
        slot.owner.as_mut().unwrap().begin_close()
    }

    fn verify_authorization_close_identity(
        &self,
        slot: &RetainedAuthorizationCloseSlot,
    ) -> anyhow::Result<()> {
        ensure!(
            self.authorization.transferred
                && !self.authorization.settled
                && self
                    .authorization
                    .claim
                    .as_ref()
                    .zip(slot.claim.as_ref())
                    .is_some_and(|(a, b)| Arc::ptr_eq(a, b)),
            "retained authorization close belongs to another source attempt"
        );
        Ok(())
    }

    /// Requires the ended token's exclusive writer borrow and the exact same
    /// slot's actual close ACK. Arbitrary/old joined slots cannot authorize reuse.
    pub fn retire_authorization_close(
        &mut self,
        slot: &mut RetainedAuthorizationCloseSlot,
    ) -> anyhow::Result<()> {
        self.verify_authorization_close_identity(slot)?;
        ensure!(
            slot.is_joined(),
            "source reader reuse requires actual worker close acknowledgment"
        );
        *slot = RetainedAuthorizationCloseSlot::default();
        self.authorization.claim = None;
        self.authorization.transferred = false;
        Ok(())
    }

    /// Terminal cleanup only. Known-never-opened is accepted, but this owner
    /// stays claimed and cannot acquire a fresh authorization reader afterward.
    pub fn settle_authorization_close(
        &mut self,
        slot: &mut RetainedAuthorizationCloseSlot,
    ) -> anyhow::Result<()> {
        self.verify_authorization_close_identity(slot)?;
        ensure!(slot.is_settled(), "source reader cleanup is not settled");
        self.authorization.settled = true;
        Ok(())
    }

    /// Descriptive physical presence only, never admission or authorization.
    pub fn retains_authorization_reader(&self) -> bool {
        self.authorization.reader.retains_reader()
    }
    pub fn retains_authorization_acquisition(&self) -> bool {
        self.authorization.reader.retains_acquisition()
    }
    pub fn authorization_claimed(&self) -> bool {
        self.authorization.claim.is_some()
    }
}

impl<'writer> RetainedAuthorizationReader<'writer> {
    pub fn begin_retained_close(
        &mut self,
        slot: &mut RetainedAuthorizationCloseSlot,
    ) -> anyhow::Result<()> {
        self.writer.begin_retained_authorization_close(slot)
    }

    /// Consuming the borrow token precedes the exact-identity retirement check.
    pub fn retire_authorization_close(
        self,
        slot: &mut RetainedAuthorizationCloseSlot,
    ) -> anyhow::Result<()> {
        let RetainedAuthorizationReader { writer } = self;
        writer.retire_authorization_close(slot)
    }

    /// Named inline settlement for consuming callers without a transferred
    /// workspace slot. Cancellation keeps original resources on FreshWriter;
    /// only an actual acknowledged close permits this exact owner to reopen.
    pub fn retire_inline(self) -> impl std::future::Future<Output = anyhow::Result<()>> + 'writer {
        let RetainedAuthorizationReader { writer } = self;
        let begun = if writer.authorization.transferred || writer.authorization.settled {
            Err(anyhow::anyhow!(
                "source reader is already transferred or settled"
            ))
        } else {
            writer.authorization.reader.begin_close()
        };
        async move {
            begun?;
            writer.authorization.reader.join_close().await?;
            ensure!(
                writer.authorization.reader.close_joined(),
                "source reader reuse requires actual worker close acknowledgment"
            );
            writer.authorization.reader = RestrictedReaderOwner::default();
            writer.authorization.claim = None;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
