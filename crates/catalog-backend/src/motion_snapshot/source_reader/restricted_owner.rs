//! Resource custody only. Phase authorization and budget admission belong to the
//! fixed caller. This slot never reopens, resets, grants SQL authority, or guesses
//! that SQLx's connection-internal error path acknowledged worker shutdown.
use super::authorize;
use anyhow::{Context as _, ensure};
use rusqlite::ffi;
use sqlx::{Connection as _, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    fs::File,
    future::{Future, poll_fn},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};

// Supported retained-path cohort, not a claim about every filesystem.
pub(crate) const MAX_PATH_BYTES: usize = 4096;
pub(crate) const MAX_CONNECT_FRAME: usize = 64 * 1024;
pub(crate) const MAX_CLOSE_FRAME: usize = 64 * 1024;
type ConnectFuture = Pin<Box<dyn Future<Output = Result<SqliteConnection, sqlx::Error>> + Send>>;
type CloseFuture = Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send>>;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderState {
    #[default]
    Vacant,
    Claimed,
    PreparingConnect,
    Opening,
    // A panic leaves this marker and the same box: it must never be repolled.
    PollingConnect,
    ConnectFailed,
    ConnectOversized,
    Connected,
    // Cancellation/unwind at any setup await leaves the original connection
    // here. Setup is never retried, but real close remains possible.
    SettingUp,
    SetupFailed,
    Ready,
    Transferred,
    Closing,
    PollingClose,
    CloseFailed,
    CloseOversized,
    Joined,
    NeverOpened,
}

#[derive(Default)]
pub(crate) struct RestrictedReaderOwner {
    path: Option<PathBuf>,
    file: Option<File>,
    identity: Option<(u64, u64)>,
    opening: Option<ConnectFuture>,
    reader: Option<SqliteConnection>,
    close: Option<CloseFuture>,
    state: ReaderState,
    #[cfg(test)]
    setup_fault: Option<SetupFault>,
    #[cfg(test)]
    setup_entered: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[cfg(test)]
pub(crate) enum SetupFault {
    Pending,
    Error,
    Panic,
}

impl RestrictedReaderOwner {
    /// Includes this inline slot and both simultaneously retained frame shares.
    /// This is a cost, never evidence that a host admitted that cost.
    pub(crate) const fn storage_reservation() -> u64 {
        (std::mem::size_of::<Self>() + MAX_CONNECT_FRAME + MAX_CLOSE_FRAME) as u64
    }

    /// Add this share when the enclosing owner already counts this inline slot
    /// and has a separate existing close-frame reservation.
    pub(crate) const fn opening_frame_reservation() -> u64 {
        MAX_CONNECT_FRAME as u64
    }

    pub(crate) fn is_claimed(&self) -> bool {
        self.state != ReaderState::Vacant
    }

    pub(crate) fn claim(&mut self) -> anyhow::Result<()> {
        ensure!(
            !self.is_claimed(),
            "restricted reader acquisition is already claimed"
        );
        self.state = ReaderState::Claimed;
        Ok(())
    }

    pub(crate) fn install_path(&mut self, path: PathBuf) -> anyhow::Result<()> {
        ensure!(
            self.state == ReaderState::Claimed && self.path.is_none(),
            "restricted reader path is already claimed"
        );
        use std::os::unix::ffi::OsStrExt;
        ensure!(
            path.as_os_str().as_bytes().len() <= MAX_PATH_BYTES,
            "restricted reader pathname exceeds supported bounded cohort"
        );
        self.path = Some(path);
        self.file = Some(
            File::options()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(self.path()?)?,
        );
        // The returned FD is already installed before any fallible inspection.
        let metadata = self.file.as_ref().unwrap().metadata()?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "retained authorization source is not single-link regular file"
        );
        self.identity = Some((metadata.dev(), metadata.ino()));
        Ok(())
    }

