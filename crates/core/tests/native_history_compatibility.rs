//! Production history policy used by the native compatibility adapter.
//! These tests prove reducer semantics, not native projection, physical import,
//! HTTP admission, or permission to remap a retired timeline to a survivor.
use playscale_core::{downloads, timeline_viewing as viewing};

#[test]
fn shorter_current_delivery_clamps_only_on_explicit_new_session() {
    for automatic_watched in [false, true] {
        for manual_watched in [None, Some(false), Some(true)] {
            let retained = viewing::View {
                revision: 41,
                manual_epoch: 7,
                position_ms: 9_000,
                automatic_watched,
                manual_watched,
                session: Some("historical-session".into()),
            };
            let original = retained.clone();
            // The adapter must retain this row verbatim while rebuilding its
            // catalog projection. Only this explicit production transition
            // adopts the duration of the new, exact-timeline delivery.
            let started = viewing::start(
                &retained,
                41,
                "new-session".into(),
                "current-delivery".into(),
                Some(5_000),
            )
            .unwrap();
            assert_eq!(retained, original);
            assert_eq!(started.view.position_ms, 5_000);
            assert_eq!(started.view.revision, 42);
            assert_eq!(started.view.manual_epoch, 7);
            assert_eq!(started.view.automatic_watched, automatic_watched);
            assert_eq!(started.view.manual_watched, manual_watched);
            assert_eq!(started.view.watched(), retained.watched());
            assert_eq!(started.superseded.as_deref(), Some("historical-session"));
            assert_eq!(started.session.id, "new-session");
            assert_eq!(started.session.delivery, "current-delivery");
            assert_eq!(started.session.position_ms, 5_000);
            assert_eq!(started.session.duration_ms, Some(5_000));
            assert_eq!(started.session.manual_epoch, 7);
            assert_eq!(started.session.sequence, 0);
            assert_eq!(started.session.status, viewing::Status::Paused);
            assert_eq!(
                viewing::start(
                    &retained,
                    40,
                    "stale".into(),
                    "current-delivery".into(),
                    Some(5_000)
                )
                .unwrap_err(),
                viewing::Error::RevisionConflict
            );
            assert_eq!(retained, original);
        }
    }
}

#[test]
fn manual_epoch_changes_only_through_explicit_history_override() {
    let retained = viewing::View {
        revision: 4,
        manual_epoch: 2,
        position_ms: 900,
        automatic_watched: true,
        manual_watched: Some(false),
        session: None,
    };
    let overridden = viewing::override_watched(&retained, 4, Some(true), false).unwrap();
    assert_eq!(overridden.view.revision, 5);
    assert_eq!(overridden.view.manual_epoch, 3);
    assert_eq!(overridden.view.position_ms, 900);
    let started = viewing::start(
        &overridden.view,
        5,
        "session".into(),
        "shorter-delivery".into(),
        Some(500),
    )
    .unwrap();
    assert_eq!(started.view.revision, 6);
    assert_eq!(started.view.manual_epoch, 3);
    assert_eq!(started.view.position_ms, 500);
    assert!(started.view.watched());
    assert_eq!(overridden.view.position_ms, 900);
}

#[test]
fn mapped_timeline_revision_delta_expires_pin_and_retains_offline_event_as_history_only() {
    let pin = downloads::Pin {
        timeline_revision: 7,
        version_id: "version".into(),
        file_id: "file".into(),
        file_revision: "unchanged-content".into(),
    };
    let mut current = downloads::Current {
        timeline_revision: Some(7),
        bound: true,
        file_revision: Some("unchanged-content".into()),
        readable: true,
    };
    assert_eq!(
        downloads::state(downloads::Stored::Ready, &pin, &current),
        downloads::State::Ready
    );
    // The native head's mapped Motion revision changed, although file bytes,
    // IDs and current permission did not. That is not the old download's pin.
    current.timeline_revision = Some(8);
    assert_eq!(
        downloads::state(downloads::Stored::Ready, &pin, &current),
        downloads::State::Expired
    );
    assert!(!downloads::transferable(downloads::state(
        downloads::Stored::Ready,
        &pin,
        &current
    )));
    let history = viewing::View {
        revision: 4,
        manual_epoch: 2,
        position_ms: 250,
        automatic_watched: false,
        manual_watched: None,
        session: None,
    };
    let event = downloads::OfflineEvent {
        id: "old-copy-event".into(),
        sequence: 1,
        download_id: "download".into(),
        timeline_id: "timeline".into(),
        timeline_revision: 7,
        base_viewing_revision: 4,
        base_manual_epoch: 2,
        position_ms: 400,
        status: viewing::Status::Paused,
        device_time: "device-clock".into(),
    };
    let decision = downloads::reconcile(
        downloads::Retention::server(0),
        1,
        &[],
        &event,
        Some(&downloads::Target {
            timeline_revision: 8,
            duration_ms: Some(500),
        }),
        &history,
        None,
        90.0,
    );
    let downloads::Decision::Consumed {
        recorded,
        view,
        fenced,
        lineage,
    } = decision
    else {
        panic!("expected retained historical observation, got {decision:?}");
    };
    assert_eq!(recorded.event, event);
    assert_eq!(recorded.outcome, downloads::Outcome::HistoryOnly);
    assert_eq!(recorded.code, Some(downloads::Code::TimelineChanged));
    assert_eq!(view, None);
    assert_eq!(fenced, None);
    assert_eq!(lineage, None);
    assert_eq!(history.position_ms, 250);
    assert_eq!(history.revision, 4);
    assert_eq!(history.manual_epoch, 2);
}
