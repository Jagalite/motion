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
        artwork: None,
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
            events: vec![],
            // The compaction point remembers the history it belonged to.
            base_viewing_epoch: Some("server:restore".into()),
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
            base_viewing_epoch: None,
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
            base_viewing_epoch: None,
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

#[test]
fn recoverable_leftovers_count_against_the_quota() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let store = Store::open(&root, &scope()).unwrap();
    // A verified blob a crash left before its manifest entry was written.
    std::fs::write(
        root.join("blobs").join(entry("d1", b"0123456789").sha256),
        b"0123456789",
    )
    .unwrap();
    assert_eq!(store.usage().unwrap(), 10);
    assert!(!store.admits_transfer(1, 10, 0).unwrap());
}

/// A title the person removes stays removed: the intent is durable, the
/// copy and its blob go, and neither publication nor recovery saves it
/// again until the server's answer settles the removal.
#[test]
fn a_removed_title_is_never_saved_again_until_the_server_confirms() {
    use playscale_core::offline::Deletion;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"removed media";
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    assert!(
        !store.remove_by_person("unknown").unwrap(),
        "only a saved title"
    );
    assert!(store.remove_by_person("d1").unwrap());
    assert!(store.manifest().downloads.is_empty());
    assert!(blobs(&root).is_empty());
    assert_eq!(store.removals(), ["d1"]);
    // Neither a new transfer nor a crash leftover is saved again.
    stage(&root, "d1", media);
    assert!(store.publish(entry("d1", media)).is_err());
    assert!(store.recover(entry("d1", media)).is_err());
    drop(store);
    // Durable: a reopened writer still refuses it.
    let mut store = Store::open(&root, &scope()).unwrap();
    assert_eq!(store.removals(), ["d1"]);
    assert!(!store.settle_removal("d1", Deletion::Failed).unwrap());
    assert_eq!(store.removals(), ["d1"]);
    assert!(store.settle_removal("d1", Deletion::Absent).unwrap());
    assert!(store.removals().is_empty());
    // Settled: a new demand may save it again.
    stage(&root, "d1", media);
    assert_eq!(
        store.publish(entry("d1", media)).unwrap(),
        Publication::Published
    );
    // A staged (unpublished) transfer can be removed too.
    stage(&root, "d2", b"partial");
    assert!(store.remove_by_person("d2").unwrap());
    assert!(store.staged().unwrap().is_empty());
    assert_eq!(store.removals(), ["d2"]);
}

/// The running host removes titles as the writer under the lock it holds;
/// another handle that merely names a file is refused.
#[test]
fn only_the_held_lock_of_this_root_admits_a_writer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"held media";
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    drop(store);
    let host = Cache::open(&root).unwrap();
    let elsewhere = dir.path().join("other.lock");
    std::fs::write(&elsewhere, b"").unwrap();
    assert!(
        Store::open_held(&root, &scope(), std::fs::File::open(&elsewhere).unwrap()).is_err(),
        "a handle on another file is not this root's lock"
    );
    assert!(host.remove("d1").unwrap());
    assert!(!host.remove("d1").unwrap());
    drop(host);
    let store = Store::open(&root, &scope()).unwrap();
    assert!(store.manifest().downloads.is_empty());
    assert_eq!(store.removals(), ["d1"]);
}

/// A crash after the removal intent was written but before the copy went:
/// the next writer or host completes the removal before presenting titles.
#[test]
fn an_interrupted_removal_is_completed_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"interrupted media";
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    drop(store);
    // The intent is durable; the copy is still there.
    std::fs::write(root.join("removals.json"), br#"["d1"]"#).unwrap();
    let host = Cache::open(&root).unwrap();
    assert!(!host.presents("d1"), "the host never presents it");
    drop(host);
    let store = Store::open(&root, &scope()).unwrap();
    assert!(store.manifest().downloads.is_empty());
    assert!(blobs(&root).is_empty());
    assert_eq!(store.removals(), ["d1"]);
}