    pub(crate) fn path(&self) -> anyhow::Result<&Path> {
        self.path
            .as_deref()
            .context("restricted reader path is not installed")
    }

    pub(crate) fn verify_identity(&self) -> anyhow::Result<()> {
        let held = self
            .file
            .as_ref()
            .context("restricted reader file is not installed")?
            .metadata()?;
        let named = std::fs::symlink_metadata(self.path()?)?;
        let identity = self
            .identity
            .context("restricted reader identity is not installed")?;
        ensure!(
            held.is_file()
                && named.is_file()
                && held.nlink() == 1
                && named.nlink() == 1
                && (held.dev(), held.ino()) == identity
                && (named.dev(), named.ino()) == identity,
            "retained authorization source identity changed"
        );
        Ok(())
    }

    pub(crate) async fn acquire(&mut self) -> anyhow::Result<()> {
        self.acquire_inner(false).await
    }

    /// Fixed capture-only policy. No options, capacity, query or callback is
    /// supplied by callers. All other readers keep the existing SQLx defaults.
    pub(crate) async fn acquire_uncached(&mut self) -> anyhow::Result<()> {
        self.acquire_inner(true).await
    }

    async fn acquire_inner(&mut self, uncached: bool) -> anyhow::Result<()> {
        ensure!(
            self.state == ReaderState::Claimed && self.opening.is_none(),
            "restricted reader acquisition cannot retry"
        );
        // Claim the first attempt before any fallible setup. A path/metadata
        // failure is known-never-opened, but must not enable another acquire.
        self.state = ReaderState::PreparingConnect;
        self.verify_identity()?;
        let options = SqliteConnectOptions::new()
            .filename(self.path()?)
            .read_only(true)
            .create_if_missing(false)
            .pragma("query_only", "ON")
            .pragma("cache_size", "-2048");
        let options = if uncached { options.statement_cache_capacity(0) } else { options };
        // The outer owned future is installed before it or SQLx's nested
        // connect future can be polled. No self-reference or borrowed options.
        self.opening = Some(Box::pin(async move {
            let opening = SqliteConnection::connect_with(&options);
            // SQLx returns a boxed future. Its concrete frame is additional to
            // the outer owned-options frame, so bound both rather than hiding
            // the nested box behind one size_of_val observation.
            ensure_sqlx_connect_frame(opening.as_ref().get_ref())?;
            opening.await
        }));
        let bytes = std::mem::size_of_val(self.opening.as_ref().unwrap().as_ref().get_ref());
        if bytes > MAX_CONNECT_FRAME / 2 {
            self.state = ReaderState::ConnectOversized;
            anyhow::bail!("restricted reader owned opening frame exceeded its admitted share");
        }
        self.state = ReaderState::Opening;
        poll_fn(|context| self.poll_connect(context)).await?;
        self.state = ReaderState::SettingUp;
        let result = self.setup().await;
        match result {
            Ok(()) => {
                self.state = ReaderState::Ready;
                Ok(())
            }
            Err(error) => {
                self.state = ReaderState::SetupFailed;
                Err(error)
            }
        }
    }

    async fn setup(&mut self) -> anyhow::Result<()> {
        #[cfg(test)]
        if let Some(marker) = &self.setup_entered {
            marker.store(true, std::sync::atomic::Ordering::Release);
        }
        #[cfg(test)]
        match self.setup_fault.take() {
            Some(SetupFault::Pending) => std::future::pending::<()>().await,
            Some(SetupFault::Error) => anyhow::bail!("injected restricted reader setup failure"),
            Some(SetupFault::Panic) => panic!("injected restricted reader setup panic"),
            None => {}
        }
        let reader = self
            .reader
            .as_mut()
            .context("restricted reader connection missing")?;
        sqlx::query("BEGIN").execute(&mut *reader).await?;
        let _: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema")
            .fetch_one(&mut *reader)
            .await?;
        {
            let mut lease = reader.lock_handle().await?;
            // SAFETY: unchanged static default-deny callback, exclusively held
            // reader; no borrowed state and no source-writer hook mutation.
            ensure!(
                unsafe {
                    ffi::sqlite3_set_authorizer(
                        lease.as_raw_handle().as_ptr(),
                        Some(authorize),
                        std::ptr::null_mut(),
                    )
                } == ffi::SQLITE_OK,
                "retained authorization authorizer installation failed"
            );
        }
        super::bounded_main_path(reader, false).await?;
        self.verify_identity()
    }

