//! Explicit catalog structure: timelines, multi-episode versions, episode
//! order groups and item relationships over real SQLite.
use playscale::{App, curation, db};
use playscale_core::identity::{
    Binding, Equivalence, IdentityError, RelationshipKind, VersionRequest,
};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

async fn fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let app = App {
        health: Arc::new(playscale::operations::Health::new(false)),
        db,
        admin_token: Arc::new("test-secret-token".into()),
        origin: Arc::new("http://127.0.0.1:8787".into()),
        authority: Arc::new("127.0.0.1:8787".into()),
        ffprobe: Arc::new("ffprobe".into()),
        jobs: Arc::new(Mutex::new(())),
        streams: Arc::new(Semaphore::new(2)),
        event_streams: Arc::new(Semaphore::new(2)),
        storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
        processing: Arc::new(playscale::processing::Runtime::new(
            dir.path().join("cache"),
            Default::default(),
        )),
    };
    // A series with one season and two episodes; episode 1's file holds both
    // episodes (a double episode).
    for sql in [
        "INSERT INTO libraries (id,name,root,root_identity) VALUES ('lib','TV','/tv','1:2')",
        "INSERT INTO items (id,title,kind) VALUES ('show','Show','video'),('s1','Season 1','video'),('e1','Pilot','video'),('e2','Second','video')",
        "INSERT INTO item_origins VALUES ('show','Show'),('s1','Season 1'),('e1','Pilot'),('e2','Second')",
        "INSERT INTO item_structure (item_id,media_type,parent_id,number) VALUES ('show','series',NULL,NULL),('s1','season','show',1),('e1','episode','s1',1),('e2','episode','s1',2)",
        "INSERT INTO editions (id,item_id,label) VALUES ('show-ed','show','Broadcast'),('e1-ed','e1','Original'),('e2-ed','e2','Original')",
        "INSERT INTO timelines (id,edition_id) VALUES ('e1-ed','e1-ed'),('e2-ed','e2-ed')",
        "INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json) VALUES ('double','e1-ed','lib','s01e01-e02.mkv','r','s',1,'[]')",
    ] {
        sqlx::query(sql).execute(&app.db).await.unwrap();
    }
    (dir, app)
}

async fn revision(app: &App, item: &str) -> u64 {
    let mut conn = app.db.acquire().await.unwrap();
    curation::load_aggregate(&mut conn, item)
        .await
        .unwrap()
        .unwrap()
        .work
        .revision
}

fn part(start: u64, end: u64) -> Binding {
    Binding {
        file_id: "double".into(),
        revision: "r".into(),
        part: 1,
        start_ms: Some(start),
        end_ms: Some(end),
    }
}

#[tokio::test]
async fn a_double_episode_file_binds_known_intervals_to_both_episodes() {
    let (_dir, app) = fixture().await;
    let request = |b: Binding| VersionRequest {
        bindings: vec![b],
        equivalence: Equivalence::Unknown,
    };
    let e1 = curation::create_version_explicit(
        &app,
        "e1-ed",
        "Part 1",
        &request(part(0, 1_500_000)),
        revision(&app, "e1").await,
    )
    .await
    .unwrap();
    // Episode 2 (another work and edition) binds the second interval.
    curation::create_version_explicit(
        &app,
        "e2-ed",
        "Part 2",
        &request(part(1_500_000, 3_000_000)),
        revision(&app, "e2").await,
    )
    .await
    .unwrap();
    // Overlap, unknown split points and stale review are rejected.
    for bad in [
        part(1_000_000, 2_000_000),
        Binding {
            start_ms: None,
            end_ms: None,
            ..part(0, 1)
        },
    ] {
        assert!(
            curation::create_version_explicit(
                &app,
                "e2-ed",
                "Bad",
                &request(bad),
                revision(&app, "e2").await
            )
            .await
            .is_err()
        );
    }
    assert!(matches!(
        curation::create_version_explicit(
            &app,
            "e2-ed",
            "Stale",
            &request(part(3_000_000, 4_000_000)),
            0
        )
        .await,
        Err(curation::CurationError::Rejected(
            IdentityError::StaleRevision(_)
        ))
    ));
    let mismatched: i64 = sqlx::query_scalar("SELECT count(*) FROM version_edition_mismatch")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(mismatched, 0, "interval bindings may cross editions");
    // The shared file can no longer be reassigned to a single edition.
    let mut tx = db::begin_write(&app.db).await.unwrap();
    assert!(
        curation::reassign_file(&mut tx, "double", "e2-ed")
            .await
            .is_err()
    );
    drop(tx);
    let _ = e1;
}