/// A removal whose local cleanup failed keeps its intent even after the
/// server confirms the deletion: the intent is all that suppresses the copy.
#[cfg(unix)]
#[test]
fn a_removal_is_settled_only_once_its_copy_is_gone() {
    use playscale_core::offline::Deletion;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"stuck media";
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    // A partial re-transfer that cannot be deleted (read-only staging).
    stage(&root, "d1", b"partial");
    let staging = root.join("staging");
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(store.remove_by_person("d1").is_err(), "the cleanup failed");
    assert_eq!(store.removals(), ["d1"]);
    assert_eq!(
        store.manifest().downloads.len(),
        1,
        "the copy is still listed"
    );
    // The server deleted it, but the copy is still here: not settled.
    assert!(!store.settle_removal("d1", Deletion::Deleted).unwrap());
    drop(store);
    let store = Store::open(&root, &scope()).unwrap();
    assert_eq!(store.removals(), ["d1"], "a reopen keeps the intent too");
    drop(store);
    let host = Cache::open(&root).unwrap();
    assert!(!host.presents("d1"));
    drop(host);
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = Store::open(&root, &scope()).unwrap();
    assert!(store.settle_removal("d1", Deletion::Deleted).unwrap());
    assert!(store.manifest().downloads.is_empty() && store.removals().is_empty());
}

/// After a server restore the first batch under the new history carries a
/// re-anchor from the history the log's compaction point belongs to.
#[test]
fn a_compaction_point_of_an_earlier_history_is_reanchored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"restored media";
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    let identity = entry("d1", media).identity;
    let write = |log: &EventLog| {
        std::fs::write(root.join("events.json"), serde_json::to_vec(log).unwrap()).unwrap();
    };
    // Compacted through 2 in "server:before"; event 3 is under the new history.
    write(&EventLog {
        base_sequence: 2,
        events: vec![event(&identity, 3)],
        base_viewing_epoch: Some("server:before".into()),
    });
    assert_eq!(
        store.pending_reanchor().unwrap(),
        Some(playscale_core::offline::Reanchor {
            from_viewing_epoch: "server:before".into(),
            through: 2,
        })
    );
    // Once compacted in the current history, nothing more is needed.
    store.acknowledge(3).unwrap();
    assert_eq!(
        store.events().unwrap().base_viewing_epoch.as_deref(),
        Some("server:restore")
    );
    write(&EventLog {
        base_sequence: 3,
        events: vec![event(&identity, 4)],
        base_viewing_epoch: Some("server:restore".into()),
    });
    assert_eq!(store.pending_reanchor().unwrap(), None);
}

