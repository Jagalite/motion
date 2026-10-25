//! Bounded database-pair transport framing, not restore or database authority.
//! The reader verifies the complete framing and actual bytes. Database admission,
//! incarnation rotation and two-database reconciliation remain separate owners.
use anyhow::{Context, ensure};
use playscale_core::paired_backup::{Artifact, PairManifest};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

const MAGIC: &[u8; 16] = b"MOTION-PAIR-0001";
const MAX_MANIFEST: usize = 65536;
const MAX_IMAGE: u64 = 56 * 1024 * 1024;
const MAX_CONTAINER: u64 = 128 * 1024 * 1024;

fn check_cancel(cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "pair container cancelled");
    Ok(())
}
fn layout(manifest: &PairManifest, json_len: usize) -> anyhow::Result<u64> {
    ensure!(manifest.contract_version == 1, "unsupported pair contract");
    ensure!(json_len <= MAX_MANIFEST, "pair manifest exceeds bound");
    for artifact in [&manifest.motion_database, &manifest.native_database] {
        ensure!(
            artifact.bytes > 0 && artifact.bytes <= MAX_IMAGE,
            "pair image exceeds cohort"
        );
        ensure!(
            artifact.sha256.len() == 64
                && artifact
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid pair image digest"
        );
    }
    let size = 20u64
        .checked_add(u64::try_from(json_len)?)
        .and_then(|v| v.checked_add(manifest.motion_database.bytes))
        .and_then(|v| v.checked_add(manifest.native_database.bytes))
        .context("pair container length overflow")?;
    ensure!(size <= MAX_CONTAINER, "pair container exceeds cohort");
    Ok(size)
}
struct HashWriter<'a> {
    output: &'a mut dyn Write,
    digest: Sha256,
    bytes: u64,
    limit: u64,
    cancel: &'a AtomicBool,
}
impl Write for HashWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::other("pair container cancelled"));
        }
        if self
            .bytes
            .checked_add(data.len() as u64)
            .is_none_or(|n| n > self.limit)
        {
            return Err(std::io::Error::other("pair component exceeds exact length"));
        }
        let n = self.output.write(data)?;
        self.digest.update(&data[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
impl HashWriter<'_> {
    fn artifact(self) -> Artifact {
        Artifact {
            bytes: self.bytes,
            sha256: format!("{:x}", self.digest.finalize()),
        }
    }
}
fn component(
    output: &mut dyn Write,
    expected: &Artifact,
    cancel: &AtomicBool,
    copy: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut writer = HashWriter {
        output,
        digest: Sha256::new(),
        bytes: 0,
        limit: expected.bytes,
        cancel,
    };
    copy(&mut writer)?;
    ensure!(
        &writer.artifact() == expected,
        "pair component bytes differ from verified receipt"
    );
    check_cancel(cancel)
}
/// Call only with a manifest derived from genuine verified pair owners. The
/// resulting Artifact proves transport bytes, not an independently admitted DB.
pub(super) fn write(
    output: &mut dyn Write,
    manifest: &PairManifest,
    cancel: &AtomicBool,
    motion: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
    native: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<Artifact> {
    check_cancel(cancel)?;
    let json = serde_json::to_vec(manifest)?;
    let size = layout(manifest, json.len())?;
    let mut writer = HashWriter {
        output,
        digest: Sha256::new(),
        bytes: 0,
        limit: size,
        cancel,
    };
    writer.write_all(MAGIC)?;
    writer.write_all(&u32::try_from(json.len())?.to_le_bytes())?;
    writer.write_all(&json)?;
    component(&mut writer, &manifest.motion_database, cancel, motion)?;
    component(&mut writer, &manifest.native_database, cancel, native)?;
    let artifact = writer.artifact();
    ensure!(artifact.bytes == size, "pair framing length mismatch");
    check_cancel(cancel)?;
    Ok(artifact)
}
fn read_component(
    reader: &mut dyn Read,
    output: &mut dyn Write,
    expected: &Artifact,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut left = expected.bytes;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    while left > 0 {
        check_cancel(cancel)?;
        let size = usize::try_from(left.min(buffer.len() as u64))?;
        reader.read_exact(&mut buffer[..size])?;
        digest.update(&buffer[..size]);
        output.write_all(&buffer[..size])?;
        left -= size as u64;
    }
    ensure!(
        format!("{:x}", digest.finalize()) == expected.sha256,
        "pair component digest mismatch"
    );
    Ok(())
}
/// Bounded complete transport verification. The returned manifest is untrusted
/// descriptive data until copied databases pass their actual admission owners.
pub(super) fn read(reader: &mut dyn Read, cancel: &AtomicBool) -> anyhow::Result<PairManifest> {
    extract(reader, &mut std::io::sink(), &mut std::io::sink(), cancel)
}
/// Caller owns private stages and discards them on any verification failure.
pub(super) fn extract(
    reader: &mut dyn Read,
    motion: &mut dyn Write,
    native: &mut dyn Write,
    cancel: &AtomicBool,
) -> anyhow::Result<PairManifest> {
    check_cancel(cancel)?;
    let mut magic = [0u8; 16];
    reader.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "unsupported pair container");
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    ensure!(length <= MAX_MANIFEST, "pair manifest exceeds bound");
    let mut json = vec![0u8; length];
    reader.read_exact(&mut json)?;
    let manifest: PairManifest = serde_json::from_slice(&json)?;
    layout(&manifest, length)?;
    // Reject ambiguous/noncanonical encodings rather than accepting a second
    // spelling for the manifest used to bind publication and later recovery.
    ensure!(
        serde_json::to_vec(&manifest)? == json,
        "noncanonical pair manifest"
    );
    read_component(reader, motion, &manifest.motion_database, cancel)?;
    read_component(reader, native, &manifest.native_database, cancel)?;
    let mut extra = [0u8; 1];
    ensure!(
        reader.read(&mut extra)? == 0,
        "trailing pair container bytes"
    );
    check_cancel(cancel)?;
    Ok(manifest)
}

/// Publication revalidation against the actual retained pair's commitments,
/// including identity, schema, complete inventories, ledgers and binding proof.
/// The expected value must come from PreparedPair, never this reader's output.
pub(super) fn read_expected(
    reader: &mut dyn Read,
    expected: &PairManifest,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    ensure!(
        &read(reader, cancel)? == expected,
        "pair identity/binding/inventory commitments differ"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> PairManifest {
        let hash = "a".repeat(64);
        PairManifest {
            contract_version: 1,
            pair_id: "transport-only-fixture".into(),
            native_identity: playscale_core::paired_backup::NativeIdentity {
                catalog_id: "catalog".into(),
                incarnation: "incarnation".into(),
                generation: 7,
                schema_version: 40,
                profile: "motion".into(),
            },
            motion_schema_version: 92,
            motion_database: Artifact {
                bytes: 6,
                sha256: format!("{:x}", Sha256::digest(b"motion")),
            },
            native_database: Artifact {
                bytes: 6,
                sha256: format!("{:x}", Sha256::digest(b"native")),
            },
            motion_migration_ledger_sha256: hash.clone(),
            native_migration_ledger_sha256: hash.clone(),
            motion_control_journals_sha256: hash.clone(),
            native_control_journals_sha256: hash.clone(),
            motion_binding_sha256: hash,
        }
    }
    fn encoded() -> Vec<u8> {
        let mut bytes = Vec::new();
        let result = write(
            &mut bytes,
            &manifest(),
            &AtomicBool::new(false),
            |out| Ok(out.write_all(b"motion")?),
            |out| Ok(out.write_all(b"native")?),
        )
        .unwrap();
        assert_eq!(result.bytes, bytes.len() as u64);
        assert_eq!(result.sha256, format!("{:x}", Sha256::digest(&bytes)));
        bytes
    }
    #[test]
    fn complete_transport_roundtrip_rejects_corruption_truncation_and_trailing_bytes() {
        let bytes = encoded();
        assert_eq!(&bytes[..16], b"MOTION-PAIR-0001");
        let json_len = serde_json::to_vec(&manifest()).unwrap().len();
        assert_eq!(
            u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize,
            json_len
        );
        assert_eq!(bytes.len(), 20 + json_len + b"motionnative".len());
        assert_eq!(
            read(&mut bytes.as_slice(), &AtomicBool::new(false)).unwrap(),
            manifest()
        );
        for length in [0, 15, 19, bytes.len() - 1] {
            assert!(read(&mut &bytes[..length], &AtomicBool::new(false)).is_err());
        }
        let mut mismatched = manifest();
        mismatched.motion_binding_sha256 = "b".repeat(64);
        assert!(
            read_expected(&mut bytes.as_slice(), &mismatched, &AtomicBool::new(false)).is_err()
        );
        mismatched = manifest();
        mismatched.native_identity.incarnation = "different".into();
        assert!(
            read_expected(&mut bytes.as_slice(), &mismatched, &AtomicBool::new(false)).is_err()
        );
        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(read(&mut corrupt.as_slice(), &AtomicBool::new(false)).is_err());
        let mut extra = bytes;
        extra.push(0);
        assert!(read(&mut extra.as_slice(), &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn limits_cancellation_and_wrong_source_bytes_refuse_before_complete_container() {
        let mut oversized = manifest();
        oversized.native_database.bytes = MAX_IMAGE + 1;
        let mut output = Vec::new();
        assert!(
            write(
                &mut output,
                &oversized,
                &AtomicBool::new(false),
                |_| panic!("must refuse before copying"),
                |_| panic!("must refuse before copying")
            )
            .is_err()
        );
        assert!(output.is_empty());
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read(&mut header.as_slice(), &AtomicBool::new(false)).is_err());
        assert!(
            write(
                &mut output,
                &manifest(),
                &AtomicBool::new(true),
                |_| panic!("cancelled"),
                |_| panic!("cancelled")
            )
            .is_err()
        );
        assert!(output.is_empty());
        assert!(
            write(
                &mut output,
                &manifest(),
                &AtomicBool::new(false),
                |out| Ok(out.write_all(b"tamper")?),
                |_| panic!("first digest must fail")
            )
            .is_err()
        );
        let cancel = AtomicBool::new(false);
        assert!(
            write(
                &mut Vec::new(),
                &manifest(),
                &cancel,
                |out| {
                    out.write_all(b"motion")?;
                    cancel.store(true, Ordering::Release);
                    Ok(())
                },
                |_| panic!("cancelled between images")
            )
            .is_err()
        );
    }
    #[test]
    fn duplicate_and_noncanonical_manifest_encodings_are_not_admitted() {
        for json in [
            format!(
                "{{\"contract_version\":1,{}",
                &serde_json::to_string(&manifest()).unwrap()[1..]
            ),
            serde_json::to_string_pretty(&manifest()).unwrap(),
        ] {
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
            bytes.extend_from_slice(json.as_bytes());
            bytes.extend_from_slice(b"motionnative");
            assert!(read(&mut bytes.as_slice(), &AtomicBool::new(false)).is_err());
        }
    }
}
