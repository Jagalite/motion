use playscale_core::{downloads::*, maintenance::*, offline::*};
fn identity() -> DownloadIdentity {
    DownloadIdentity {
        download_id: "d".into(),
        timeline_id: "t".into(),
        timeline_revision: "1".into(),
        source_revision: "r".into(),
        base_viewing_revision: "0".into(),
        base_manual_epoch: "0".into(),
        viewing_epoch: Some("s:new".into()),
    }
}
#[test]
fn restored_pins_require_exact_held_identity_and_valid_causality() {
    let held = identity();
    assert!(restored_pin_admitted("s:new", &held, Some(&held)));
    assert!(!restored_pin_admitted("s:new", &held, None));
    for n in 0..7 {
        let mut pin = held.clone();
        match n {
            0 => pin.download_id = "other".into(),
            1 => pin.timeline_id = "other".into(),
            2 => pin.timeline_revision = "2".into(),
            3 => pin.source_revision = "other".into(),
            4 => pin.viewing_epoch = Some("s:old".into()),
            5 => pin.base_viewing_revision = "-1".into(),
            _ => pin.base_manual_epoch = "no".into(),
        };
        assert!(!restored_pin_admitted("s:new", &pin, Some(&held)));
    }
}
#[test]
fn device_scope_and_effective_duration_are_explicit() {
    assert_eq!(device_admission(None, None), DeviceAdmission::Missing);
    assert_eq!(
        device_admission(Some("a"), Some("b")),
        DeviceAdmission::Foreign
    );
    assert_eq!(device_admission(Some("a"), None), DeviceAdmission::Own);
    for p in [false, true] {
        for owner in ["a", "b"] {
            for device in ["d", "e"] {
                assert_eq!(
                    owned_download("a", "d", owner, device, p),
                    p && owner == "a" && device == "d"
                );
            }
        }
    }
    assert_eq!(offline_duration(Some(12), Some(99)), Some(12));
    assert_eq!(offline_duration(Some(0), Some(99)), Some(99));
    assert_eq!(offline_duration(None, Some(-1)), None);
}
#[test]
fn backup_deletion_requires_committed_withdrawal_and_acknowledgment() {
    let mut reachable = vec![BackupAvailability::Ready];
    for _ in 0..5 {
        for state in reachable.clone() {
            for input in [
                BackupRetentionInput::Expire,
                BackupRetentionInput::RemovalFailed,
                BackupRetentionInput::DurableAbsence,
            ] {
                let next = state.transition(input);
                assert!(
                    !(state == BackupAvailability::Ready && next == BackupAvailability::Reclaimed)
                );
                if input == BackupRetentionInput::RemovalFailed {
                    assert_eq!(next, state);
                }
                if state == BackupAvailability::Reclaimed {
                    assert_eq!(next, state);
                }
                assert_eq!(BackupAvailability::from_marker(next.marker()), Some(next));
                if !reachable.contains(&next) {
                    reachable.push(next);
                }
            }
        }
    }
    assert_eq!(reachable.len(), 3);
}
#[test]
fn references_and_terminal_phase_bound_retention() {
    for kind in [
        HistoryKind::Job,
        HistoryKind::Playback,
        HistoryKind::Processing,
    ] {
        for phase in ["running", "completed", "ended", "cancelled"] {
            for referenced in [false, true] {
                for cleaned in [false, true] {
                    if referenced {
                        assert!(!prune_history(kind, phase, 0, 1, referenced, cleaned));
                    }
                    assert!(!prune_history(kind, phase, 1, 1, referenced, cleaned));
                    if kind == HistoryKind::Processing && !cleaned {
                        assert!(!prune_history(kind, phase, 0, 1, referenced, cleaned));
                    }
                }
            }
        }
    }
}
#[test]
fn legacy_disposition_and_cursor_fence() {
    for legacy in [false, true] {
        for synthetic in [false, true] {
            for count in 0..3 {
                for invitation in [false, true] {
                    assert_eq!(
                        legacy_ownership(legacy, synthetic, count, invitation)
                            == LegacyOwnership::DetachAndDelete,
                        legacy && synthetic && count == 1 && !invitation
                    );
                }
            }
        }
    }
    assert_eq!(dispatch_cursor(1, 2, true, Some("last")), None);
    assert_eq!(
        dispatch_cursor(1, 1, true, None),
        Some(CursorUpdate::Preserve)
    );
    assert_eq!(
        dispatch_cursor(1, 1, false, Some("last")),
        Some(CursorUpdate::Clear)
    );
    assert_eq!(
        dispatch_cursor(1, 1, true, Some("last")),
        Some(CursorUpdate::Advance("last".into()))
    );
}
#[test]
fn publication_and_ticket_interleavings_preserve_exact_identity() {
    for removed in [false, true] {
        for published in [false, true] {
            for staged in [false, true] {
                for blob in [false, true] {
                    for sides in [false, true] {
                        let decision = cache_publication(removed, published, staged, blob, sides);
                        if removed {
                            assert_eq!(decision, CachePublication::Removed);
                        }
                        if decision == CachePublication::VerifyRecovery {
                            assert!(!staged && blob && sides && !removed);
                        }
                    }
                }
            }
        }
    }
    for committed in [false, true] {
        for referenced in [false, true] {
            assert_eq!(
                release_blob(committed, referenced),
                committed && !referenced
            );
        }
    }
    let opening = LocalPlayback::Closed.begin("d");
    assert_eq!(opening.snapshot("other", "old", true), opening);
    assert_eq!(opening.snapshot("d", "old", false), LocalPlayback::Closed);
    let first = opening.snapshot("d", "old", true);
    let next = first.begin("e").snapshot("e", "new", true);
    assert_eq!(next.retire("old"), next);
    assert!(next.admits("new"));
    assert!(!next.admits("old"));
    assert_eq!(next.retire("new"), LocalPlayback::Closed);
}
#[test]
fn first_subtitle_artifact_is_immutable_and_cancel_is_terminal() {
    let first = SubtitleArtifact::Ready {
        digest: "a".into(),
        size: 4,
    };
    let other = SubtitleArtifact::Ready {
        digest: "b".into(),
        size: 4,
    };
    assert_eq!(
        publish_subtitle(true, &SubtitleArtifact::Pending, &first),
        SubtitlePublication::Record
    );
    assert_eq!(
        publish_subtitle(true, &first, &first),
        SubtitlePublication::Unchanged
    );
    assert_eq!(
        publish_subtitle(true, &first, &other),
        SubtitlePublication::Changed
    );
    assert_eq!(
        publish_subtitle(false, &first, &first),
        SubtitlePublication::Cancelled
    );
    assert_eq!(
        publish_subtitle(true, &SubtitleArtifact::Failed, &first),
        SubtitlePublication::Changed
    );
}
#[test]
fn restore_publication_and_baseline_are_acknowledgment_fenced() {
    for bits in 0..32 {
        let progress = RestoreProgress {
            snapshot_verified: bits & 1 != 0,
            history_rotated: bits & 2 != 0,
            authority_withdrawn: bits & 4 != 0,
            baseline_captured: bits & 8 != 0,
            database_durable: bits & 16 != 0,
        };
        assert_eq!(progress.publishable(true), bits == 31);
        assert_eq!(progress.publishable(false), bits & 17 == 17);
    }
    assert!(capture_baseline("current", Some("current"), false));
    assert!(!capture_baseline("current", Some("old"), false));
    assert!(!capture_baseline("current", Some("current"), true));
    let contract: Vec<(String, String)> = serde_json::from_str(RESTORE_AUTHORITY).unwrap();
    assert_eq!(contract.len(), 13);
    assert!(
        contract
            .iter()
            .any(|(table, sql)| table == "catalog_review_plans"
                && sql == "DELETE FROM catalog_review_plans")
    );
    assert!(
        !contract
            .iter()
            .any(|(table, _)| table == "catalog_receipts")
    );
    assert!(
        contract
            .iter()
            .any(|(table, _)| table == "signin_mail_tests")
    );
}
#[test]
fn automatic_backup_ownership_age_and_rank_bound_deletion() {
    for owned in [false, true] {
        for auto in [false, true] {
            for partial in [false, true] {
                for age in [86400, 86401] {
                    for valid in [false, true] {
                        let d = automatic_backup(owned, auto, partial, age, valid, true);
                        if !owned || !auto {
                            assert_eq!(d, AutomaticBackup::Ignore);
                        }
                        if d == AutomaticBackup::RemoveAbandoned {
                            assert!(owned && auto && partial && age > 86400);
                        }
                        if d == AutomaticBackup::RetainCandidate {
                            assert!(owned && auto && !partial && valid);
                        }
                    }
                }
            }
        }
    }
    assert!(retain_backup_rank(0, 1));
    assert!(!retain_backup_rank(1, 1));
    assert!(!retain_backup_rank(0, 0));
}
#[test]
fn dependency_reference_policy_excludes_unrelated_changes() {
    let old = PreparationFileDependency {
        revision: "r",
        available: true,
        library: "lib",
        edition: Some("ed"),
    };
    assert!(!preparation_file_invalidated(&old, &old, true));
    let new = PreparationFileDependency {
        revision: "r2",
        ..old.clone()
    };
    assert!(preparation_file_invalidated(&old, &new, true));
    assert!(!preparation_file_invalidated(&old, &new, false));
    for topic in [
        "catalog",
        "metadata",
        "libraries",
        "sources",
        "devices",
        "profiles",
    ] {
        assert!(smart_collection_invalidated(topic, "x", None));
    }
    assert!(smart_collection_invalidated("viewing", "p", Some("p")));
    assert!(!smart_collection_invalidated("viewing", "p", Some("q")));
    assert!(!smart_collection_invalidated(
        "organization",
        "p",
        Some("p")
    ));
}
#[test]
fn subtitle_waiters_share_failures_without_republishing_cancelled_demand() {
    for active in [false, true] {
        for failed in [false, true] {
            for cached in [false, true] {
                for recent in [false, true] {
                    let next = subtitle_work(active, failed, cached, recent);
                    let expected = if !active {
                        SubtitleWork::Cancelled
                    } else if failed {
                        SubtitleWork::PermanentFailure
                    } else if cached {
                        SubtitleWork::ReadArtifact
                    } else if recent {
                        SubtitleWork::SharedFailure
                    } else {
                        SubtitleWork::Convert
                    };
                    assert_eq!(next, expected);
                }
            }
        }
    }
}