#[tokio::test]
async fn timelines_join_order_groups_of_their_series_once_per_position() {
    let (_dir, app) = fixture().await;
    let aired = curation::create_order_group(&app, "show-ed", "Aired", curation::OrderKind::Aired)
        .await
        .unwrap();
    let r = curation::place_timeline(&app, "e1-ed", &aired, 1, 1)
        .await
        .unwrap();
    assert!(
        curation::place_timeline(&app, "e2-ed", &aired, 1, r)
            .await
            .is_err(),
        "position taken"
    );
    assert!(
        curation::place_timeline(&app, "e2-ed", &aired, 2, 1)
            .await
            .is_err(),
        "stale group"
    );
    curation::place_timeline(&app, "e2-ed", &aired, 2, r)
        .await
        .unwrap();
    // A timeline of an unrelated work cannot join the series' order.
    sqlx::query("INSERT INTO items (id,title,kind) VALUES ('film','Film','video')")
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO editions (id,item_id,label) VALUES ('film-ed','film','Original')")
        .execute(&app.db)
        .await
        .unwrap();
    let film_tl = curation::create_timeline(
        &app,
        "film",
        "film-ed",
        revision(&app, "film").await,
        Some(6_000_000),
    )
    .await
    .unwrap();
    let r = r + 1;
    assert!(
        curation::place_timeline(&app, &film_tl, &aired, 3, r)
            .await
            .is_err()
    );
    assert!(
        curation::create_timeline(&app, "film", "e1-ed", revision(&app, "film").await, None)
            .await
            .is_err(),
        "edition of another work"
    );
}

#[tokio::test]
async fn relationships_reject_cycles_and_duplicates() {
    let (_dir, app) = fixture().await;
    let edge =
        curation::create_relationship(&app, "e1", "show", RelationshipKind::ExtraOf, Some(0))
            .await
            .unwrap();
    assert!(
        curation::create_relationship(&app, "show", "e1", RelationshipKind::ExtraOf, None)
            .await
            .is_err()
    );
    assert!(
        curation::create_relationship(&app, "e1", "show", RelationshipKind::ExtraOf, None)
            .await
            .is_err()
    );
    assert!(
        curation::create_relationship(&app, "e1", "e1", RelationshipKind::CreatedBy, None)
            .await
            .is_err()
    );
    assert_eq!(
        curation::relationships(&app.db, "show")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        curation::delete_relationship(&app, &edge.id, 0)
            .await
            .is_err()
    );
    curation::delete_relationship(&app, &edge.id, 1)
        .await
        .unwrap();
    assert!(
        curation::relationships(&app.db, "show")
            .await
            .unwrap()
            .is_empty()
    );
    let files = curation::item_files(&app.db, "e1").await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].id, "double");
}

async fn group_revision(app: &App, group: &str) -> u64 {
    let r: i64 = sqlx::query_scalar("SELECT revision FROM order_groups WHERE id=?")
        .bind(group)
        .fetch_one(&app.db)
        .await
        .unwrap();
    r as u64
}

