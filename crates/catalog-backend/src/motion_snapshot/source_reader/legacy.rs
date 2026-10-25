//! Compatibility reader for the old consuming actor/image API only. Unlike the
//! retained reader, its connection is token-owned: cancellation/error/drop does
//! not prove worker-close acknowledgment. It cannot enter retained close/reuse
//! or connected/service authorization paths.
use super::{FreshWriter, binding, main_path, open_restricted_reader, verify_reader_allocation};
use anyhow::{Context, ensure};
use sqlx::SqliteConnection;
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

pub struct LegacyAuthorizationReader<'writer> {
    reader: SqliteConnection,
    writer: &'writer mut FreshWriter,
    file: File,
    path: PathBuf,
    identity: (u64, u64),
}

impl FreshWriter {
    /// Explicit legacy compatibility. No retained-reader shutdown guarantee.
    pub async fn legacy_authorization_reader(
        &mut self,
    ) -> anyhow::Result<LegacyAuthorizationReader<'_>> {
        ensure!(
            !self.authorization_claimed(),
            "legacy source cannot bypass retained acquisition custody"
        );
        let path = main_path(&mut self.transaction, true).await?;
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "retained authorization source is not single-link regular file"
        );
        let identity = (metadata.dev(), metadata.ino());
        let reader = open_restricted_reader(&path).await?;
        let mut token = LegacyAuthorizationReader {
            reader,
            writer: self,
            file,
            path,
            identity,
        };
        token.verify().await?;
        Ok(token)
    }
}

impl LegacyAuthorizationReader<'_> {
    pub async fn authorize_connection(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        self.verify().await?;
        Ok(&mut self.reader)
    }
    pub async fn verify(&mut self) -> anyhow::Result<()> {
        ensure!(
            main_path(&mut self.writer.transaction, true).await? == self.path
                && main_path(&mut self.reader, false).await? == self.path,
            "retained authorization foreign main file"
        );
        let held = self.file.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            held.is_file()
                && named.is_file()
                && held.nlink() == 1
                && named.nlink() == 1
                && (held.dev(), held.ino()) == self.identity
                && (named.dev(), named.ino()) == self.identity,
            "retained authorization source identity changed"
        );
        let current = binding::validate(&mut self.writer.transaction)
            .await?
            .context("retained authorization requires native source binding")?;
        ensure!(
            Some(&current) == self.writer.binding.as_ref(),
            "retained authorization writer binding changed"
        );
        ensure!(
            binding::validate(&mut self.reader).await?.as_ref() == Some(&current),
            "retained authorization reader binding differs"
        );
        Ok(())
    }
    pub async fn retained_allocations(&mut self) -> anyhow::Result<u64> {
        self.verify().await?;
        async fn observed(db: &mut SqliteConnection) -> anyhow::Result<u64> {
            let mut lease = db.lock_handle().await?;
            // SAFETY: lexical non-owning inspection under exclusive SQLx lease.
            let connection =
                unsafe { rusqlite::Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
            super::super::connection_allocations(&[&connection])
        }
        let source = observed(&mut self.writer.transaction).await?;
        let reader = observed(&mut self.reader).await?;
        verify_reader_allocation(reader)?;
        source
            .checked_add(reader)
            .context("authorization retained allocation overflow")
    }
}
