//! Real filesystem behavior of the device cache writer: verified atomic
//! publication, idempotent completion, crash leftovers, removal, quota,
//! acknowledged-event compaction and rebasing, and exclusion from the
//! presentation host.
use motion_ui::offline::CachedDownload;
use motion_ui_host::{
    Cache, EventLog,
    store::{Publication, Store, staging_name},
};
use playscale_core::offline::{CacheScope, DownloadIdentity, OfflineEvent};
use sha2::{Digest, Sha256};

fn scope() -> CacheScope {
    CacheScope {
        server_id: "server".into(),
        principal_id: "device".into(),
        profile_id: "default".into(),
        device_id: "device".into(),
    }
}

fn entry(id: &str, bytes: &[u8]) -> CachedDownload {
    CachedDownload {
        identity: DownloadIdentity {
            download_id: id.into(),
            timeline_id: "timeline".into(),
            timeline_revision: "1".into(),
            source_revision: format!("{:x}", Sha256::digest(bytes)),
            base_viewing_revision: "0".into(),
            base_manual_epoch: "0".into(),
        },
        title: "Saved".into(),
        duration_ms: 100_000,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
        content_type: "video/mp4".into(),
        resume_position_ms: None,
    }
}

fn stage(root: &std::path::Path, id: &str, bytes: &[u8]) {
    std::fs::write(root.join("staging").join(staging_name(id).unwrap()), bytes).unwrap();
}

fn blobs(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("blobs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn publication_is_verified_atomic_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"verified media bytes";

    // A corrupted or truncated transfer is discarded and publishes nothing.
    stage(&root, "d1", b"verified media byteZ");
    assert!(store.publish(entry("d1", media)).is_err());
    stage(&root, "d1", b"verified");
    assert!(store.publish(entry("d1", media)).is_err());
    assert!(store.manifest().downloads.is_empty());
    assert!(blobs(&root).is_empty());
    assert!(!root.join("staging/d1.part").exists());

    stage(&root, "d1", media);
    assert_eq!(
        store.publish(entry("d1", media)).unwrap(),
        Publication::Published
    );
    assert_eq!(blobs(&root), vec![entry("d1", media).sha256]);
    assert!(!root.join("staging/d1.part").exists());
    // Duplicate completion (a retried agent) is a no-op.
    stage(&root, "d1", media);
    assert_eq!(
        store.publish(entry("d1", media)).unwrap(),
        Publication::AlreadyPublished
    );
    assert!(!root.join("staging/d1.part").exists());
    drop(store);

    // The presentation host reads exactly what was published.
    let cache = Cache::open(&root).unwrap();
    drop(cache);
    let store = Store::open(&root, &scope()).unwrap();
    assert_eq!(store.manifest().downloads, vec![entry("d1", media)]);
}

#[test]
fn crash_leftovers_are_recovered_or_collected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let media = b"blob renamed before the manifest";
    {
        let _store = Store::open(&root, &scope()).unwrap();
    }
    // Crash after the blob rename, before the manifest: completion verifies
    // the blob in place.
    std::fs::write(root.join("blobs").join(entry("d1", media).sha256), media).unwrap();
    let mut store = Store::open(&root, &scope()).unwrap();
    assert_eq!(
        store.publish(entry("d1", media)).unwrap(),
        Publication::Published
    );
    // An orphan blob from an abandoned transfer is collected on the next write.
    std::fs::write(root.join("blobs").join("0".repeat(64)), b"orphan").unwrap();
    assert!(!store.remove("never-published").unwrap());
    assert_eq!(blobs(&root), vec![entry("d1", media).sha256]);
}

