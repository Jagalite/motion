//! Create-only evidence for the fixed93→97 control operation. A synced record
//! can survive a rolled-back database transaction: it NEVER proves completion.
//! Pending93 plus an existing record must stop for reconciliation, not choose
//! different timestamps or overwrite the record. A future reviewed retry must
//! replay these exact additions. Completed97 recovery separately reauthenticates
//! the original93 image and proves original rows plus this precise delta.
use super::reference::ClosedReference;
use crate::motion_snapshot::row_preservation::ControlAdditions;
use anyhow::{Context, ensure};
use rusqlite::types::Value;
use serde::{
    Deserialize, Serialize,
    ser::{SerializeSeq, SerializeStruct},
};
use std::{
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, MetadataExt},
    },
};

pub(super) const FILE_NAME: &str = "motion-control-upgrade93-97.json";
const NAME: &std::ffi::CStr = c"motion-control-upgrade93-97.json";
const MAX_BYTES: usize = 256 * 1024;
const MAX_CELL_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordAnchor {
    pub(super) operation_id: String,
    pub(super) selected_pair_sha256: String,
    pub(super) original93_image_sha256: String,
    pub(super) original93_inventory_sha256: String,
    pub(super) stage_device: u64,
    pub(super) stage_inode: u64,
    pub(super) frozen_binding_sha256: String,
}

// These are the only storage classes present in the seven authorized appended
// rows. In particular, JSON numbers never coerce SQL text, and hex text never
// stands in for a checksum BLOB. NULL and REAL are invalid for this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Cell {
    Integer(i64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    contract_version: u32,
    anchor: RecordAnchor,
    ledger: Vec<Vec<Cell>>,
    guards: Vec<Vec<Cell>>,
}

pub(super) struct PreparedRecord {
    stored: Stored,
    bytes: Vec<u8>,
}

impl PreparedRecord {
    /// The caller adds this named reservation to its existing 256 MiB cohort
    /// BEFORE cloning the anchor or calling `new`. There is no independent
    /// publication allowance. Successful sizing borrows all authority inputs
    /// and counts their actual canonical representation without building it.
    ///
    /// Containers have explicit capacities below. Publication moves payloads
    /// from Cell to Value, so only their containers overlap, not string/blob
    /// clones. Both exact-sized verification read buffers are charged even
    /// though their current reads are sequential. Original caller allocations
    /// and any caller-created clones remain in the caller's existing cohort.
    pub(super) fn storage_reservation(
        anchor: &RecordAnchor,
        additions: &ControlAdditions,
        reference: &ClosedReference,
        guards: &[(String, (String, String))],
    ) -> anyhow::Result<u64> {
        validate_anchor(anchor)?;
        validate_additions(additions, reference, guards)?;
        let mut total = 0u64;
        let mut add = |count: usize, size: usize| -> anyhow::Result<()> {
            let bytes = count
                .checked_mul(size)
                .context("control publication allocation overflow")?;
            total = total
                .checked_add(u64::try_from(bytes)?)
                .context("control publication allocation overflow")?;
            Ok(())
        };
        // Inline owners plus move/conversion temporaries are included rather
        // than treating their structs as zero-sized bookkeeping.
        add(1, std::mem::size_of::<RetainedRecordPublication>())?;
        add(1, std::mem::size_of::<PreparedRecord>())?;
        add(1, std::mem::size_of::<Stored>())?;
        add(1, std::mem::size_of::<ControlAdditions>())?;
        // The completed owner is first assembled as a move temporary before
        // entering its inline slot. Its anchor and descriptor transfer also
        // have named local owners; none of these terms stand in for payloads.
        add(1, std::mem::size_of::<DurableRecord>())?;
        add(1, std::mem::size_of::<RecordAnchor>())?;
        add(1, std::mem::size_of::<(Option<File>, Option<File>)>())?;
        add(1, std::mem::size_of::<(File, File)>())?;
        // Two publication observations remain live while read_bounded holds
        // its before/after observations. Reserve the nested regular/parent
        // metadata result separately instead of assuming return-slot reuse.
        add(2, std::mem::size_of::<std::fs::Metadata>())?;
        add(2, std::mem::size_of::<std::fs::Metadata>())?;
        add(1, std::mem::size_of::<std::fs::Metadata>())?;
        // Canonical-owner and two verification-buffer headers. Their exact
        // backing allocations are charged separately by encoded length below.
        add(3, std::mem::size_of::<Vec<u8>>())?;
        // Explicit encoding/conversion owners and iterators coexist with the
        // source Stored and destination ControlAdditions during payload moves.
        add(1, std::mem::size_of::<Vec<Vec<Cell>>>())?;
        add(1, std::mem::size_of::<Vec<Cell>>())?;
        add(1, std::mem::size_of::<Vec<Vec<Value>>>())?;
        add(1, std::mem::size_of::<Vec<Value>>())?;
        add(1, std::mem::size_of::<std::vec::IntoIter<Vec<Cell>>>())?;
        add(1, std::mem::size_of::<std::vec::IntoIter<Cell>>())?;
        add(1, std::mem::size_of::<Cell>())?;
        add(1, std::mem::size_of::<Value>())?;
        add(1, std::mem::size_of::<BorrowedStored<'_>>())?;
        for value in [
            &anchor.operation_id,
            &anchor.selected_pair_sha256,
            &anchor.original93_image_sha256,
            &anchor.original93_inventory_sha256,
            &anchor.frozen_binding_sha256,
        ] {
            // A caller may either clone or move this anchor into `new`.
            add(1, value.capacity())?;
        }
        for rows in [&additions.ledger, &additions.guards] {
            add(rows.len(), std::mem::size_of::<Vec<Cell>>())?;
            add(rows.len(), std::mem::size_of::<Vec<Value>>())?;
            for row in rows {
                add(row.len(), std::mem::size_of::<Cell>())?;
                add(row.len(), std::mem::size_of::<Value>())?;
                for value in row {
                    match value {
                        Value::Integer(_) => (),
                        Value::Text(value) if value.len() <= MAX_CELL_BYTES => {
                            add(1, value.len())?;
                        }
                        Value::Blob(value) if value.len() <= MAX_CELL_BYTES => {
                            add(1, value.len())?;
                        }
                        _ => anyhow::bail!("unsupported control record cell storage or bound"),
                    }
                }
            }
        }
        let encoded = canonical_length(&BorrowedStored {
            contract_version: 1,
            anchor,
            ledger: BorrowedRows(&additions.ledger),
            guards: BorrowedRows(&additions.guards),
        })?;
        // One canonical owner and two bounded verification buffers.
        add(3, encoded)?;
        Ok(total)
    }

    pub(super) fn new(
        anchor: RecordAnchor,
        additions: &ControlAdditions,
        reference: &ClosedReference,
        guards: &[(String, (String, String))],
    ) -> anyhow::Result<Self> {
        validate_anchor(&anchor)?;
        // Validate dimensions and values before cloning caller-provided cells.
        validate_additions(additions, reference, guards)?;
        let stored = Stored {
            contract_version: 1,
            anchor,
            ledger: encode_rows(&additions.ledger)?,
            guards: encode_rows(&additions.guards)?,
        };
        let bytes = canonical_bytes(&stored)?;
        Ok(Self { stored, bytes })
    }
}

/// The caller supplies its retained, no-follow opened reservation directory and
/// keeps the supported operation/server lock. No parent pathname is reopened.
/// This does not defend against hostile privileged or same-process ABA changes.
pub(super) struct RecordDirectory {
    parent: File,
    identity: (u64, u64),
}

impl RecordDirectory {
    #[cfg(test)]
    pub(super) fn original_parent_fd(&self) -> std::os::fd::RawFd {
        self.parent.as_raw_fd()
    }

