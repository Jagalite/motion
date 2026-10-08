use playscale_core::{
    jobs::{Effect, Input, Job, Phase, transition},
    maintenance::{CacheEntry, due, enough_space},
    scan::{Existing, Observed, reconcile},
    viewing::{FileIdentity, Session, State, check_file, check_switch, event},
};
#[test]
fn attempts_and_revisions_do_not_wrap() {
    for phase in [
        Phase::Queued,
        Phase::Running,
        Phase::Cancelling,
        Phase::Cancelled,
        Phase::Failed,
        Phase::Completed,
    ] {
        let state = Job {
            phase,
            attempt: u32::MAX,
        };
        for input in [
            Input::Start,
            Input::Retry,
            Input::Recover,
            Input::Cancel,
            Input::Finished {
                attempt: u32::MAX,
                success: true,
            },
        ] {
            let (next, effects) = transition(&state, input);
            assert_eq!(next.attempt, u32::MAX);
            assert!(!effects.iter().any(|e| matches!(e, Effect::Run { .. })));
        }
    }
    assert!(playscale_core::revision::advance(i64::MAX, i64::MAX).is_err());
    assert!(playscale_core::revision::advance(-1, -1).is_err());
    assert!(playscale_core::revision::advance(4, 3).is_err());
    assert_eq!(
        playscale_core::revision::advance(i64::MAX - 1, i64::MAX - 1),
        Ok(i64::MAX)
    );
}
#[test]
fn scan_moves_require_unique_missing_and_observed_revisions() {
    let file = |id: &str, path: &str| Existing {
        id: id.into(),
        edition: format!("edition-{id}"),
        path: path.into(),
        revision: "hash".into(),
    };
    let observed = |path: &str| Observed {
        path: path.into(),
        revision: "hash".into(),
    };
    let old = vec![file("one", "old")];
    let moved = reconcile(&old, &[observed("new")]);
    assert_eq!(
        moved.identities,
        vec![Some(("one".into(), "edition-one".into()))]
    );
    assert_eq!(moved.unavailable, vec!["one"]);
    assert_eq!(
        reconcile(&old, &[observed("new"), observed("copy")]).identities,
        vec![None, None]
    );
    assert_eq!(
        reconcile(
            &[file("one", "old"), file("two", "copy")],
            &[observed("new")]
        )
        .identities,
        vec![None]
    );
    let replaced = reconcile(
        &old,
        &[Observed {
            path: "old".into(),
            revision: "replacement".into(),
        }],
    );
    assert_eq!(replaced.identities[0], moved.identities[0]);
    assert!(replaced.unavailable.is_empty());
}
#[test]
fn retention_respects_active_work_playback_and_attempt_races() {
    for phase in [
        Phase::Queued,
        Phase::Running,
        Phase::Cancelling,
        Phase::Cancelled,
        Phase::Failed,
        Phase::Completed,
    ] {
        let state = CacheEntry {
            job: Job { phase, attempt: 2 },
            cleaned: false,
            updated_at: 0,
            last_active_playback: None,
        };
        assert_eq!(
            state.removable(10000, 100),
            matches!(phase, Phase::Cancelled | Phase::Failed | Phase::Completed)
        );
        assert!(
            !CacheEntry {
                last_active_playback: Some(9999),
                ..state.clone()
            }
            .removable(10000, 100)
        );
        assert!(
            !CacheEntry {
                cleaned: true,
                ..state.clone()
            }
            .removable(10000, 100)
        );
        assert!(!state.acknowledge_removal(1));
        assert_eq!(
            state.abandoned_attempt(),
            (phase == Phase::Queued).then_some(2)
        );
    }
    let state = CacheEntry {
        job: Job {
            phase: Phase::Completed,
            attempt: 1,
        },
        cleaned: false,
        updated_at: 900,
        last_active_playback: None,
    };
    assert!(!state.removable(1000, 100));
    assert!(state.removable(1001, 100));
    assert!(!enough_space(u64::MAX, u64::MAX, 1));
    assert!(enough_space(100, 60, 40));
    assert!(!due(Some(200), 100, 10));
    assert!(!due(None, 100, 0));
    assert!(due(Some(90), 100, 10));
}
#[test]
fn viewing_completion_override_ownership_retries_and_file_switches() {
    let state = State {
        revision: 0,
        session_id: None,
        position: 0.0,
        automatic_watched: false,
        manual_watched: None,
    };
    let active = state.start(0, "first".into(), Some(100.0)).unwrap();
    let paused = Session {
        sequence: 0,
        position: 0.0,
        status: "paused".into(),
    };
    let ended = Session {
        sequence: 1,
        position: 100.0,
        status: "ended".into(),
    };
    let (session, completed) = event(&active, "first", &paused, &ended, Some(100.0))
        .unwrap()
        .unwrap();
    assert!(completed.watched());
    assert!(
        event(&completed, "first", &session, &ended, Some(100.0))
            .unwrap()
            .is_none()
    );
    let manual = completed
        .override_watched(completed.revision, Some(false))
        .unwrap();
    assert!(!manual.watched());
    assert!(manual.automatic_watched);
    assert!(manual.legacy(20.0).is_err());
    assert!(
        manual
            .override_watched(manual.revision, None)
            .unwrap()
            .watched()
    );
    let newer = manual
        .start(manual.revision, "second".into(), Some(20.0))
        .unwrap();
    assert_eq!(newer.position, 20.0);
    assert!(event(&newer, "first", &session, &ended, None).is_err());
    let file = FileIdentity {
        id: "file".into(),
        revision: "one".into(),
    };
    let other = FileIdentity {
        id: "other".into(),
        revision: "one".into(),
    };
    assert_eq!(
        check_switch(&paused, &file, &paused, Some(&other)),
        Err("stale_sequence")
    );
    assert_eq!(
        check_switch(&paused, &file, &ended, Some(&other)),
        Err("invalid_file_switch_status")
    );
    assert!(check_file(&file, "two", true).is_err());
    assert!(check_file(&file, "one", false).is_err());
}