#[test]
fn cache_content_identity_and_shared_sidecar_release_are_exact() {
    let a = CacheEntryIdentity {
        download: "a".into(),
        media: CacheBlob {
            digest: "media".into(),
            size: 100,
        },
        artwork: Some((
            CacheBlob {
                digest: "poster".into(),
                size: 5,
            },
            "image/jpeg".into(),
        )),
        subtitles: Some(CacheBlob {
            digest: "text".into(),
            size: 9,
        }),
    };
    let mut b = a.clone();
    b.download = "b".into();
    assert_eq!(
        a.publication(std::slice::from_ref(&a), &[], false, false, false),
        CachePublication::AlreadyPublished
    );
    for change in 0..4 {
        let mut changed = a.clone();
        match change {
            0 => changed.media.size += 1,
            1 => changed.artwork.as_mut().unwrap().1 = "image/png".into(),
            2 => changed.subtitles.as_mut().unwrap().size += 1,
            _ => changed.download = "other".into(),
        }
        assert_ne!(
            changed.publication(std::slice::from_ref(&a), &[], false, false, false),
            CachePublication::AlreadyPublished
        );
    }
    assert_eq!(
        a.publication(std::slice::from_ref(&a), &["a".into()], true, true, true),
        CachePublication::Removed
    );
    assert!(cache_releases(&[a.clone(), b.clone()], std::slice::from_ref(&b)).is_empty());
    b.media.digest = "different".into();
    assert_eq!(
        cache_releases(std::slice::from_ref(&a), &[b]),
        vec!["media"]
    );
    assert_eq!(cache_releases(&[a], &[]), vec!["media", "poster", "text"]);
}

#[test]
fn schedule_authority_is_reobserved_for_the_exact_task() {
    for task in ["scan", "metadata_refresh", "preview", "unknown"] {
        for flags in 0..32 {
            let owner = flags & 1 != 0;
            let scope = flags & 2 != 0;
            let manage = flags & 4 != 0;
            let catalog = flags & 8 != 0;
            let processing = flags & 16 != 0;
            let expected = owner
                && scope
                && manage
                && (task == "scan"
                    || (task == "metadata_refresh" && catalog)
                    || (task == "preview" && processing));
            assert_eq!(
                schedule_admitted(task, owner, scope, manage, catalog, processing),
                expected
            );
        }
    }
}