    pub(super) fn from_retained(parent: File) -> anyhow::Result<Self> {
        let identity = Self::validate_retained(&parent)?;
        Ok(Self::from_validated(parent, identity))
    }

    /// Check through a borrow while the enclosing operation retains its owner.
    pub(super) fn validate_retained(parent: &File) -> anyhow::Result<(u64, u64)> {
        let metadata = parent.metadata()?;
        ensure!(
            metadata.is_dir() && metadata.nlink() > 0,
            "control record requires retained existing directory"
        );
        Ok((metadata.dev(), metadata.ino()))
    }

    /// Pure move following `validate_retained` of this exact retained File.
    pub(super) fn from_validated(parent: File, identity: (u64, u64)) -> Self {
        Self { identity, parent }
    }

    fn verify_parent(&self) -> anyhow::Result<()> {
        verify_parent(&self.parent, self.identity)
    }

    /// Pending93 must refuse any existing operation evidence before choosing
    /// new timestamps or applying effects. The later O_EXCL create remains the
    /// final create-only check; absence here is never a completion observation.
    pub(super) fn require_absent(&self) -> anyhow::Result<()> {
        self.verify_parent()?;
        ensure!(
            read_at(&self.parent)?.is_none(),
            "existing control operation record requires exact reconciliation"
        );
        self.verify_parent()
    }

    pub(super) fn load_exact(
        &self,
        anchor: &RecordAnchor,
        reference: &ClosedReference,
        guards: &[(String, (String, String))],
    ) -> anyhow::Result<Option<DurableRecord>> {
        validate_anchor(anchor)?;
        self.verify_parent()?;
        let Some(file) = read_at(&self.parent)? else {
            return Ok(None);
        };
        let bytes = read_bounded(&file, None)?;
        let stored: Stored = serde_json::from_slice(&bytes)?;
        ensure!(
            canonical_bytes(&stored)? == bytes,
            "control operation record is not canonical"
        );
        ensure!(
            stored.contract_version == 1,
            "unsupported control record contract"
        );
        ensure!(
            &stored.anchor == anchor,
            "control operation record anchor differs"
        );
        validate_anchor(&stored.anchor)?;
        let additions = decode_additions(&stored)?;
        validate_additions(&additions, reference, guards)?;
        let durable = self.owner(file, bytes, stored.anchor, additions)?;
        durable.verify()?;
        // A valid record may have survived a crash before the original fsync.
        // Establish durability now, without interpreting it as database commit.
        durable.file.sync_all()?;
        durable.parent.sync_all()?;
        durable.verify()?;
        Ok(Some(durable))
    }

    pub(super) fn create_only(&self, prepared: &PreparedRecord) -> anyhow::Result<DurableRecord> {
        self.create_inner(prepared, |_| Ok(()))
    }

    fn create_inner(
        &self,
        prepared: &PreparedRecord,
        checkpoint: impl Fn(u8) -> anyhow::Result<()>,
    ) -> anyhow::Result<DurableRecord> {
        self.verify_parent()?;
        ensure!(
            !prepared.bytes.is_empty() && prepared.bytes.len() <= MAX_BYTES,
            "control operation record exceeds bound"
        );
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                NAME.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::mode_t,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context(
                "control record create-only open failed; existing evidence is never replaced",
            );
        }
        // SAFETY: successful openat transferred this new owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        // Every failure from this point deliberately retains the named file.
        // Partial evidence blocks replacement and requires reconciliation.
        checkpoint(0)?;
        file.write_all(&prepared.bytes)?;
        checkpoint(1)?;
        file.sync_all()?;
        checkpoint(2)?;
        self.parent.sync_all()?;
        checkpoint(3)?;
        let durable = self.owner(
            file,
            prepared.bytes.clone(),
            prepared.stored.anchor.clone(),
            decode_additions(&prepared.stored)?,
        )?;
        durable.verify()?;
        Ok(durable)
    }

