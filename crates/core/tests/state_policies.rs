use playscale_core::{
    jobs::{Effect, Input, Job, Phase, transition},
    maintenance::{CacheEntry, due, enough_space},
    scan::{
        Assignment, Coverage, Existing, Observed, Outcome, outcome, reconcile, reconcile_covered,
    },
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
    let one = Assignment::Existing {
        id: "one".into(),
        edition: "edition-one".into(),
    };
    assert_eq!(moved.assignments, vec![one.clone()]);
    assert_eq!(moved.unavailable, vec!["one"]);
    // Two identical observations cannot both be the moved occurrence; they are
    // copies of the one edition holding that verified content.
    let copy = Assignment::Copy {
        edition: "edition-one".into(),
    };
    assert_eq!(
        reconcile(&old, &[observed("new"), observed("copy")]).assignments,
        vec![copy.clone(), copy]
    );
    // Content held by two editions is ambiguous: a new work, never a guess.
    assert_eq!(
        reconcile(
            &[file("one", "old"), file("two", "copy")],
            &[observed("new")]
        )
        .assignments,
        vec![Assignment::New]
    );
    let replaced = reconcile(
        &old,
        &[Observed {
            path: "old".into(),
            revision: "replacement".into(),
        }],
    );
    assert_eq!(replaced.assignments[0], one);
    assert!(replaced.unavailable.is_empty());
}
#[test]
fn absence_requires_a_covered_directory_listing() {
    let file = |id: &str, path: &str, revision: &str| Existing {
        id: id.into(),
        edition: format!("edition-{id}"),
        path: path.into(),
        revision: revision.into(),
    };
    let old = vec![
        file("root", "a.mkv", "ra"),
        file("listed", "ok/b.mkv", "rb"),
        file("unread", "locked/c.mkv", "rc"),
        file("deep", "ok/unvisited/d.mkv", "rd"),
        file("gone", "removed/sub/e.mkv", "re"),
    ];
    let coverage = Coverage::Listed {
        complete: ["".into(), "ok".into()].into(),
        incomplete: ["locked".into(), "ok/unvisited".into()].into(),
    };
    let plan = reconcile_covered(&old, &[], &coverage);
    // "removed" did not appear in the complete root listing, so its whole
    // subtree is proven absent; unread and unvisited directories are not.
    assert_eq!(plan.unavailable, vec!["root", "listed", "gone"]);
    // A file seen at a new path whose content is still unproven elsewhere is a
    // copy, not a move: the unread original may still exist.
    let plan = reconcile_covered(
        &old,
        &[Observed {
            path: "ok/c-renamed.mkv".into(),
            revision: "rc".into(),
        }],
        &coverage,
    );
    assert_eq!(
        plan.assignments,
        vec![Assignment::Copy {
            edition: "edition-unread".into()
        }]
    );
    assert!(!plan.unavailable.contains(&"unread".to_string()));
    assert_eq!(outcome(false, 0), None);
    assert_eq!(outcome(true, 0), Some(Outcome::Complete));
    assert_eq!(outcome(true, 3), Some(Outcome::Partial));
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
                for binding_now in [7, 8] {
                    let (next, effects) = finish(
                        &job,
                        &Completion {
                            attempt,
                            complete_inventory_roots: root.clone().map(|r: String| (r.clone(), r)),
                            current_root: "current".into(),
                            library_enabled: enabled,
                            binding_at_start: 7,
                            binding_now,
                        },
                    );
                    // A rebind during the attempt fences publication.
                    let publish = attempt == 2
                        && enabled
                        && binding_now == 7
                        && root.as_deref() == Some("current");
                    assert_eq!(effects.contains(&Effect::Publish { attempt: 2 }), publish);
                    if attempt != 2 {
                        assert_eq!(next, job);
                    }
                }
            }
        }
    }
}

#[test]
fn exclusions_take_files_out_of_scope_without_proving_absence() {
    use playscale_core::scan::reconcile_scoped;
    let file = |id: &str, path: &str| Existing {
        id: id.into(),
        edition: format!("e-{id}"),
        path: path.into(),
        revision: format!("r-{id}"),
    };
    let observed = |path: &str, revision: &str| Observed {
        path: path.into(),
        revision: revision.into(),
    };
    let old = vec![file("keep", "a.mkv"), file("extra", "Extras/b.mkv")];
    let found = vec![
        observed("Extras/b.mkv", "r-extra"),
        observed("a.mkv", "r-keep"),
        observed("c.mkv", "new"),
        observed("Extras/c.mkv", "new"),
    ];
    let (plan, excluded) = reconcile_scoped(&old, &found, &Coverage::Complete, &["Extras".into()]);
    assert_eq!(excluded, ["extra"]);
    assert!(plan.unavailable.is_empty(), "exclusion is not absence");
    assert_eq!(plan.assignments.len(), found.len());
    assert_eq!(plan.assignments[0], Assignment::OutOfScope);
    assert!(matches!(plan.assignments[1], Assignment::Existing { .. }));
    assert_eq!(plan.assignments[2], Assignment::New);
    assert_eq!(plan.assignments[3], Assignment::OutOfScope);
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