    fn poll_connect(&mut self, context: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.state == ReaderState::Connected {
            return Poll::Ready(Ok(()));
        }
        if self.state != ReaderState::Opening {
            return Poll::Ready(Err(anyhow::anyhow!(
                "restricted reader acquisition is terminal and unresolved: {:?}",
                self.state
            )));
        }
        self.state = ReaderState::PollingConnect;
        match self
            .opening
            .as_mut()
            .expect("opening state retains exact future")
            .as_mut()
            .poll(context)
        {
            Poll::Pending => {
                self.state = ReaderState::Opening;
                Poll::Pending
            }
            Poll::Ready(Ok(reader)) => {
                // No intervening verification, allocation, or await can strand
                // a returned connection outside this actual retained owner.
                self.reader = Some(reader);
                self.state = ReaderState::Connected;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                // SQLx performs PRAGMAs internally and can drop an established
                // connection before returning Err. Retain the completed box;
                // absence of our reader is NOT a shutdown acknowledgment.
                self.state = ReaderState::ConnectFailed;
                Poll::Ready(Err(error.into()))
            }
        }
    }

    pub(crate) fn reader_mut(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        ensure!(
            self.state == ReaderState::Ready,
            "restricted reader is not active"
        );
        self.reader
            .as_mut()
            .context("restricted reader connection missing")
    }

    /// For fixed backend close owners only: install the returned connection's
    /// actual close future synchronously before any fallible work or await.
    /// FD/path and exact completed opening box stay here. This is irreversible.
    pub(crate) fn take_for_retained_close(&mut self) -> anyhow::Result<SqliteConnection> {
        ensure!(
            matches!(
                self.state,
                ReaderState::Connected
                    | ReaderState::SettingUp
                    | ReaderState::SetupFailed
                    | ReaderState::Ready
            ) && self.reader.is_some(),
            "restricted reader cannot transfer unresolved or already closing acquisition"
        );
        self.state = ReaderState::Transferred;
        Ok(self.reader.take().unwrap())
    }