    fn owner(
        &self,
        file: File,
        bytes: Vec<u8>,
        anchor: RecordAnchor,
        additions: ControlAdditions,
    ) -> anyhow::Result<DurableRecord> {
        self.verify_parent()?;
        let metadata = regular(&file)?;
        Ok(DurableRecord {
            parent: self.parent.try_clone()?,
            parent_identity: self.identity,
            file_identity: (metadata.dev(), metadata.ino()),
            file,
            bytes,
            anchor,
            additions,
        })
    }
}

/// Single-attempt publication owned inline by the enclosing operation. Unlike
/// the legacy create-only helper, every acquired descriptor stays in this owner
/// across errors and unwinding. No failure adopts, deletes, or retries evidence.
pub(super) struct RetainedRecordPublication {
    prepared: Option<PreparedRecord>,
    file: Option<File>,
    parent: Option<File>,
    verification_file: Option<File>,
    durable: Option<DurableRecord>,
    phase: PublicationPhase,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationPhase {
    Prepared,
    CreateAttempted,
    Created,
    Written,
    FileSynced,
    ParentSynced,
    Verified,
    Taken,
}

impl RetainedRecordPublication {
    /// Moves only: no allocation, descriptor duplication, or validation.
    pub(super) fn new(prepared: PreparedRecord) -> Self {
        Self {
            prepared: Some(prepared),
            file: None,
            parent: None,
            verification_file: None,
            durable: None,
            phase: PublicationPhase::Prepared,
        }
    }

    pub(super) fn phase(&self) -> &'static str {
        match self.phase {
            PublicationPhase::Prepared => "prepared",
            PublicationPhase::CreateAttempted => "create attempted",
            PublicationPhase::Created => "created evidence retained",
            PublicationPhase::Written => "record written",
            PublicationPhase::FileSynced => "record fsynced",
            PublicationPhase::ParentSynced => "record and parent fsynced",
            PublicationPhase::Verified => "durable evidence verified",
            PublicationPhase::Taken => "durable evidence transferred",
        }
    }

    pub(super) fn publish(
        &mut self,
        directory: &RecordDirectory,
        cancel: &std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        {
            self.publish_inner(directory, cancel, deadline, |_, _| Ok(()))
        }
        #[cfg(not(test))]
        {
            self.publish_inner(directory, cancel, deadline)
        }
    }

    /// Pure move, available only after every publication check succeeded.
    pub(super) fn take_durable(&mut self) -> Option<DurableRecord> {
        if self.phase != PublicationPhase::Verified {
            return None;
        }
        self.phase = PublicationPhase::Taken;
        self.durable.take()
    }

    #[cfg(test)]
    pub(super) fn original_record_fd(&self) -> Option<std::os::fd::RawFd> {
        self.file
            .as_ref()
            .or_else(|| self.durable.as_ref().map(|durable| &durable.file))
            .map(AsRawFd::as_raw_fd)
    }

    /// Test checkpoints: 0=create, 1=partial write, 2=file fsync,
    /// 3=parent fsync, 4=retained parent clone, 5=retained verification open.
    #[cfg(test)]
    pub(super) fn publish_at_checkpoint(
        &mut self,
        directory: &RecordDirectory,
        cancel: &std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
        checkpoint: impl FnMut(u8, std::os::fd::RawFd) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.publish_inner(directory, cancel, deadline, checkpoint)
    }

