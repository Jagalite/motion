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
            viewing_epoch: Some("server:restore".into()),
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
    // An unreferenced blob may still belong to a ready demand that has not
    // recovered yet: publication and removal leave it; only `collect`,
    // run after recovery, deletes it.
    let pending = b"recoverable later";
    std::fs::write(
        root.join("blobs").join(entry("d2", pending).sha256),
        pending,
    )
    .unwrap();
    assert!(!store.remove("never-published").unwrap());
    stage(&root, "d3", b"another publication");
    store.publish(entry("d3", b"another publication")).unwrap();
    assert!(store.recover(entry("d2", pending)).unwrap());
    std::fs::write(root.join("blobs").join("0".repeat(64)), b"orphan").unwrap();
    store.collect().unwrap();
    let mut expected = vec![
        entry("d1", media).sha256,
        entry("d2", pending).sha256,
        entry("d3", b"another publication").sha256,
    ];
    expected.sort();
    assert_eq!(blobs(&root), expected);
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
    assert_eq!(store.staged().unwrap(), vec!["b".to_string()]);
    // A cancelled demand's partial transfer is released by removal too.
    assert!(!store.remove("b").unwrap());
    assert!(store.staged().unwrap().is_empty());
    assert_eq!(store.usage().unwrap(), 10);
    stage(&root, "b", b"01234");
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
    assert!(
        store
            .rebase("d1", "7", "1", "server:restore2", Some(42_000))
            .unwrap()
    );
    assert!(
        !store
            .rebase("d1", "7", "1", "server:restore2", Some(42_000))
            .unwrap()
    );
    drop(store);

    // The host continues the device sequence after the compacted base and
    // resumes from the rebased server position.
    let cache = Cache::open(&root).unwrap();
    drop(cache);
    let store = Store::open(&root, &scope()).unwrap();
    let rebased = &store.manifest().downloads[0];
    assert_eq!(rebased.identity.base_viewing_revision, "7");
    assert_eq!(
        rebased.identity.viewing_epoch.as_deref(),
        Some("server:restore2")
    );
    assert_eq!(rebased.resume_position_ms, Some(42_000));
    assert_eq!(store.events().unwrap().next_sequence(), Some(4));
}

#[test]
fn a_conflicting_event_is_never_compacted_by_the_watermark() {
    use playscale_core::offline::Acknowledgement;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let media = b"offline title";
    let mut store = Store::open(&root, &scope()).unwrap();
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    let identity = entry("d1", media).identity;
    std::fs::write(
        root.join("events.json"),
        serde_json::to_vec(&EventLog {
            base_sequence: 0,
            events: vec![event(&identity, 1), event(&identity, 2)],
        })
        .unwrap(),
    )
    .unwrap();
    // The server holds other content at 1 (event_conflict) and reports 2 as next.
    let refused = [
        Acknowledgement {
            event_id: "e1".into(),
            consumed: false,
        },
        Acknowledgement {
            event_id: "e2".into(),
            consumed: false,
        },
    ];
    let log = store.acknowledge_results(&refused, 2).unwrap();
    assert_eq!((log.base_sequence, log.events.len()), (0, 2));
    let partly = [
        Acknowledgement {
            event_id: "e1".into(),
            consumed: true,
        },
        Acknowledgement {
            event_id: "e2".into(),
            consumed: false,
        },
    ];
    let log = store.acknowledge_results(&partly, 3).unwrap();
    assert_eq!((log.base_sequence, log.events.len()), (1, 1));
}

#[test]
fn a_crashed_publication_is_recovered_without_another_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let media = b"verified before the crash";
    let mut store = Store::open(&root, &scope()).unwrap();
    // Nothing to recover: no blob.
    assert!(!store.recover(entry("d1", media)).unwrap());
    // The blob was renamed into place, then the process died.
    std::fs::write(root.join("blobs").join(entry("d1", media).sha256), media).unwrap();
    assert!(store.recover(entry("d1", media)).unwrap());
    assert_eq!(store.manifest().downloads, vec![entry("d1", media)]);
    // A corrupted leftover is discarded, not published, so the download can
    // be transferred afresh.
    let other = b"another title";
    let leftover = root.join("blobs").join(entry("d2", other).sha256);
    std::fs::write(&leftover, b"corrupt").unwrap();
    assert!(!store.recover(entry("d2", other)).unwrap());
    assert!(!leftover.exists());
    assert_eq!(store.manifest().downloads.len(), 1);
    stage(&root, "d2", other);
    assert_eq!(
        store.publish(entry("d2", other)).unwrap(),
        Publication::Published
    );
}

#[test]
fn compaction_keeps_the_resume_checkpoint_without_moving_the_base() {
    use playscale_core::offline::Acknowledgement;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let media = b"offline title";
    let mut store = Store::open(&root, &scope()).unwrap();
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    let identity = entry("d1", media).identity;
    std::fs::write(
        root.join("events.json"),
        serde_json::to_vec(&EventLog {
            base_sequence: 0,
            events: vec![event(&identity, 1), event(&identity, 2)],
        })
        .unwrap(),
    )
    .unwrap();
    let both = [
        Acknowledgement {
            event_id: "e1".into(),
            consumed: true,
        },
        Acknowledgement {
            event_id: "e2".into(),
            consumed: true,
        },
    ];
    let log = store.acknowledge_results(&both, 3).unwrap();
    assert!(log.events.is_empty());
    let saved = &store.manifest().downloads[0];
    assert_eq!(saved.resume_position_ms, Some(2_000));
    assert_eq!(
        saved.identity, identity,
        "the causal base moves only by rebase"
    );
    drop(store);
    let reopened = Store::open(&root, &scope()).unwrap();
    assert_eq!(
        reopened.manifest().downloads[0].resume_position_ms,
        Some(2_000)
    );
}

#[test]
fn blobs_of_ended_demands_left_by_a_crash_are_collected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let store = Store::open(&root, &scope()).unwrap();
    // Verified and renamed into place, then the process died, then the demand ended.
    let orphan = root.join("blobs").join(entry("gone", b"orphan").sha256);
    std::fs::write(&orphan, b"orphan").unwrap();
    store.collect().unwrap();
    assert!(!orphan.exists());
}
