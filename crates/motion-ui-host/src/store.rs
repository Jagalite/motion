//! Device-side cache writer. Transfers stage bytes under `staging/`; only
//! publication verifies them and makes them visible. Every operation holds
//! the cache's exclusive lock, so it never runs beside the presentation host
//! (whose admitted streams are private snapshots in any case) or another
//! writer. Publication order: verified blob rename and directory fsync, then
//! the manifest replacement and its directory fsync. A crash in between
//! leaves an unreferenced blob that the next write collects.
use crate::{
    CacheManifest, EventLog, MAX_DOWNLOADS, MAX_LOG, bounded, replace_durably, validate_download,
    validate_manifest,
};
use anyhow::{Context, bail, ensure};
use cap_std::fs::Dir;
use fs2::FileExt;
use motion_ui::offline::CachedDownload;
use playscale_core::offline::{self, CacheScope};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub struct Store {
    path: PathBuf,
    root: Dir,
    root_sync: std::fs::File,
    blobs: Dir,
    blobs_sync: std::fs::File,
    staging: Dir,
    _lock: std::fs::File,
    manifest: CacheManifest,
    /// Downloads the person removed on this device whose deletion the server
    /// has not confirmed yet (`removals.json`): never published again
    /// (`offline::may_publish`) until settled (`offline::settles`).
    removals: Vec<String>,
}

/// At most this many unsettled removals are kept.
const MAX_REMOVALS: usize = 500;
const MAX_REMOVALS_BYTES: u64 = 128 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    Published,
    /// The same download and digest were already published.
    AlreadyPublished,
}

/// A readable handle on `name` inside `parent` for fsync, checked to be the
/// directory the capability handle refers to.
fn sync_handle(path: &Path, dir: &Dir) -> anyhow::Result<std::fs::File> {
    let handle = std::fs::File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (dir.dir_metadata()?, handle.metadata()?);
        ensure!(
            b.is_dir()
                && cap_std::fs::MetadataExt::dev(&a) == b.dev()
                && cap_std::fs::MetadataExt::ino(&a) == b.ino(),
            "cache directory changed while opening"
        );
    }
    Ok(handle)
}

fn subdirectory(root: &Dir, path: &Path, name: &str) -> anyhow::Result<(Dir, std::fs::File)> {
    match root.create_dir(name) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let dir = root.open_dir(name)?;
    let handle = sync_handle(&path.join(name), &dir)?;
    Ok((dir, handle))
}

/// The staging file name for a download's transfer.
pub fn staging_name(download_id: &str) -> anyhow::Result<String> {
    ensure!(
        offline::identifier(download_id),
        "invalid download identity"
    );
    Ok(format!("{download_id}.part"))
}

impl Store {
    /// Open (creating when absent) the cache root for exactly `scope`.
    pub fn open(path: &Path, scope: &CacheScope) -> anyhow::Result<Self> {
        Self::open_with(path, scope, None)
    }

    /// Open the root as the writer that already holds its exclusive lock:
    /// `lock` is a handle on this root's `events.lock` whose lock the caller
    /// (the running offline host) holds, so no other writer can run. Like
    /// `open`, it replays pending removals best effort.
    pub fn open_held(path: &Path, scope: &CacheScope, lock: std::fs::File) -> anyhow::Result<Self> {
        Self::open_with(path, scope, Some(lock))
    }