    fn publish_inner(
        &mut self,
        directory: &RecordDirectory,
        cancel: &std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
        #[cfg(test)] mut checkpoint: impl FnMut(u8, std::os::fd::RawFd) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == PublicationPhase::Prepared,
            "control record publication is single-attempt; reconciliation required"
        );
        self.phase = PublicationPhase::CreateAttempted;
        directory.verify_parent()?;
        let prepared = self
            .prepared
            .as_ref()
            .context("control publication lost prepared evidence")?;
        ensure!(
            !prepared.bytes.is_empty() && prepared.bytes.len() <= MAX_BYTES,
            "control operation record exceeds bound"
        );
        // Refuse immediately before the fixed-name effect. Once created, do
        // not abandon the required write/fsync sequence due to late expiry.
        super::check(cancel)?;
        super::check_deadline(deadline)?;
        let fd = unsafe {
            libc::openat(
                directory.parent.as_raw_fd(),
                NAME.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::mode_t,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context(
                "control record create-only open failed; existing evidence is never replaced",
            );
        }
        // SAFETY: successful openat transferred this descriptor. Install its
        // owner immediately, before metadata, writes, checkpoints, or allocation.
        self.file = Some(unsafe { File::from_raw_fd(fd) });
        self.phase = PublicationPhase::Created;
        #[cfg(test)]
        checkpoint(0, fd)?;
        let file = self.file.as_mut().context("retained record is missing")?;
        #[cfg(test)]
        {
            let middle = prepared.bytes.len() / 2;
            file.write_all(&prepared.bytes[..middle])?;
            checkpoint(1, fd)?;
            file.write_all(&prepared.bytes[middle..])?;
        }
        #[cfg(not(test))]
        file.write_all(&prepared.bytes)?;
        self.phase = PublicationPhase::Written;
        file.sync_all()?;
        self.phase = PublicationPhase::FileSynced;
        #[cfg(test)]
        checkpoint(2, fd)?;
        directory.parent.sync_all()?;
        self.phase = PublicationPhase::ParentSynced;
        #[cfg(test)]
        checkpoint(3, fd)?;

        directory.verify_parent()?;
        self.parent = Some(directory.parent.try_clone()?);
        #[cfg(test)]
        checkpoint(4, fd)?;
        let parent = self.parent.as_ref().context("retained parent is missing")?;
        verify_parent(parent, directory.identity)?;
        let held = regular(file)?;
        ensure!(
            held.len() == prepared.bytes.len() as u64,
            "held control record size changed"
        );
        let named_fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                NAME.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if named_fd < 0 {
            return Err(io::Error::last_os_error())
                .context("named control record verification open failed");
        }
        // The verifier is also retained before any regular-file validation.
        self.verification_file = Some(unsafe { File::from_raw_fd(named_fd) });
        #[cfg(test)]
        checkpoint(5, fd)?;
        let named = self
            .verification_file
            .as_ref()
            .context("retained verifier is missing")?;
        let named_metadata = regular(named)?;
        ensure!(
            (named_metadata.dev(), named_metadata.ino()) == (held.dev(), held.ino())
                && named_metadata.len() == prepared.bytes.len() as u64,
            "named control record identity or size changed"
        );
        ensure!(
            read_bounded(file, Some(prepared.bytes.len()))? == prepared.bytes
                && read_bounded(named, Some(prepared.bytes.len()))? == prepared.bytes,
            "control operation record bytes changed"
        );
        verify_parent(parent, directory.identity)?;
        directory.verify_parent()?;

        // Any conversion allocation happens while all Files remain installed.
        // Cell payload ownership moves directly; no decoding clone is needed.
        let PreparedRecord { stored, bytes } = self
            .prepared
            .take()
            .context("control publication lost prepared evidence")?;
        let additions = ControlAdditions {
            ledger: move_rows(stored.ledger),
            guards: move_rows(stored.guards),
        };
        let file_identity = (held.dev(), held.ino());
        match (self.parent.take(), self.file.take()) {
            (Some(parent), Some(file)) => {
                self.durable = Some(DurableRecord {
                    parent,
                    parent_identity: directory.identity,
                    file,
                    file_identity,
                    bytes,
                    anchor: stored.anchor,
                    additions,
                });
                self.phase = PublicationPhase::Verified;
                Ok(())
            }
            (parent, file) => {
                // Even an invariant failure restores all descriptor owners
                // before constructing an error. The success arm is moves only.
                self.parent = parent;
                self.file = file;
                anyhow::bail!("control publication lost retained owner")
            }
        }
    }
}

fn move_rows(rows: Vec<Vec<Cell>>) -> Vec<Vec<Value>> {
    let mut moved = Vec::with_capacity(rows.len());
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for cell in row {
            values.push(match cell {
                Cell::Integer(value) => Value::Integer(value),
                Cell::Text(value) => Value::Text(value),
                Cell::Blob(value) => Value::Blob(value),
            });
        }
        moved.push(values);
    }
    moved
}

/// Retained durable evidence only. There is deliberately no completed/committed
/// flag, migration execution method, or native admission capability here.
pub(super) struct DurableRecord {
    parent: File,
    parent_identity: (u64, u64),
    file: File,
    file_identity: (u64, u64),
    bytes: Vec<u8>,
    anchor: RecordAnchor,
    additions: ControlAdditions,
}

impl DurableRecord {
    #[cfg(test)]
    pub(super) fn original_record_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }

    /// Retained owned capacities plus both exact-sized verification read buffers.
    /// No record is read or authenticated here: this preflight must run first.
    pub(super) fn retained_and_verification_bytes(&self) -> anyhow::Result<u64> {
        let mut total = 0u64;
        let mut add = |bytes: usize| -> anyhow::Result<()> {
            total = total
                .checked_add(u64::try_from(bytes)?)
                .context("control record allocation overflow")?;
            ensure!(
                total <= 256 * 1024 * 1024,
                "control record allocation bound exceeded"
            );
            Ok(())
        };
        add(self.bytes.capacity())?;
        add(self
            .bytes
            .len()
            .checked_mul(2)
            .context("control record allocation overflow")?)?;
        for value in [
            &self.anchor.operation_id,
            &self.anchor.selected_pair_sha256,
            &self.anchor.original93_image_sha256,
            &self.anchor.original93_inventory_sha256,
            &self.anchor.frozen_binding_sha256,
        ] {
            add(value.capacity())?;
        }
        for rows in [&self.additions.ledger, &self.additions.guards] {
            add(rows
                .capacity()
                .checked_mul(std::mem::size_of::<Vec<Value>>())
                .context("control record allocation overflow")?)?;
            for row in rows {
                add(row
                    .capacity()
                    .checked_mul(std::mem::size_of::<Value>())
                    .context("control record allocation overflow")?)?;
                for cell in row {
                    match cell {
                        Value::Text(value) => add(value.capacity())?,
                        Value::Blob(value) => add(value.capacity())?,
                        _ => (),
                    }
                }
            }
        }
        Ok(total)
    }

    pub(super) fn verified_parent_identity(&self) -> anyhow::Result<(u64, u64)> {
        self.verify()?;
        Ok(self.parent_identity)
    }
    pub(super) fn additions(&self) -> &ControlAdditions {
        &self.additions
    }
    pub(super) fn anchor(&self) -> &RecordAnchor {
        &self.anchor
    }
    pub(super) fn sha256(&self) -> String {
        catabolic_core::sha256(&self.bytes)
    }
    pub(super) fn close_verification_bytes(&self) -> anyhow::Result<u64> {
        u64::try_from(self.bytes.len())?
            .checked_mul(2)
            .context("close record verification allocation overflow")
    }

    pub(super) fn verify(&self) -> anyhow::Result<()> {
        verify_parent(&self.parent, self.parent_identity)?;
        let held = regular(&self.file)?;
        ensure!(
            (held.dev(), held.ino()) == self.file_identity && held.len() == self.bytes.len() as u64,
            "held control record identity or size changed"
        );
        let named = read_at(&self.parent)?.context("named control record disappeared")?;
        let named_metadata = regular(&named)?;
        ensure!(
            (named_metadata.dev(), named_metadata.ino()) == self.file_identity
                && named_metadata.len() == self.bytes.len() as u64,
            "named control record identity or size changed"
        );
        ensure!(
            read_bounded(&self.file, Some(self.bytes.len()))? == self.bytes
                && read_bounded(&named, Some(self.bytes.len()))? == self.bytes,
            "control operation record bytes changed"
        );
        verify_parent(&self.parent, self.parent_identity)
    }
}