/// Progress recorded before a server restore is re-applied onto the
/// downloads' current pins or discarded, by the person's choice, in one
/// durable write; the next batch then re-anchors past the old events.
#[test]
fn progress_from_before_a_restore_is_reapplied_or_discarded_by_choice() {
    use motion_ui_host::store::{RestoreChoice, RestoreOutcome};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"choice media";
    stage(&root, "d1", media);
    store.publish(entry("d1", media)).unwrap();
    // The manifest pin is already the restored history ("server:restore").
    let mut before = entry("d1", media).identity;
    before.viewing_epoch = Some("server:before".into());
    before.base_viewing_revision = "4".into();
    let log = EventLog {
        base_sequence: 2,
        events: vec![event(&before, 3), event(&before, 4)],
        base_viewing_epoch: Some("server:before".into()),
    };
    let write = |log: &EventLog| {
        std::fs::write(root.join("events.json"), serde_json::to_vec(log).unwrap()).unwrap();
    };
    write(&log);
    assert!(
        store
            .resolve_restore(RestoreChoice::Discard, "bad", &[])
            .is_err()
    );
    // The restored pin: this download's base as restored, under the current
    // history. A pin for another history or download is refused.
    let mut restored = entry("d1", media).identity;
    restored.base_viewing_revision = "7".into();
    let mut foreign = restored.clone();
    foreign.viewing_epoch = Some("server:other".into());
    assert!(
        store
            .resolve_restore(RestoreChoice::Reapply, "server:restore", &[foreign])
            .is_err()
    );
    let mut unknown = restored.clone();
    unknown.download_id = "d9".into();
    assert!(
        store
            .resolve_restore(RestoreChoice::Reapply, "server:restore", &[unknown])
            .is_err()
    );
    let kept = store
        .resolve_restore(
            RestoreChoice::Reapply,
            "server:restore",
            std::slice::from_ref(&restored),
        )
        .unwrap();
    assert_eq!(
        kept,
        RestoreOutcome {
            reapplied: 2,
            ..Default::default()
        }
    );
    let after = store.events().unwrap();
    assert_eq!(after.base_sequence, 4);
    let pin = restored.clone();
    assert_eq!(
        after
            .events
            .iter()
            .map(|e| (e.device_sequence.as_str(), e.position_ms, &e.media))
            .collect::<Vec<_>>(),
        [("5", 3_000, &pin), ("6", 4_000, &pin)]
    );
    assert_eq!(
        store.pending_reanchor().unwrap().map(|r| r.through),
        Some(4)
    );
    // Nothing earlier is left: a second choice changes nothing.
    assert_eq!(
        store
            .resolve_restore(RestoreChoice::Discard, "server:restore", &[])
            .unwrap(),
        RestoreOutcome::default()
    );
    // Discarding instead drops them; the host still opens the log.
    write(&log);
    let dropped = store
        .resolve_restore(RestoreChoice::Discard, "server:restore", &[])
        .unwrap();
    assert_eq!(dropped.discarded, 2);
    let after = store.events().unwrap();
    assert_eq!((after.base_sequence, after.events.len()), (4, 0));
    drop(store);
    Cache::open(&root).unwrap();
}

/// A poster is verified into its own blob before the entry that names it;
/// a mismatch publishes nothing; removal and collection treat it as the
/// entry's bytes.
#[test]
fn a_poster_is_published_and_released_with_its_download() {
    use motion_ui::offline::CachedArtwork;
    use motion_ui_host::store::artwork_staging_name;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    let media = b"titled media";
    let poster = b"poster bytes";
    let with_poster = |poster: &[u8]| CachedDownload {
        artwork: Some(CachedArtwork {
            sha256: format!("{:x}", Sha256::digest(poster)),
            size: poster.len() as u64,
            content_type: "image/png".into(),
        }),
        ..entry("d1", media)
    };
    let stage_poster = |bytes: &[u8]| {
        std::fs::write(
            root.join("staging")
                .join(artwork_staging_name("d1").unwrap()),
            bytes,
        )
        .unwrap()
    };
    // A poster whose bytes do not match: nothing is published.
    stage(&root, "d1", media);
    stage_poster(b"other bytes!");
    assert!(store.publish(with_poster(poster)).is_err());
    assert!(store.manifest().downloads.is_empty());
    stage_poster(poster);
    assert_eq!(
        store.publish(with_poster(poster)).unwrap(),
        Publication::Published
    );
    let poster_blob = format!("{:x}", Sha256::digest(poster));
    assert!(blobs(&root).contains(&poster_blob));
    // Collection keeps a referenced poster; removal releases it.
    store.collect().unwrap();
    assert!(blobs(&root).contains(&poster_blob));
    assert!(store.remove("d1").unwrap());
    assert!(blobs(&root).is_empty());
}

/// A staged poster alone still belongs to its download's lifecycle.
#[test]
fn a_staged_poster_alone_is_a_staged_download() {
    use motion_ui_host::store::artwork_staging_name;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut store = Store::open(&root, &scope()).unwrap();
    std::fs::write(
        root.join("staging")
            .join(artwork_staging_name("d1").unwrap()),
        b"poster",
    )
    .unwrap();
    stage(&root, "d2", b"partial");
    std::fs::write(
        root.join("staging")
            .join(artwork_staging_name("d2").unwrap()),
        b"poster",
    )
    .unwrap();
    assert_eq!(store.staged().unwrap(), ["d1", "d2"]);
    assert!(!store.remove("d1").unwrap(), "nothing published");
    assert_eq!(store.staged().unwrap(), ["d2"]);
}