    fn open_with(
        path: &Path,
        scope: &CacheScope,
        held: Option<std::fs::File>,
    ) -> anyhow::Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(path)?;
        let root = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let root_sync = sync_handle(path, &root)?;
        let lock = match held {
            Some(lock) => {
                // The handle must be this root's lock file, not any file.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let (a, b) = (root.metadata("events.lock")?, lock.metadata()?);
                    ensure!(
                        cap_std::fs::MetadataExt::dev(&a) == b.dev()
                            && cap_std::fs::MetadataExt::ino(&a) == b.ino(),
                        "the held lock is not this cache's"
                    );
                }
                lock
            }
            None => {
                let lock = root
                    .open_with(
                        "events.lock",
                        cap_std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true),
                    )?
                    .into_std();
                lock.try_lock_exclusive()
                    .context("the offline cache is in use")?;
                lock
            }
        };
        let (blobs, blobs_sync) = subdirectory(&root, path, "blobs")?;
        let (staging, _) = subdirectory(&root, path, "staging")?;
        let manifest = match root.open("manifest.json") {
            Ok(file) => {
                let manifest: CacheManifest =
                    serde_json::from_slice(&bounded(file, crate::MAX_MANIFEST)?)?;
                validate_manifest(&manifest)?;
                ensure!(
                    &manifest.scope == scope,
                    "the cache root belongs to another server, principal, profile or device"
                );
                manifest
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let manifest = CacheManifest {
                    protocol: 1,
                    scope: scope.clone(),
                    downloads: vec![],
                };
                validate_manifest(&manifest)?;
                replace_durably(
                    &root,
                    &root_sync,
                    "manifest.json",
                    &serde_json::to_vec(&manifest)?,
                )?;
                manifest
            }
            Err(e) => return Err(e.into()),
        };
        let removals: Vec<String> = match root.open("removals.json") {
            Ok(file) => serde_json::from_slice(&bounded(file, MAX_REMOVALS_BYTES)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        ensure!(
            removals.len() <= MAX_REMOVALS && removals.iter().all(|id| offline::identifier(id)),
            "invalid removal record"
        );
        let mut store = Self {
            path: path.to_owned(),
            root,
            root_sync,
            blobs,
            blobs_sync,
            staging,
            _lock: lock,
            manifest,
            removals,
        };
        // A removal whose intent is durable but whose copy a crash left in
        // place is completed now, best effort: whatever fails is retried at
        // the next open, and meanwhile the title is neither published nor
        // presented (`offline::may_publish` over the durable intents).
        let _ = store.replay_removals();
        Ok(store)
    }

    /// Complete every pending removal whose copy is still here.
    pub fn replay_removals(&mut self) -> anyhow::Result<()> {
        for id in self.removals.clone() {
            self.remove(&id)?;
        }
        Ok(())
    }

    pub fn manifest(&self) -> &CacheManifest {
        &self.manifest
    }

    /// Removals the server has not confirmed yet.
    pub fn removals(&self) -> &[String] {
        &self.removals
    }

    fn write_removals(&mut self, removals: Vec<String>) -> anyhow::Result<()> {
        ensure!(
            removals.len() <= MAX_REMOVALS,
            "too many removals await the server"
        );
        replace_durably(
            &self.root,
            &self.root_sync,
            "removals.json",
            &serde_json::to_vec(&removals)?,
        )?;
        self.removals = removals; // Only after the durable rename.
        Ok(())
    }

    /// The person removes a saved download from this device. The intent is
    /// durable before the copy goes, so a crash in between leaves a removal
    /// the next pass completes, never a copy the agent saves again.
    /// Offline progress already recorded for it is kept.
    pub fn remove_by_person(&mut self, download_id: &str) -> anyhow::Result<bool> {
        ensure!(
            offline::identifier(download_id),
            "invalid download identity"
        );
        let saved = self
            .manifest
            .downloads
            .iter()
            .any(|d| d.identity.download_id == download_id)
            || self.staged()?.iter().any(|id| id == download_id);
        if !saved {
            return Ok(false);
        }
        if offline::may_publish(download_id, &self.removals) {
            let mut next = self.removals.clone();
            next.push(download_id.to_owned());
            self.write_removals(next)?;
        }
        self.remove(download_id)?;
        Ok(true)
    }

    /// Settle a pending removal with the server's answer to deleting the
    /// download and whether its copy is gone here (`offline::settles`).
    /// Returns whether it is now settled.
    pub fn settle_removal(
        &mut self,
        download_id: &str,
        outcome: offline::Deletion,
    ) -> anyhow::Result<bool> {
        if offline::may_publish(download_id, &self.removals) {
            return Ok(true);
        }
        // Complete the local removal first (best effort); the intent stays
        // while any copy remains (`offline::settles`).
        let _ = self.remove(download_id);
        let saved = self
            .manifest
            .downloads
            .iter()
            .any(|d| d.identity.download_id == download_id)
            || self.staged()?.iter().any(|id| id == download_id);
        if !offline::settles(outcome, saved) {
            return Ok(false);
        }
        let next = self
            .removals
            .iter()
            .filter(|id| *id != download_id)
            .cloned()
            .collect();
        self.write_removals(next)?;
        Ok(true)
    }

    /// Downloads with a staged (unpublished) transfer. These are lifecycle
    /// facts too: a cancelled or expired demand must also release them.
    pub fn staged(&self) -> anyhow::Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in self.staging.entries()? {
            let name = entry?.file_name();
            if let Some(id) = name.to_string_lossy().strip_suffix(".part")
                && offline::identifier(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Bytes on disk: every blob (each digest once) plus staged partial transfers.
    pub fn usage(&self) -> anyhow::Result<u64> {
        // Every blob on disk once (shared digests are one file), including
        // verified crash leftovers a ready demand may still recover.
        let mut used = 0u64;
        for entry in self.blobs.entries()? {
            used = used.saturating_add(entry?.metadata()?.len());
        }
        for entry in self.staging.entries()? {
            used = used.saturating_add(entry?.metadata()?.len());
        }
        Ok(used)
    }

    /// Whether `incoming` more bytes fit the quota and leave `headroom` free.
    pub fn admits_transfer(
        &self,
        incoming: u64,
        quota: u64,
        headroom: u64,
    ) -> anyhow::Result<bool> {
        Ok(offline::admits_transfer(
            self.usage()?,
            incoming,
            quota,
            fs2::available_space(&self.path)?,
            headroom,
        ))
    }

    fn write_manifest(&mut self, manifest: CacheManifest) -> anyhow::Result<()> {
        validate_manifest(&manifest)?;
        let bytes = serde_json::to_vec(&manifest)?;
        ensure!(
            bytes.len() as u64 <= crate::MAX_MANIFEST,
            "cache manifest is full"
        );
        replace_durably(&self.root, &self.root_sync, "manifest.json", &bytes)?;
        self.manifest = manifest; // Only after the durable rename.
        Ok(())
    }

    fn verify(file: &mut cap_std::fs::File, size: u64, sha256: &str) -> anyhow::Result<()> {
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() == size,
            "transfer size does not match the manifest"
        );
        let mut hash = Sha256::new();
        let mut read = 0u64;
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read += count as u64;
            ensure!(read <= size, "transfer grew during verification");
            hash.update(&buffer[..count]);
        }
        ensure!(
            read == size && format!("{:x}", hash.finalize()) == sha256,
            "transfer checksum mismatch"
        );
        Ok(())
    }

    /// Verify the staged transfer of `entry` and publish it atomically.
    /// A failed verification removes the staged bytes and changes nothing
    /// visible; an exact repeat is idempotent.
    pub fn publish(&mut self, entry: CachedDownload) -> anyhow::Result<Publication> {
        validate_download(&entry)?;
        let id = entry.identity.download_id.clone();
        ensure!(
            offline::may_publish(&id, &self.removals),
            "this download was removed on this device"
        );
        let staged = staging_name(&id)?;
        if self.manifest.downloads.iter().any(|d| {
            d.identity.download_id == id && d.sha256 == entry.sha256 && d.size == entry.size
        }) {
            let _ = self.staging.remove_file(&staged);
            return Ok(Publication::AlreadyPublished);
        }
        match self.staging.open(&staged) {
            Ok(mut file) => {
                if let Err(e) = Self::verify(&mut file, entry.size, &entry.sha256) {
                    drop(file);
                    self.staging.remove_file(&staged)?;
                    return Err(e);
                }
                file.sync_all()?;
                self.staging.rename(&staged, &self.blobs, &entry.sha256)?;
                self.blobs_sync.sync_all()?;
            }
            // A crash after the blob rename: verify the blob already in place.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut blob = self
                    .blobs
                    .open(&entry.sha256)
                    .context("no staged transfer for this download")?;
                Self::verify(&mut blob, entry.size, &entry.sha256)?;
            }
            Err(e) => return Err(e.into()),
        }
        let mut next = self.manifest.clone();
        let replaced = next
            .downloads
            .iter()
            .find(|d| d.identity.download_id == id)
            .map(|d| d.sha256.clone());
        next.downloads.retain(|d| d.identity.download_id != id);
        ensure!(
            next.downloads.len() < MAX_DOWNLOADS,
            "the cache holds the maximum number of downloads"
        );
        next.downloads.push(entry);
        self.write_manifest(next)?;
        // Only bytes this publication superseded; other unreferenced blobs
        // may still be recovered by their own ready demand (see `collect`).
        if let Some(old) = replaced {
            self.release(&old)?;
        }
        Ok(Publication::Published)
    }

    /// Remove a download's entry, staged bytes and (when unshared) its blob.
    /// Removal only affects this device; it cannot reach other copies.
    pub fn remove(&mut self, download_id: &str) -> anyhow::Result<bool> {
        let staged = staging_name(download_id)?;
        match self.staging.remove_file(&staged) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut next = self.manifest.clone();
        let blob = next
            .downloads
            .iter()
            .find(|d| d.identity.download_id == download_id)
            .map(|d| d.sha256.clone());
        next.downloads
            .retain(|d| d.identity.download_id != download_id);
        let removed = next.downloads.len() != self.manifest.downloads.len();
        if removed {
            self.write_manifest(next)?;
        }
        if let Some(blob) = blob {
            self.release(&blob)?;
        }
        Ok(removed)
    }

    /// Delete one blob a write just stopped referencing, unless another
    /// entry still shares it.
    fn release(&self, sha256: &str) -> anyhow::Result<()> {
        if self.manifest.downloads.iter().any(|d| d.sha256 == sha256) {
            return Ok(());
        }
        match self.blobs.remove_file(sha256) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.blobs_sync.sync_all()?;
        Ok(())
    }

    /// Delete every blob no manifest entry references. Run only after each
    /// still-ready demand had its chance to `recover` a verified leftover;
    /// publication and removal never collect globally.
    pub fn collect(&self) -> anyhow::Result<()> {
        for entry in self.blobs.entries()? {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            if !self.manifest.downloads.iter().any(|d| d.sha256 == name) {
                self.blobs.remove_file(name.as_ref())?;
            }
        }
        self.blobs_sync.sync_all()?;
        Ok(())
    }

    pub fn events(&self) -> anyhow::Result<EventLog> {
        let log = match self.root.open("events.json") {
            Ok(file) => EventLog::parse(&bounded(file, MAX_LOG)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => EventLog::default(),
            Err(e) => return Err(e.into()),
        };
        log.validate(&self.manifest.scope)?;
        Ok(log)
    }

    /// The re-anchor the next batch (sent under its first event's history)
    /// must carry (`offline::reanchor`): the log's compaction point belongs
    /// to another history of the server, which a restored server never saw.
    pub fn pending_reanchor(&self) -> anyhow::Result<Option<offline::Reanchor>> {
        let log = self.events()?;
        Ok(log
            .events
            .first()
            .and_then(|e| e.media.viewing_epoch.as_deref())
            .and_then(|epoch| {
                offline::reanchor(log.base_sequence, log.base_viewing_epoch.as_deref(), epoch)
            }))
    }

    /// Compact the events the server acknowledged through `through` (its
    /// `next_device_sequence - 1`). An acknowledgement beyond anything this
    /// log wrote is refused and erases nothing.
    pub fn acknowledge(&self, through: u64) -> anyhow::Result<EventLog> {
        let log = self.events()?;
        let Some((base_sequence, events)) =
            offline::compact(log.base_sequence, &log.events, through)
        else {
            bail!("the server acknowledged sequences this device never wrote");
        };
        let next = EventLog {
            base_viewing_epoch: offline::compacted_epoch(
                log.base_viewing_epoch.as_deref(),
                &log.events,
                log.base_sequence,
                base_sequence,
            ),
            base_sequence,
            events,
        };
        if next != log {
            replace_durably(
                &self.root,
                &self.root_sync,
                "events.json",
                &serde_json::to_vec(&next)?,
            )?;
        }
        Ok(next)
    }

    /// Compact after a sync from the server's results for the leading
    /// `results.len()` retained events (`offline::acknowledged_through`).
    pub fn acknowledge_results(
        &mut self,
        results: &[offline::Acknowledgement],
        next_device_sequence: u64,
    ) -> anyhow::Result<EventLog> {
        let log = self.events()?;
        let through = offline::acknowledged_through(
            log.base_sequence,
            &log.events,
            results,
            next_device_sequence,
        );
        // The offline host resumes from the newest retained event; keep that
        // checkpoint in the manifest before compaction removes the events.
        // The causal base is unchanged: only a rebase after reading the
        // server may move it.
        let mut next = self.manifest.clone();
        let mut changed = false;
        for entry in &mut next.downloads {
            let latest = log.events.iter().rev().find(|e| {
                e.media.download_id == entry.identity.download_id
                    && offline::sequence(&e.device_sequence).is_some_and(|s| s <= through)
            });
            if let Some(e) = latest {
                let resume = (e.status != "ended" && e.position_ms < entry.duration_ms)
                    .then_some(e.position_ms)
                    .filter(|p| *p > 0);
                if entry.resume_position_ms != resume {
                    entry.resume_position_ms = resume;
                    changed = true;
                }
            }
        }
        if changed {
            self.write_manifest(next)?;
        }
        self.acknowledge(through)
    }

    /// Publish `entry` from a blob already verified into place by a
    /// publication that crashed before its manifest write, without staging
    /// or transferring again. Returns whether the download is now published.
    pub fn recover(&mut self, entry: CachedDownload) -> anyhow::Result<bool> {
        validate_download(&entry)?;
        ensure!(
            offline::may_publish(&entry.identity.download_id, &self.removals),
            "this download was removed on this device"
        );
        let staged = staging_name(&entry.identity.download_id)?;
        let staged_exists = match self.staging.metadata(&staged) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        let blob_exists = match self.blobs.metadata(&entry.sha256) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if staged_exists || !blob_exists {
            return Ok(false);
        }
        let mut blob = self.blobs.open(&entry.sha256)?;
        if Self::verify(&mut blob, entry.size, &entry.sha256).is_err() {
            drop(blob);
            // Invalid leftover bytes are discarded (unless another published
            // entry still names them) so the next pass transfers afresh.
            if !self
                .manifest
                .downloads
                .iter()
                .any(|d| d.sha256 == entry.sha256)
            {
                self.blobs.remove_file(&entry.sha256)?;
                self.blobs_sync.sync_all()?;
            }
            return Ok(false);
        }
        self.publish(entry)?;
        Ok(true)
    }

    /// Pin later offline events of a download to the server's current
    /// viewing state, read after reconciliation, and remember its position
    /// for offline resume once local events were compacted.
    pub fn rebase(
        &mut self,
        download_id: &str,
        viewing_revision: &str,
        manual_epoch: &str,
        viewing_epoch: &str,
        resume_position_ms: Option<u64>,
    ) -> anyhow::Result<bool> {
        ensure!(
            offline::sequence(viewing_revision).is_some()
                && offline::sequence(manual_epoch).is_some()
                && offline::viewing_epoch(viewing_epoch),
            "invalid causal revision"
        );
        let mut next = self.manifest.clone();
        let Some(entry) = next
            .downloads
            .iter_mut()
            .find(|d| d.identity.download_id == download_id)
        else {
            return Ok(false);
        };
        let resume = resume_position_ms.filter(|p| *p < entry.duration_ms);
        if entry.identity.base_viewing_revision == viewing_revision
            && entry.identity.base_manual_epoch == manual_epoch
            && entry.identity.viewing_epoch.as_deref() == Some(viewing_epoch)
            && entry.resume_position_ms == resume
        {
            return Ok(false);
        }
        entry.identity.base_viewing_revision = viewing_revision.into();
        entry.identity.base_manual_epoch = manual_epoch.into();
        entry.identity.viewing_epoch = Some(viewing_epoch.into());
        entry.resume_position_ms = resume;
        self.write_manifest(next)?;
        Ok(true)
    }
}