fn verify_parent(parent: &File, identity: (u64, u64)) -> anyhow::Result<()> {
    let metadata = parent.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.nlink() > 0 && (metadata.dev(), metadata.ino()) == identity,
        "control record parent identity changed"
    );
    Ok(())
}

fn read_at(parent: &File) -> anyhow::Result<Option<File>> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            NAME.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(error.into());
    }
    // SAFETY: successful openat transferred this new owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    regular(&file)?;
    Ok(Some(file))
}

fn regular(file: &File) -> anyhow::Result<std::fs::Metadata> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.len() > 0
            && metadata.len() <= MAX_BYTES as u64,
        "invalid control record storage or size"
    );
    Ok(metadata)
}

fn read_bounded(file: &File, expected_len: Option<usize>) -> anyhow::Result<Vec<u8>> {
    let before = regular(file)?;
    if let Some(expected) = expected_len {
        // Verification admitted only the retained record length. A changed
        // length must refuse before allocation, even after earlier metadata checks.
        ensure!(
            before.len() == u64::try_from(expected)?,
            "control record size changed before read"
        );
    }
    let mut bytes = vec![0; usize::try_from(before.len())?];
    file.read_exact_at(&mut bytes, 0)?;
    let after = regular(file)?;
    ensure!(
        (before.dev(), before.ino(), before.len()) == (after.dev(), after.ino(), after.len()),
        "control record changed while reading"
    );
    Ok(bytes)
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// Serialization mirrors Stored/Cell exactly but borrows all caller payloads.
// The sizing pass never constructs a Value/Cell tree or encoded byte buffer.
#[derive(Serialize)]
struct BorrowedStored<'a> {
    contract_version: u32,
    anchor: &'a RecordAnchor,
    ledger: BorrowedRows<'a>,
    guards: BorrowedRows<'a>,
}

struct BorrowedRows<'a>(&'a [Vec<Value>]);
struct BorrowedRow<'a>(&'a [Value]);
struct BorrowedCell<'a>(&'a Value);

impl Serialize for BorrowedRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut rows = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            rows.serialize_element(&BorrowedRow(row))?;
        }
        rows.end()
    }
}

impl Serialize for BorrowedRow<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut row = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            row.serialize_element(&BorrowedCell(value))?;
        }
        row.end()
    }
}

impl Serialize for BorrowedCell<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut cell = serializer.serialize_struct("Cell", 2)?;
        match self.0 {
            Value::Integer(value) => {
                cell.serialize_field("type", "integer")?;
                cell.serialize_field("value", value)?;
            }
            Value::Text(value) => {
                cell.serialize_field("type", "text")?;
                cell.serialize_field("value", value)?;
            }
            Value::Blob(value) => {
                cell.serialize_field("type", "blob")?;
                cell.serialize_field("value", value)?;
            }
            _ => {
                return Err(serde::ser::Error::custom(
                    "unsupported control record cell storage",
                ));
            }
        }
        cell.end()
    }
}

fn canonical_length(stored: &(impl Serialize + ?Sized)) -> anyhow::Result<usize> {
    struct EncodedBudget(usize);
    impl Write for EncodedBudget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size <= MAX_BYTES)
                .ok_or_else(|| {
                    io::Error::other("control operation encoded record exceeds bound")
                })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut budget = EncodedBudget(0);
    serde_json::to_writer(&mut budget, stored)?;
    Ok(budget.0)
}

fn canonical_bytes(stored: &Stored) -> anyhow::Result<Vec<u8>> {
    let length = canonical_length(stored)?;
    let mut bytes = Vec::with_capacity(length);
    serde_json::to_writer(&mut bytes, stored)?;
    ensure!(
        bytes.len() == length,
        "control record encoded length changed"
    );
    Ok(bytes)
}

fn validate_anchor(anchor: &RecordAnchor) -> anyhow::Result<()> {
    ensure!(
        !anchor.operation_id.is_empty()
            && anchor.operation_id.len() <= 256
            && !anchor.operation_id.chars().any(char::is_control)
            && anchor.stage_inode != 0
            && [
                &anchor.selected_pair_sha256,
                &anchor.original93_image_sha256,
                &anchor.original93_inventory_sha256,
                &anchor.frozen_binding_sha256,
            ]
            .into_iter()
            .all(|value| digest(value)),
        "invalid control operation record anchor"
    );
    Ok(())
}