    /// Resource receipt only. The fixed external owner calls this only after
    /// the matching transferred connection's actual SQLx close returned Ok.
    /// It cannot reopen the reader or grant authorization/effect authority.
    pub(crate) fn mark_transferred_close_joined(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.state == ReaderState::Transferred && self.reader.is_none() && self.close.is_none(),
            "external reader close receipt does not match transferred custody"
        );
        self.state = ReaderState::Joined;
        Ok(())
    }

    /// Marks close intent without polling. Opening cleanup resumes only its
    /// original future in join_close, never setup and never a new connection.
    pub(crate) fn begin_close(&mut self) -> anyhow::Result<()> {
        match self.state {
            ReaderState::Claimed | ReaderState::PreparingConnect => {
                // No opening future was ever made or polled: this is known
                // absence, explicitly distinct from actual close acknowledgment.
                self.state = ReaderState::NeverOpened;
                Ok(())
            }
            ReaderState::Opening => Ok(()),
            ReaderState::Connected
            | ReaderState::SettingUp
            | ReaderState::SetupFailed
            | ReaderState::Ready => {
                let reader = self.take_for_retained_close()?;
                self.close = Some(reader.close());
                self.state = ReaderState::Closing;
                let bytes = std::mem::size_of_val(self.close.as_ref().unwrap().as_ref().get_ref());
                if bytes > MAX_CLOSE_FRAME {
                    self.state = ReaderState::CloseOversized;
                    anyhow::bail!("restricted reader close frame exceeded its admitted share");
                }
                Ok(())
            }
            _ => anyhow::bail!("restricted reader close cannot retry: {:?}", self.state),
        }
    }

    pub(crate) async fn join_close(&mut self) -> anyhow::Result<()> {
        if self.state == ReaderState::Opening {
            poll_fn(|context| self.poll_connect(context)).await?;
            self.begin_close()?;
        }
        poll_fn(|context| self.poll_close(context)).await
    }

    fn poll_close(&mut self, context: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        match self.state {
            ReaderState::Joined | ReaderState::NeverOpened => return Poll::Ready(Ok(())),
            ReaderState::Closing => {}
            _ => {
                return Poll::Ready(Err(anyhow::anyhow!(
                    "restricted reader closure is terminal or unresolved: {:?}",
                    self.state
                )));
            }
        }
        self.state = ReaderState::PollingClose;
        match self
            .close
            .as_mut()
            .expect("closing state retains exact future")
            .as_mut()
            .poll(context)
        {
            Poll::Pending => {
                self.state = ReaderState::Closing;
                Poll::Pending
            }
            Poll::Ready(Ok(())) => {
                self.state = ReaderState::Joined;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.state = ReaderState::CloseFailed;
                Poll::Ready(Err(error.into()))
            }
        }
    }

    pub(crate) async fn settle_inline(&mut self) -> anyhow::Result<()> {
        if matches!(
            self.state,
            ReaderState::Claimed
                | ReaderState::PreparingConnect
                | ReaderState::Opening
                | ReaderState::Connected
                | ReaderState::SettingUp
                | ReaderState::SetupFailed
                | ReaderState::Ready
        ) {
            self.begin_close()?;
        }
        self.join_close().await
    }

    pub(crate) fn retains_reader(&self) -> bool {
        self.reader.is_some()
    }
    pub(crate) fn retains_acquisition(&self) -> bool {
        self.opening.is_some()
    }
    pub(crate) fn retains_close(&self) -> bool {
        self.close.is_some()
    }
    pub(crate) fn close_joined(&self) -> bool {
        self.state == ReaderState::Joined
    }
    pub(crate) fn is_settled(&self) -> bool {
        matches!(self.state, ReaderState::Joined | ReaderState::NeverOpened)
    }

    #[cfg(test)]
    pub(crate) fn set_setup_fault(
        &mut self,
        fault: SetupFault,
    ) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        let marker = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.setup_fault = Some(fault);
        self.setup_entered = Some(marker.clone());
        marker
    }
    #[cfg(test)]
    pub(crate) fn has_connection(&self) -> bool {
        self.reader.is_some()
    }
    #[cfg(test)]
    pub(crate) fn has_file(&self) -> bool {
        self.file.is_some()
    }
    #[cfg(test)]
    pub(crate) fn file_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.file.as_ref().unwrap().as_raw_fd()
    }
    #[cfg(test)]
    pub(crate) fn connect_address(&self) -> *const () {
        std::ptr::from_ref(self.opening.as_ref().unwrap().as_ref().get_ref()).cast::<()>()
    }
    #[cfg(test)]
    pub(crate) fn close_address(&self) -> *const () {
        std::ptr::from_ref(self.close.as_ref().unwrap().as_ref().get_ref()).cast::<()>()
    }
}

fn ensure_sqlx_connect_frame(
    future: &(dyn Future<Output = Result<SqliteConnection, sqlx::Error>> + Send),
) -> Result<(), sqlx::Error> {
    if std::mem::size_of_val(future) > MAX_CONNECT_FRAME / 2 {
        return Err(sqlx::Error::Protocol(
            "restricted SQLx opening frame exceeded admitted share".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