#[tokio::test]
async fn order_and_relationships_follow_moves_merges_and_splits() {
    let (_dir, app) = fixture().await;
    let aired = curation::create_order_group(&app, "show-ed", "Aired", curation::OrderKind::Aired)
        .await
        .unwrap();
    let dvd = curation::create_order_group(&app, "show-ed", "DVD", curation::OrderKind::Dvd)
        .await
        .unwrap();
    curation::place_timeline(&app, "e1-ed", &aired, 1, 1)
        .await
        .unwrap();
    let aired_seen = group_revision(&app, &aired).await;
    // Moving e1 to the DVD order changes the aired order too.
    curation::place_timeline(&app, "e1-ed", &dvd, 1, 1)
        .await
        .unwrap();
    assert!(
        curation::place_timeline(&app, "e2-ed", &aired, 1, aired_seen)
            .await
            .is_err()
    );

    // Merging e1 into e2 remaps its relationship onto e2.
    curation::create_relationship(&app, "e1", "show", RelationshipKind::ExtraOf, None)
        .await
        .unwrap();
    let mut conn = app.db.acquire().await.unwrap();
    let mut expected = std::collections::BTreeMap::new();
    for id in ["e1", "e2"] {
        let a = curation::load_aggregate(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        expected.insert(id.to_string(), a.work.revision);
    }
    drop(conn);
    let request = playscale_core::identity::MergeRequest {
        sources: vec!["e1".into()],
        target: "e2".into(),
        expected,
    };
    let show_before = revision(&app, "show").await;
    let plan = curation::preview_merge(&app.db, &request).await.unwrap();
    curation::commit_merge(&app, &plan).await.unwrap();
    assert!(
        revision(&app, "show").await > show_before,
        "neighbor's structure changed"
    );
    let edges = curation::relationships(&app.db, "show").await.unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source_item_id, "e2");

    // Splitting e2's ordered timeline into a new work clears the membership.
    curation::place_timeline(&app, "e2-ed", &aired, 2, group_revision(&app, &aired).await)
        .await
        .unwrap();
    sqlx::query("INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json) VALUES ('e2-file','e2-ed','lib','e2.mkv','r2','s',1,'[]')")
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO media_versions (id,timeline_id,origin,equivalence) VALUES ('e2-v','e2-ed','original','declared')")
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO version_files (version_id,part,file_id,file_revision) VALUES ('e2-v',1,'e2-file','r2')")
        .execute(&app.db)
        .await
        .unwrap();
    // e1's merged edition keeps a version, so the split leaves e2 non-empty.
    for sql in [
        "INSERT INTO media_versions (id,timeline_id,origin,equivalence) VALUES ('dv','e1-ed','original','declared')",
        "INSERT INTO version_files (version_id,part,file_id,file_revision) VALUES ('dv',1,'double','r')",
    ] {
        sqlx::query(sql).execute(&app.db).await.unwrap();
    }
    let before = group_revision(&app, &aired).await;
    let split = curation::preview_split(
        &app.db,
        &playscale_core::identity::SplitRequest {
            item: "e2".into(),
            versions: vec!["e2-v".into()],
            new_title: "Mis-filed".into(),
            expected_revision: {
                let mut conn = app.db.acquire().await.unwrap();
                curation::load_aggregate(&mut conn, "e2")
                    .await
                    .unwrap()
                    .unwrap()
                    .work
                    .revision
            },
        },
    )
    .await
    .unwrap();
    curation::commit_split(&app, &split).await.unwrap();
    let membership: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT t.order_group_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE e.item_id=?",
    )
    .bind(&split.new_item)
    .fetch_all(&app.db)
    .await
    .unwrap();
    assert!(membership.iter().all(Option::is_none), "{membership:?}");
    assert!(group_revision(&app, &aired).await > before);
}

#[tokio::test]
async fn merges_that_would_create_relationship_cycles_are_rejected() {
    let (_dir, app) = fixture().await;
    sqlx::query("INSERT INTO items (id,title,kind) VALUES ('x','X','video'),('y','Y','video'),('dup','Dup','video')")
        .execute(&app.db)
        .await
        .unwrap();
    curation::create_relationship(&app, "x", "y", RelationshipKind::PartOf, None)
        .await
        .unwrap();
    curation::create_relationship(&app, "y", "dup", RelationshipKind::PartOf, None)
        .await
        .unwrap();
    let mut conn = app.db.acquire().await.unwrap();
    let mut expected = std::collections::BTreeMap::new();
    for id in ["dup", "x"] {
        let a = curation::load_aggregate(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        expected.insert(id.to_string(), a.work.revision);
    }
    drop(conn);
    let request = playscale_core::identity::MergeRequest {
        sources: vec!["dup".into()],
        target: "x".into(),
        expected,
    };
    assert!(matches!(
        curation::preview_merge(&app.db, &request).await,
        Err(curation::CurationError::Rejected(
            IdentityError::InvalidBindings(_)
        ))
    ));
}