fn timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 19
        || bytes.iter().enumerate().any(|(i, b)| match i {
            4 | 7 => *b != b'-',
            10 => *b != b' ',
            13 | 16 => *b != b':',
            _ => !b.is_ascii_digit(),
        })
    {
        return false;
    }
    let number = |a, b| value[a..b].parse::<u32>().unwrap_or(u32::MAX);
    let (year, month, day) = (number(0, 4), number(5, 7), number(8, 10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    year > 0
        && day > 0
        && day <= days
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}

fn validate_additions(
    additions: &ControlAdditions,
    reference: &ClosedReference,
    guards: &[(String, (String, String))],
) -> anyhow::Result<()> {
    let suffix = reference.suffix();
    ensure!(
        suffix.len() == 4
            && additions.ledger.len() == 4
            && additions.guards.len() == 3
            && guards.len() == 3,
        "control operation record requires exact four migrations and three guards"
    );
    for (offset, (row, migration)) in additions.ledger.iter().zip(suffix).enumerate() {
        ensure!(
            migration.version == 94 + offset as i64
                && !migration.migration_type.is_down_migration()
                && migration.checksum.len() == 48
                && row.len() == 7,
            "control record compiled suffix or ledger shape differs"
        );
        ensure!(
            matches!(&row[0], Value::Integer(id)
                if additions.ledger[..offset].iter().all(|previous| previous[0] != Value::Integer(*id)))
                && matches!(&row[1], Value::Integer(v) if *v == migration.version)
                && matches!(&row[2], Value::Text(v) if v == migration.description.as_ref())
                && matches!(&row[3], Value::Text(v) if timestamp(v))
                && matches!(&row[4], Value::Integer(1))
                && matches!(&row[5], Value::Blob(v) if v.as_slice() == migration.checksum.as_ref())
                && matches!(&row[6], Value::Integer(0)),
            "control record ledger authority fields differ"
        );
    }
    for (offset, ((row, (name, (table, sql))), expected_name)) in additions
        .guards
        .iter()
        .zip(guards)
        .zip([
            "motion_source_freeze_delete_preview_sheet_artifacts",
            "motion_source_freeze_insert_preview_sheet_artifacts",
            "motion_source_freeze_update_preview_sheet_artifacts",
        ])
        .enumerate()
    {
        ensure!(
            table == "preview_sheet_artifacts"
                && name == expected_name
                && !sql.is_empty()
                && sql.len() <= MAX_CELL_BYTES
                && row.len() == 4,
            "control record guard producer or shape differs"
        );
        ensure!(
            matches!(&row[0], Value::Integer(id)
                if additions.guards[..offset].iter().all(|previous| previous[0] != Value::Integer(*id)))
                && matches!(&row[1], Value::Text(v) if v == name)
                && matches!(&row[2], Value::Text(v) if v == table)
                && matches!(&row[3], Value::Text(v) if v == sql),
            "control record exact guard additions differ"
        );
    }
    Ok(())
}

fn encode_rows(rows: &[Vec<Value>]) -> anyhow::Result<Vec<Vec<Cell>>> {
    // Explicit capacities keep the pre-clone reservation exact; collecting a
    // fallible iterator can otherwise introduce unaccounted geometric growth.
    let mut encoded = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::with_capacity(row.len());
        for value in row {
            cells.push(match value {
                Value::Integer(v) => Cell::Integer(*v),
                Value::Text(v) if v.len() <= MAX_CELL_BYTES => Cell::Text(v.clone()),
                Value::Blob(v) if v.len() <= MAX_CELL_BYTES => Cell::Blob(v.clone()),
                _ => anyhow::bail!("unsupported control record cell storage or bound"),
            });
        }
        encoded.push(cells);
    }
    Ok(encoded)
}

fn decode_additions(stored: &Stored) -> anyhow::Result<ControlAdditions> {
    ensure!(
        stored.ledger.len() == 4
            && stored.guards.len() == 3
            && stored.ledger.iter().all(|row| row.len() == 7)
            && stored.guards.iter().all(|row| row.len() == 4),
        "control record row or cell counts differ"
    );
    let decode = |rows: &[Vec<Cell>], columns| -> anyhow::Result<Vec<Vec<Value>>> {
        rows.iter()
            .map(|row| {
                ensure!(row.len() == columns, "control record cell count differs");
                row.iter()
                    .map(|cell| match cell {
                        Cell::Integer(v) => Ok(Value::Integer(*v)),
                        Cell::Text(v) if v.len() <= MAX_CELL_BYTES => Ok(Value::Text(v.clone())),
                        Cell::Blob(v) if v.len() <= MAX_CELL_BYTES => Ok(Value::Blob(v.clone())),
                        _ => anyhow::bail!("control record cell exceeds bound"),
                    })
                    .collect()
            })
            .collect()
    };
    Ok(ControlAdditions {
        ledger: decode(&stored.ledger, 7)?,
        guards: decode(&stored.guards, 4)?,
    })
}

#[cfg(test)]
mod tests {
    // Keep the established legacy tests in the same module and reuse their
    // real directory/reference/guard fixture for the retained owner tests.
    include!("record/tests.rs");