#[test]
fn removal_keeps_shared_blobs_and_failed_writes_change_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"shared content";
    for id in ["a", "b"] {
        stage(&root, id, media);
        store.publish(entry(id, media)).unwrap();
    }
    assert!(store.remove("a").unwrap());
    assert_eq!(blobs(&root).len(), 1, "still referenced by b");
    assert!(store.remove("b").unwrap());
    assert!(blobs(&root).is_empty());
    assert!(store.manifest().downloads.is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let other = b"cannot be published";
        stage(&root, "c", other);
        std::fs::set_permissions(root.join("blobs"), std::fs::Permissions::from_mode(0o500))
            .unwrap();
        let failed = store.publish(entry("c", other));
        std::fs::set_permissions(root.join("blobs"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(failed.is_err());
        assert!(store.manifest().downloads.is_empty());
        drop(store);
        let reopened = Store::open(&root, &scope()).unwrap();
        assert!(reopened.manifest().downloads.is_empty());
    }
}

#[test]
fn quota_counts_published_and_staged_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    stage(&root, "a", b"0123456789");
    store.publish(entry("a", b"0123456789")).unwrap();
    stage(&root, "b", b"01234");
    assert_eq!(store.usage().unwrap(), 15);
    assert!(store.admits_transfer(5, 20, 0).unwrap());
    assert!(!store.admits_transfer(6, 20, 0).unwrap());
    assert!(!store.admits_transfer(1, u64::MAX, u64::MAX).unwrap());
}

#[test]
fn roots_are_scoped_and_exclusive_with_the_presentation_host() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let store = Store::open(&root, &scope()).unwrap();
    assert!(
        Cache::open(&root).is_err(),
        "the host cannot open a cache being written"
    );
    drop(store);
    let mut other = scope();
    other.profile_id = "kids".into();
    assert!(Store::open(&root, &other).is_err());
    let host = Cache::open(&root).unwrap();
    assert!(
        Store::open(&root, &scope()).is_err(),
        "no writes beside a running host"
    );
    drop(host);
}

fn event(media: &DownloadIdentity, sequence: u64) -> OfflineEvent {
    OfflineEvent {
        scope: scope(),
        media: media.clone(),
        event_id: format!("e{sequence}"),
        device_sequence: sequence.to_string(),
        position_ms: sequence * 1_000,
        status: "paused".into(),
        device_time: None,
    }
}

#[test]
fn acknowledged_events_compact_without_renumbering_and_rebase_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let media = b"offline title";
    let mut store = Store::open(&root, &scope()).unwrap();
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    let identity = entry("d1", media).identity;
    // A log written before compaction existed is a bare array.
    std::fs::write(
        root.join("events.json"),
        serde_json::to_vec(&vec![
            event(&identity, 1),
            event(&identity, 2),
            event(&identity, 3),
        ])
        .unwrap(),
    )
    .unwrap();
    assert_eq!(store.events().unwrap().next_sequence(), Some(4));
    assert!(
        store.acknowledge(4).is_err(),
        "never erase for a foreign acknowledgement"
    );
    assert_eq!(store.events().unwrap().events.len(), 3);
    let log = store.acknowledge(2).unwrap();
    assert_eq!(
        (log.base_sequence, log.events.len(), log.next_sequence()),
        (2, 1, Some(4))
    );
    let log = store.acknowledge(3).unwrap();
    assert_eq!(
        log,
        EventLog {
            base_sequence: 3,
            events: vec![]
        }
    );
    assert!(store.rebase("d1", "7", "1", Some(42_000)).unwrap());
    assert!(!store.rebase("d1", "7", "1", Some(42_000)).unwrap());
    drop(store);

    // The host continues the device sequence after the compacted base and
    // resumes from the rebased server position.
    let cache = Cache::open(&root).unwrap();
    drop(cache);
    let store = Store::open(&root, &scope()).unwrap();
    let rebased = &store.manifest().downloads[0];
    assert_eq!(rebased.identity.base_viewing_revision, "7");
    assert_eq!(rebased.resume_position_ms, Some(42_000));
    assert_eq!(store.events().unwrap().next_sequence(), Some(4));
}