#[test]
fn incomplete_replaced_or_disabled_scan_cannot_publish() {
    use playscale_core::scan::{Completion, finish};
    let job = Job {
        phase: Phase::Running,
        attempt: 2,
    };
    for root in [None, Some("old".into()), Some("current".into())] {
        for enabled in [false, true] {
            for attempt in [1, 2, 3] {
                let (next, effects) = finish(
                    &job,
                    &Completion {
                        attempt,
                        complete_inventory_roots: root.clone().map(|r: String| (r.clone(), r)),
                        current_root: "current".into(),
                        library_enabled: enabled,
                    },
                );
                let publish = attempt == 2 && enabled && root.as_deref() == Some("current");
                assert_eq!(effects.contains(&Effect::Publish { attempt: 2 }), publish);
                if attempt != 2 {
                    assert_eq!(next, job);
                }
            }
        }
    }
}

#[test]
fn rendition_registration_and_session_eligibility_bind_both_revisions() {
    use playscale_core::{
        renditions::Identity,
        viewing::{FileState, RenditionBinding},
    };
    let registration = Identity {
        item: "item".into(),
        file: "output".into(),
        file_revision: "out-v1".into(),
        source_file: "source".into(),
        source_revision: "src-v1".into(),
        recipe: serde_json::json!({"recipe":"test"}),
    };
    assert!(
        registration
            .register(Some(&registration), "item", "src-v1", "out-v1")
            .is_ok()
    );
    assert!(
        registration
            .register(None, "item", "src-v2", "out-v1")
            .is_err()
    );
    let mut changed = registration.clone();
    changed.recipe = serde_json::json!({"recipe":"other"});
    assert!(
        changed
            .register(Some(&registration), "item", "src-v1", "out-v1")
            .is_err()
    );
    let mut file = FileState {
        identity: FileIdentity {
            id: "output".into(),
            revision: "out-v1".into(),
        },
        item: "item".into(),
        generated: true,
        available: true,
        duration: Some(100.0),
        renditions: vec![RenditionBinding {
            item: "item".into(),
            output_revision: "out-v1".into(),
            source_revision: "src-v1".into(),
            current_source_revision: "src-v1".into(),
        }],
    };
    assert!(file.eligible("item", true));
    file.renditions[0].current_source_revision = "src-v2".into();
    assert!(!file.eligible("item", false));
    file.generated = false;
    assert!(file.eligible("item", true));
    file.available = false;
    assert!(!file.eligible("item", true));
    assert!(file.eligible("item", false));
}
#[test]
fn output_observations_must_meet_duration_track_codec_and_budget_rules() {
    use playscale_core::processing::*;
    let tracks = vec![Track {
        kind: "video".into(),
        codec: "h264".into(),
        width: Some(1280),
        height: Some(720),
        frame_rate: Some(30.0),
    }];
    let requirements = OutputRequirements {
        max_bytes: 1000000,
        source_duration: 10.0,
        source_tracks: &tracks,
        recipe: "h264720p",
        video: Some(VideoRequirements {
            codec: "h264".into(),
            max_width: 1280,
            max_height: 720,
            frame_rate: Some(30.0),
        }),
    };
    let mut observation = OutputObservation {
        bytes: 100,
        duration: Some(10.0),
        tracks: &tracks,
    };
    assert!(validate_output(&observation, &requirements).is_ok());
    observation.duration = Some(1.0);
    assert!(validate_output(&observation, &requirements).is_err());
    observation.duration = Some(10.0);
    observation.bytes = 999999;
    assert!(validate_output(&observation, &requirements).is_err());
    observation.bytes = 100;
    observation.tracks = &[];
    assert!(validate_output(&observation, &requirements).is_err());
    let wrong = vec![Track {
        codec: "hevc".into(),
        ..tracks[0].clone()
    }];
    observation.tracks = &wrong;
    assert!(validate_output(&observation, &requirements).is_err());
}

#[test]
fn exhausted_attempts_become_terminal_and_cannot_retry() {
    for phase in [Phase::Queued, Phase::Running] {
        let before = Job {
            phase,
            attempt: u32::MAX,
        };
        let input = if phase == Phase::Queued {
            Input::Start
        } else {
            Input::Recover
        };
        let (failed, effects) = transition(&before, input);
        assert_eq!(failed.phase, Phase::Failed);
        assert_eq!(failed.attempt, u32::MAX);
        assert!(effects.is_empty());
        assert!(!failed.retryable());
        assert_eq!(transition(&failed, Input::Retry), (failed, vec![]));
    }
}