    #[tokio::test]
    async fn retained_publication_fixed_deadline_refuses_create_and_finishes_started_durability() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        let fixture = Fixture::new().await;
        for cancelled in [false, true] {
            let mut publication = RetainedRecordPublication::new(fixture.prepared());
            let cancel = AtomicBool::new(cancelled);
            let deadline = if cancelled {
                Instant::now() + Duration::from_secs(60)
            } else {
                Instant::now() - Duration::from_secs(1)
            };
            assert!(
                publication
                    .publish(&fixture.directory, &cancel, deadline)
                    .is_err()
            );
            assert!(publication.original_record_fd().is_none());
            assert!(!fixture.path().exists());
            assert!(
                publication
                    .publish(
                        &fixture.directory,
                        &AtomicBool::new(false),
                        Instant::now() + Duration::from_secs(60)
                    )
                    .is_err()
            );
        }
        let mut publication = RetainedRecordPublication::new(fixture.prepared());
        let cancel = AtomicBool::new(false);
        let mut acquired = None;
        publication
            .publish_at_checkpoint(
                &fixture.directory,
                &cancel,
                Instant::now() + Duration::from_secs(60),
                |at, fd| {
                    if at == 0 {
                        acquired = Some(fd);
                        cancel.store(true, Ordering::Release);
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(publication.original_record_fd(), acquired);
        assert_eq!(publication.phase(), "durable evidence verified");
        publication.take_durable().unwrap().verify().unwrap();
    }

    #[tokio::test]
    async fn retained_publication_preclone_reservation_matches_borrowed_canonical_bytes() {
        let mut fixture = Fixture::new().await;
        fixture.anchor.operation_id = "retained-\"quoted\"-\\-é".into();
        let reservation = PreparedRecord::storage_reservation(
            &fixture.anchor,
            &fixture.additions,
            &fixture.reference,
            &fixture.guards,
        )
        .unwrap();
        assert!(!fixture.path().exists());
        let prepared = fixture.prepared();
        let borrowed = BorrowedStored {
            contract_version: 1,
            anchor: &fixture.anchor,
            ledger: BorrowedRows(&fixture.additions.ledger),
            guards: BorrowedRows(&fixture.additions.guards),
        };
        assert_eq!(serde_json::to_vec(&borrowed).unwrap(), prepared.bytes);
        assert_eq!(canonical_length(&borrowed).unwrap(), prepared.bytes.len());
        assert_eq!(prepared.bytes.capacity(), prepared.bytes.len());
        for rows in [&prepared.stored.ledger, &prepared.stored.guards] {
            assert_eq!(rows.capacity(), rows.len());
            for row in rows {
                assert_eq!(row.capacity(), row.len());
            }
        }
        let canonical_pointer = prepared.bytes.as_ptr();
        let anchor_pointer = prepared.stored.anchor.operation_id.as_ptr();
        let payload_pointer = match &prepared.stored.ledger[0][2] {
            Cell::Text(value) => value.as_ptr(),
            _ => panic!("fixture description is not text"),
        };
        let mut publication = RetainedRecordPublication::new(prepared);
        assert!(publication.take_durable().is_none());
        publication
            .publish(
                &fixture.directory,
                &std::sync::atomic::AtomicBool::new(false),
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            )
            .unwrap();
        let original_fd = publication.original_record_fd().unwrap();
        assert_eq!(publication.phase(), "durable evidence verified");
        let durable = publication.take_durable().unwrap();
        assert_eq!(durable.file.as_raw_fd(), original_fd);
        assert_eq!(durable.bytes.as_ptr(), canonical_pointer);
        assert_eq!(durable.anchor.operation_id.as_ptr(), anchor_pointer);
        match &durable.additions.ledger[0][2] {
            Value::Text(value) => assert_eq!(value.as_ptr(), payload_pointer),
            _ => panic!("retained description is not text"),
        }
        assert!(reservation >= durable.retained_and_verification_bytes().unwrap());
        for rows in [&durable.additions.ledger, &durable.additions.guards] {
            assert_eq!(rows.capacity(), rows.len());
            for row in rows {
                assert_eq!(row.capacity(), row.len());
            }
        }
        durable.verify().unwrap();
        assert_eq!(durable.additions.ledger, fixture.additions.ledger);
        assert_eq!(durable.additions.guards, fixture.additions.guards);
        assert!(publication.take_durable().is_none());
        assert!(
            publication
                .publish(
                    &fixture.directory,
                    &std::sync::atomic::AtomicBool::new(false),
                    std::time::Instant::now() + std::time::Duration::from_secs(60)
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn retained_publication_preclone_reservation_refuses_escaped_growth() {
        let mut fixture = Fixture::new().await;
        for (guard, row) in fixture.guards.iter_mut().zip(&mut fixture.additions.guards) {
            guard.1.1 = "\0".repeat(MAX_CELL_BYTES);
            row[3] = Value::Text(guard.1.1.clone());
        }
        let error = PreparedRecord::storage_reservation(
            &fixture.anchor,
            &fixture.additions,
            &fixture.reference,
            &fixture.guards,
        )
        .unwrap_err();
        assert!(error.to_string().contains("encoded record exceeds bound"));
        assert!(!fixture.path().exists());
    }

    #[tokio::test]
    async fn retained_publication_errors_keep_exact_original_fd_and_never_retry() {
        for point in 0..=5 {
            let fixture = Fixture::new().await;
            let prepared = fixture.prepared();
            let complete_len = prepared.bytes.len();
            let mut publication = RetainedRecordPublication::new(prepared);
            let mut observed = None;
            assert!(
                publication
                    .publish_at_checkpoint(
                        &fixture.directory,
                        &std::sync::atomic::AtomicBool::new(false),
                        std::time::Instant::now() + std::time::Duration::from_secs(60),
                        |at, fd| {
                            if at == point {
                                observed = Some(fd);
                                anyhow::bail!("injected retained publication failure");
                            }
                            Ok(())
                        }
                    )
                    .is_err()
            );
            let fd = observed.unwrap();
            assert_eq!(publication.original_record_fd(), Some(fd));
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
            if point >= 4 {
                assert!(publication.parent.is_some());
            }
            if point >= 5 {
                assert!(publication.verification_file.is_some());
            }
            let named = fs::read(fixture.path()).unwrap();
            assert_eq!(
                named.len(),
                match point {
                    0 => 0,
                    1 => complete_len / 2,
                    _ => complete_len,
                }
            );
            let held = publication.file.as_ref().unwrap().metadata().unwrap();
            let named_metadata = fs::metadata(fixture.path()).unwrap();
            assert_eq!(
                (held.dev(), held.ino()),
                (named_metadata.dev(), named_metadata.ino())
            );
            assert!(publication.take_durable().is_none());
            assert!(fixture.directory.require_absent().is_err());
            assert!(
                publication
                    .publish(
                        &fixture.directory,
                        &std::sync::atomic::AtomicBool::new(false),
                        std::time::Instant::now() + std::time::Duration::from_secs(60)
                    )
                    .is_err()
            );
            assert_eq!(publication.original_record_fd(), Some(fd));
            assert_eq!(fs::read(fixture.path()).unwrap(), named);
        }
    }

    #[tokio::test]
    async fn retained_publication_panics_keep_exact_original_fd_and_never_retry() {
        for point in 0..=5 {
            let fixture = Fixture::new().await;
            let mut publication = RetainedRecordPublication::new(fixture.prepared());
            let mut observed = None;
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                publication.publish_at_checkpoint(
                    &fixture.directory,
                    &std::sync::atomic::AtomicBool::new(false),
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                    |at, fd| {
                        if at == point {
                            observed = Some(fd);
                            panic!("injected retained publication panic");
                        }
                        Ok(())
                    },
                )
            }));
            assert!(unwind.is_err());
            let fd = observed.unwrap();
            assert_eq!(publication.original_record_fd(), Some(fd));
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
            if point >= 4 {
                assert!(publication.parent.is_some());
            }
            if point >= 5 {
                assert!(publication.verification_file.is_some());
            }
            let before = fs::read(fixture.path()).unwrap();
            assert!(publication.take_durable().is_none());
            assert!(
                publication
                    .publish(
                        &fixture.directory,
                        &std::sync::atomic::AtomicBool::new(false),
                        std::time::Instant::now() + std::time::Duration::from_secs(60)
                    )
                    .is_err()
            );
            assert_eq!(publication.original_record_fd(), Some(fd));
            assert_eq!(fs::read(fixture.path()).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn retained_publication_refuses_existing_or_substituted_evidence() {
        let fixture = Fixture::new().await;
        fs::write(fixture.path(), b"existing partial evidence").unwrap();
        let mut publication = RetainedRecordPublication::new(fixture.prepared());
        assert!(
            publication
                .publish(
                    &fixture.directory,
                    &std::sync::atomic::AtomicBool::new(false),
                    std::time::Instant::now() + std::time::Duration::from_secs(60)
                )
                .is_err()
        );
        assert!(publication.original_record_fd().is_none());
        assert!(publication.take_durable().is_none());
        assert_eq!(
            fs::read(fixture.path()).unwrap(),
            b"existing partial evidence"
        );

        let fixture = Fixture::new().await;
        let mut publication = RetainedRecordPublication::new(fixture.prepared());
        let mut observed = None;
        let original = fixture.root.path().join("held-original-record.json");
        assert!(
            publication
                .publish_at_checkpoint(
                    &fixture.directory,
                    &std::sync::atomic::AtomicBool::new(false),
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                    |at, fd| {
                        if at == 3 {
                            observed = Some(fd);
                            fs::rename(fixture.path(), &original)?;
                            fs::write(fixture.path(), b"substituted evidence")?;
                        }
                        Ok(())
                    }
                )
                .is_err()
        );
        assert_eq!(publication.original_record_fd(), observed);
        let held = publication.file.as_ref().unwrap().metadata().unwrap();
        let original_metadata = fs::metadata(&original).unwrap();
        assert_eq!(held.ino(), original_metadata.ino());
        assert_ne!(held.ino(), fs::metadata(fixture.path()).unwrap().ino());
        assert!(publication.take_durable().is_none());
        assert!(
            publication
                .publish(
                    &fixture.directory,
                    &std::sync::atomic::AtomicBool::new(false),
                    std::time::Instant::now() + std::time::Duration::from_secs(60)
                )
                .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), b"substituted evidence");
    }

    #[tokio::test]
    async fn retained_publication_directory_validation_borrows_before_move() {
        let fixture = Fixture::new().await;
        let parent = fixture.directory.parent.try_clone().unwrap();
        let original_fd = parent.as_raw_fd();
        let identity = RecordDirectory::validate_retained(&parent).unwrap();
        assert_eq!(parent.as_raw_fd(), original_fd);
        let directory = RecordDirectory::from_validated(parent, identity);
        assert_eq!(directory.parent.as_raw_fd(), original_fd);
        directory.verify_parent().unwrap();

        let not_directory = File::open(fixture.root.path().join("stage.sqlite3")).unwrap();
        let original_fd = not_directory.as_raw_fd();
        assert!(RecordDirectory::validate_retained(&not_directory).is_err());
        assert_eq!(not_directory.as_raw_fd(), original_fd);
        assert!(unsafe { libc::fcntl(original_fd, libc::F_GETFD) } >= 0);
    }
}
