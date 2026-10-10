//! Real filesystem/SQLite evidence for guarded observation (A03), the
//! identification workflow (A04) and organization (A09).
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use playscale::{App, api, db, matching, organization as orgs, scan};
use playscale_core::{
    matching::{Decision, MatchError, Status},
    organization::{
        CollectionKind, Entry, Field, Operator, OrganizationError, Repeat, Step, Term, Value,
    },
};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    app: App,
    library: String,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let library = db::add_library(&db, "Library", &root).await.unwrap().id;
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
        Self {
            _dir: dir,
            root,
            app,
            library,
        }
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    async fn scan(&self) -> db::JobRow {
        let row = db::enqueue_mode(&self.app, &self.library, true)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(scan::worker(self.app.clone(), stop.clone()));
        let done = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let row = db::get_job(&self.app.db, &row.id).await.unwrap();
                if !["queued", "running", "cancelling"].contains(&row.phase.as_str()) {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        task.await.unwrap().unwrap();
        done
    }
    async fn file(&self, path: &str) -> (String, String, bool, String) {
        sqlx::query_as(
            "SELECT id,item_id,available,edition_id FROM catalog_files WHERE relative_path=?",
        )
        .bind(path)
        .fetch_one(&self.app.db)
        .await
        .unwrap()
    }
}

fn chmod(path: &std::path::Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

// ---------------------------------------------------------------------------
// A03: guarded observation

#[tokio::test]
async fn unreadable_directory_publishes_partial_without_inferring_absence() {
    let f = Fixture::new().await;
    f.write("a.mp4", b"alpha");
    f.write("locked/b.mp4", b"bravo");
    let first = f.scan().await;
    assert_eq!(
        (first.phase.as_str(), first.outcome.as_deref()),
        ("completed", Some("complete"))
    );
    let locked = f.root.join("locked");
    // The file is really gone, but the directory cannot be listed.
    std::fs::remove_file(locked.join("b.mp4")).unwrap();
    chmod(&locked, 0o000);
    f.write("c.mp4", b"charlie");
    let partial = f.scan().await;
    chmod(&locked, 0o755);
    assert_eq!(partial.phase, "completed");
    assert_eq!(partial.outcome.as_deref(), Some("partial"));
    assert_eq!(partial.incomplete_directories, 1);
    let reasons: Vec<(String, String)> =
        sqlx::query_as("SELECT directory,reason FROM scan_incomplete_directories WHERE job_id=?")
            .bind(&partial.id)
            .fetch_all(&f.app.db)
            .await
            .unwrap();
    assert_eq!(reasons, [("locked".to_string(), "unreadable".to_string())]);
    assert!(
        f.file("locked/b.mp4").await.2,
        "absence unproven: kept available"
    );
    assert!(
        f.file("c.mp4").await.2,
        "discoveries publish from partial coverage"
    );
    let complete = f.scan().await;
    assert_eq!(complete.outcome.as_deref(), Some("complete"));
    assert!(!f.file("locked/b.mp4").await.2, "proven absent once listed");
}

#[tokio::test]
async fn vanished_directory_is_proven_absent_by_its_parent_listing() {
    let f = Fixture::new().await;
    f.write("show/season/e1.mp4", b"episode");
    f.scan().await;
    std::fs::remove_dir_all(f.root.join("show")).unwrap();
    let row = f.scan().await;
    assert_eq!(row.outcome.as_deref(), Some("complete"));
    assert!(!f.file("show/season/e1.mp4").await.2);
}

#[tokio::test]
async fn backup_copy_is_an_occurrence_of_the_same_work() {
    let f = Fixture::new().await;
    f.write("movie.mp4", b"same verified bytes");
    f.scan().await;
    f.write("backup/movie.mp4", b"same verified bytes");
    f.write("other.mp4", b"different bytes");
    f.scan().await;
    let original = f.file("movie.mp4").await;
    let copy = f.file("backup/movie.mp4").await;
    assert_ne!(original.0, copy.0, "separate physical occurrences");
    assert_eq!(original.3, copy.3, "same edition, not a new work");
    assert_ne!(f.file("other.mp4").await.1, original.1);
    let works: i64 = sqlx::query_scalar("SELECT count(*) FROM items")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(works, 2);
}

// ---------------------------------------------------------------------------
// A04: identification

async fn seed_target(f: &Fixture, id: &str, title: &str, year: i64) {
    sqlx::query("INSERT INTO items (id,title,kind) VALUES (?,?,'video')")
        .bind(id)
        .bind(title)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_origins VALUES (?,?)")
        .bind(id)
        .bind(title)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_structure (item_id,media_type) VALUES (?, 'movie')")
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata_documents VALUES (?,'local',1,NULL,?,1)")
        .bind(id)
        .bind(json!({"values":{"release_year":year},"tags":[],"excluded_tags":[]}).to_string())
        .execute(&f.app.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn accepted_local_match_merges_and_survives_refresh() {
    let f = Fixture::new().await;
    f.write("The.Matrix.1999.1080p.mkv", b"matrix");
    f.scan().await;
    sqlx::query(
        "INSERT INTO item_structure (item_id,media_type) SELECT item_id,'movie' FROM catalog_files",
    )
    .execute(&f.app.db)
    .await
    .unwrap();
    seed_target(&f, "matrix", "The Matrix", 1999).await;
    seed_target(&f, "matrix-other-year", "The Matrix", 2021).await;
    let (file, scanned_item, _, _) = f.file("The.Matrix.1999.1080p.mkv").await;

    let proposal = matching::propose(&f.app, &file).await.unwrap();
    assert_eq!(proposal.status, Status::Pending);
    assert_eq!(proposal.candidates.len(), 1, "year disagreement excluded");
    let candidate = proposal.candidates[0].clone();
    assert_eq!(candidate.item_id.as_deref(), Some("matrix"));
    assert_eq!(candidate.reason_codes, ["title", "year"]);

    // A stale review is rejected without effects.
    let stale = matching::decide(
        &f.app,
        &proposal.id,
        proposal.revision - 1,
        Decision::Accept {
            candidate_id: candidate.id.clone(),
        },
    )
    .await;
    assert!(matches!(
        stale,
        Err(matching::MatchingError::Rejected(MatchError::StaleProposal))
    ));

    let decided = matching::decide(
        &f.app,
        &proposal.id,
        proposal.revision,
        Decision::Accept {
            candidate_id: candidate.id,
        },
    )
    .await
    .unwrap();
    assert_eq!(decided.item_id, "matrix");
    assert!(decided.merge_receipt.is_some());
    assert_eq!(f.file("The.Matrix.1999.1080p.mkv").await.1, "matrix");
    let state: String = sqlx::query_scalar("SELECT match_state FROM items WHERE id='matrix'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(state, "manual");
    let alias: String = sqlx::query_scalar("SELECT item_id FROM item_aliases WHERE alias_id=?")
        .bind(&scanned_item)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(alias, "matrix");

    // Re-proposing (refresh) after a decision raises nothing that overrides it.
    let again = matching::get(&f.app.db, &proposal.id).await.unwrap();
    assert_eq!(again.status, Status::Accepted);
    assert!(matches!(
        matching::decide(&f.app, &proposal.id, again.revision, Decision::Reject).await,
        Err(matching::MatchingError::Rejected(
            MatchError::AlreadyDecided
        ))
    ));
}

#[tokio::test]
async fn changed_file_stales_open_proposal() {
    let f = Fixture::new().await;
    f.write("Alien.1979.mkv", b"v1");
    f.scan().await;
    seed_target(&f, "alien", "Alien", 1979).await;
    let (file, ..) = f.file("Alien.1979.mkv").await;
    let proposal = matching::propose(&f.app, &file).await.unwrap();
    f.write("Alien.1979.mkv", b"v2 replaced in place");
    f.scan().await;
    let stale = matching::get(&f.app.db, &proposal.id).await.unwrap();
    assert_eq!(stale.status, Status::Stale);
    assert!(matches!(
        matching::decide(&f.app, &proposal.id, stale.revision, Decision::Defer).await,
        Err(matching::MatchingError::Rejected(MatchError::StaleProposal))
    ));
    // A fresh proposal binds the new file revision.
    let fresh = matching::propose(&f.app, &file).await.unwrap();
    assert_ne!(fresh.id, proposal.id);
    assert_ne!(fresh.file_revision, proposal.file_revision);
}

#[tokio::test]
async fn manual_identity_is_pinned_against_provider_contributions() {
    let f = Fixture::new().await;
    seed_target(&f, "film", "Film", 2000).await;
    sqlx::query("INSERT INTO metadata_documents VALUES ('film','tmdb',1,'603','{\"values\":{},\"tags\":[],\"excluded_tags\":[]}',1)")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE items SET match_state='manual' WHERE id='film'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let put = |external: &str, revision: i64| {
        Request::builder()
            .method("PUT")
            .uri("/api/v1/items/film/metadata/tmdb")
            .header("host", "127.0.0.1:8787")
            .header("authorization", "Bearer test-secret-token")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"expected_revision":revision,"external_id":external,"values":{"title":"Film"},"tags":[]})
                    .to_string(),
            ))
            .unwrap()
    };
    let router = api::router(f.app.clone(), None);
    let refused = router.clone().oneshot(put("604", 1)).await.unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let same = router.oneshot(put("603", 1)).await.unwrap();
    assert_eq!(same.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// A09: organization

async fn work(f: &Fixture, id: &str, title: &str, year: i64) {
    seed_target(f, id, title, year).await;
    sqlx::query("INSERT INTO editions (id,item_id,label) VALUES (?,?,'Original')")
        .bind(format!("{id}-ed"))
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO timelines (id,edition_id) VALUES (?,?)")
        .bind(format!("{id}-ed"))
        .bind(format!("{id}-ed"))
        .execute(&f.app.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn smart_and_manual_collections_are_profile_scoped() {
    let f = Fixture::new().await;
    work(&f, "old", "Old Film", 1980).await;
    work(&f, "new", "New Film", 2010).await;
    work(&f, "newer", "Newer Film", 2020).await;
    sqlx::query("INSERT INTO profiles (id,name) VALUES ('kid','Kid')")
        .execute(&f.app.db)
        .await
        .unwrap();
    let filter = orgs::save_filter(
        &f.app,
        "default",
        None,
        "Since 2000",
        vec![Term {
            field: Field::Year,
            operator: Operator::Gte,
            value: Value::Integer(2000),
        }],
    )
    .await
    .unwrap();
    let smart = orgs::save_collection(
        &f.app,
        "default",
        None,
        "Recent",
        CollectionKind::Smart,
        vec![],
        Some(filter.id.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        orgs::members(&f.app.db, "default", &smart.id)
            .await
            .unwrap(),
        ["new", "newer"]
    );
    assert!(matches!(
        orgs::members(&f.app.db, "kid", &smart.id).await,
        Err(orgs::OrgError::NotFound)
    ));
    // Another profile cannot reference this profile's filter.
    assert!(matches!(
        orgs::save_collection(
            &f.app,
            "kid",
            None,
            "X",
            CollectionKind::Smart,
            vec![],
            Some(filter.id.clone())
        )
        .await,
        Err(orgs::OrgError::InvalidReference(_))
    ));
    let manual = orgs::save_collection(
        &f.app,
        "default",
        None,
        "Picks",
        CollectionKind::Manual,
        vec!["newer".into(), "old".into()],
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        orgs::save_collection(
            &f.app,
            "default",
            Some((&manual.id, 0)),
            "Picks",
            CollectionKind::Manual,
            vec![],
            None
        )
        .await,
        Err(orgs::OrgError::Rejected(OrganizationError::StaleRevision))
    ));
    // Merging a member into another member resolves to the live work once.
    let before = playscale::curation::preview_merge(
        &f.app.db,
        &playscale_core::identity::MergeRequest {
            sources: vec!["newer".into()],
            target: "old".into(),
            expected: [("newer", 0), ("old", 0)]
                .into_iter()
                .map(|(k, _)| (k.to_string(), 0))
                .collect(),
        },
    )
    .await;
    assert!(before.is_err(), "revisions must be reviewed");
    let mut conn = f.app.db.acquire().await.unwrap();
    let revisions: std::collections::BTreeMap<String, u64> = [
        (
            "newer",
            playscale::curation::load_aggregate(&mut conn, "newer")
                .await
                .unwrap()
                .unwrap(),
        ),
        (
            "old",
            playscale::curation::load_aggregate(&mut conn, "old")
                .await
                .unwrap()
                .unwrap(),
        ),
    ]
    .into_iter()
    .map(|(id, a)| (id.to_string(), a.work.revision))
    .collect();
    drop(conn);
    let plan = playscale::curation::preview_merge(
        &f.app.db,
        &playscale_core::identity::MergeRequest {
            sources: vec!["newer".into()],
            target: "old".into(),
            expected: revisions,
        },
    )
    .await
    .unwrap();
    playscale::curation::commit_merge(&f.app, &plan)
        .await
        .unwrap();
    assert_eq!(
        orgs::members(&f.app.db, "default", &manual.id)
            .await
            .unwrap(),
        ["old"]
    );
}

#[tokio::test]
async fn playlists_and_queues_keep_entry_identity_and_revisions() {
    let f = Fixture::new().await;
    work(&f, "a", "A", 2000).await;
    work(&f, "b", "B", 2001).await;
    let entry = |id: &str, timeline: &str| Entry {
        entry_id: id.into(),
        timeline_id: timeline.into(),
    };
    // The same timeline may appear twice; entry IDs may not.
    let playlist = orgs::save_playlist(
        &f.app,
        "default",
        None,
        "Mix",
        vec![entry("1", "a-ed"), entry("2", "b-ed"), entry("3", "a-ed")],
    )
    .await
    .unwrap();
    assert_eq!(playlist.revision, 1);
    assert!(matches!(
        orgs::save_playlist(
            &f.app,
            "default",
            None,
            "Dup",
            vec![entry("1", "a-ed"), entry("1", "b-ed")]
        )
        .await,
        Err(orgs::OrgError::Rejected(
            OrganizationError::DuplicateMember(_)
        ))
    ));
    assert!(matches!(
        orgs::save_playlist(&f.app, "default", None, "Bad", vec![entry("1", "missing")]).await,
        Err(orgs::OrgError::InvalidReference(_))
    ));

    let (queue_id, queue) = orgs::create_queue(
        &f.app,
        "default",
        playlist.entries.clone(),
        Repeat::Off,
        Some(u64::MAX),
    )
    .await
    .unwrap();
    let first = orgs::change_queue(
        &f.app,
        "default",
        &queue_id,
        queue.revision,
        orgs::QueueChange::Step(Step::Next),
    )
    .await
    .unwrap();
    assert!(first.current.is_some());
    assert!(matches!(
        orgs::change_queue(
            &f.app,
            "default",
            &queue_id,
            queue.revision,
            orgs::QueueChange::Step(Step::Next)
        )
        .await,
        Err(orgs::OrgError::Rejected(OrganizationError::StaleRevision))
    ));
    let selected = orgs::change_queue(
        &f.app,
        "default",
        &queue_id,
        first.revision,
        orgs::QueueChange::Select("2".into()),
    )
    .await
    .unwrap();
    // Removing the current entry continues at its surviving follower.
    let order = playscale_core::organization::play_order(&selected);
    let at = order.iter().position(|e| e == "2").unwrap();
    let follower = order[at + 1..].first().cloned();
    let edited = orgs::change_queue(
        &f.app,
        "default",
        &queue_id,
        selected.revision,
        orgs::QueueChange::Edit {
            entries: vec![entry("1", "a-ed"), entry("3", "a-ed")],
            repeat: Repeat::All,
            shuffle_seed: Some(u64::MAX),
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.current, follower);
    let reloaded = orgs::get_queue(&f.app.db, "default", &queue_id)
        .await
        .unwrap();
    assert_eq!(reloaded, edited, "u64 seed and state round-trip");
    assert!(orgs::get_queue(&f.app.db, "kid", &queue_id).await.is_err());
}

// ---------------------------------------------------------------------------
// Review regressions

fn admin_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:8787")
        .header("authorization", "Bearer test-secret-token")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn merge(f: &Fixture, source: &str, target: &str) {
    let mut conn = f.app.db.acquire().await.unwrap();
    let mut expected = std::collections::BTreeMap::new();
    for id in [source, target] {
        let a = playscale::curation::load_aggregate(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        expected.insert(id.to_string(), a.work.revision);
    }
    drop(conn);
    let plan = playscale::curation::preview_merge(
        &f.app.db,
        &playscale_core::identity::MergeRequest {
            sources: vec![source.into()],
            target: target.into(),
            expected,
        },
    )
    .await
    .unwrap();
    playscale::curation::commit_merge(&f.app, &plan)
        .await
        .unwrap();
}

#[tokio::test]
async fn pinned_identity_cannot_be_withdrawn_and_moves_with_a_merge() {
    let f = Fixture::new().await;
    work(&f, "source", "Source", 2000).await;
    work(&f, "target", "Target", 2000).await;
    sqlx::query("INSERT INTO metadata_documents VALUES ('source','tmdb',1,'603',?,1)")
        .bind(json!({"values":{"title":"Imported Title"},"tags":[],"excluded_tags":[]}).to_string())
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE items SET match_state='manual' WHERE id='source'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let router = api::router(f.app.clone(), None);
    let withdraw = router
        .clone()
        .oneshot(admin_request(
            "PUT",
            "/api/v1/items/source/metadata/tmdb",
            json!({"expected_revision":1,"external_id":null,"values":{},"tags":[]}),
        ))
        .await
        .unwrap();
    assert_eq!(
        withdraw.status(),
        StatusCode::CONFLICT,
        "removal is refused"
    );

    merge(&f, "source", "target").await;
    let (state, title): (String, String) =
        sqlx::query_as("SELECT match_state,title FROM items WHERE id='target'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(state, "manual", "pin moves with the identity");
    assert_eq!(title, "Imported Title", "title projection recomputed");
    let replace = router
        .clone()
        .oneshot(admin_request(
            "PUT",
            "/api/v1/items/target/metadata/tmdb",
            json!({"expected_revision":1,"external_id":"604","values":{},"tags":[]}),
        ))
        .await
        .unwrap();
    assert_eq!(replace.status(), StatusCode::CONFLICT);
    // The retired ID takes no new contributions.
    let retired = router
        .clone()
        .oneshot(admin_request(
            "PUT",
            "/api/v1/items/source/metadata/local",
            json!({"expected_revision":1,"values":{"title":"Late"},"tags":[]}),
        ))
        .await
        .unwrap();
    assert_eq!(retired.status(), StatusCode::NOT_FOUND);

    // Retired IDs resolve to the live work for reads and edition creation.
    let created = router
        .clone()
        .oneshot(admin_request(
            "POST",
            "/api/v1/items/source/editions",
            json!({"label":"Extended"}),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let owner: String = sqlx::query_scalar("SELECT item_id FROM editions WHERE label='Extended'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(owner, "target");
}

#[tokio::test]
async fn reproposing_a_decided_file_keeps_the_decision() {
    let f = Fixture::new().await;
    f.write("Heat.1995.mkv", b"heat");
    f.scan().await;
    seed_target(&f, "heat", "Heat", 1995).await;
    let (file, ..) = f.file("Heat.1995.mkv").await;
    let proposal = matching::propose(&f.app, &file).await.unwrap();
    matching::decide(&f.app, &proposal.id, proposal.revision, Decision::Reject)
        .await
        .unwrap();
    let again = matching::propose(&f.app, &file).await.unwrap();
    assert_eq!(again.id, proposal.id);
    assert_eq!(again.status, Status::Rejected);
    assert!(matching::inbox(&f.app.db, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn proposal_history_ignores_wall_clock_order() {
    let f = Fixture::new().await;
    f.write("Ran.1985.mkv", b"v1");
    f.scan().await;
    seed_target(&f, "ran", "Ran", 1985).await;
    let (file, ..) = f.file("Ran.1985.mkv").await;
    let first = matching::propose(&f.app, &file).await.unwrap();
    matching::decide(&f.app, &first.id, first.revision, Decision::Reject)
        .await
        .unwrap();
    // The clock later moves backward relative to the decided proposal.
    sqlx::query("UPDATE match_proposals SET created_at=99999999999 WHERE id=?")
        .bind(&first.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    f.write("Ran.1985.mkv", b"v2");
    f.scan().await;
    let second = matching::propose(&f.app, &file).await.unwrap();
    assert_ne!(second.id, first.id);
    let again = matching::propose(&f.app, &file).await.unwrap();
    assert_eq!(
        again.id, second.id,
        "open proposal refreshed, not re-raised"
    );
}

// ---------------------------------------------------------------------------
// Timelines and versions

#[tokio::test]
async fn scanned_works_get_versions_copies_do_not_and_replacement_needs_confirmation() {
    let f = Fixture::new().await;
    f.write("film.mp4", b"original bytes");
    f.write("backup/film.mp4", b"original bytes");
    f.scan().await;
    let (file, item, ..) = f.file("film.mp4").await;
    let view = playscale::curation::read_structure(&f.app.db, &item)
        .await
        .unwrap()
        .unwrap();
    let versions: Vec<_> = view
        .editions
        .iter()
        .flat_map(|e| &e.timelines)
        .flat_map(|t| &t.versions)
        .collect();
    assert_eq!(versions.len(), 1, "a copy is an occurrence, not a version");
    let version = versions[0].clone();
    assert_eq!(
        version.availability,
        playscale_core::identity::Availability::Available
    );

    // Replace whichever file the version is bound to; the copy still holds the
    // reviewed bytes, so the version stays available.
    let bound = version.bindings[0].file_id.clone();
    let bound_path: String = sqlx::query_scalar("SELECT relative_path FROM media_files WHERE id=?")
        .bind(&bound)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    f.write(&bound_path, b"replaced bytes");
    f.scan().await;
    let state = |view: playscale::curation::StructureView| {
        view.editions[0].timelines[0].versions[0].availability
    };
    let view = playscale::curation::read_structure(&f.app.db, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state(view),
        playscale_core::identity::Availability::Available
    );
    // Remove the copy as well: now the only reviewed content is gone and the
    // bound file holds other bytes, so the version is stale.
    let other = if bound_path == "film.mp4" {
        "backup/film.mp4"
    } else {
        "film.mp4"
    };
    std::fs::remove_file(f.root.join(other)).unwrap();
    f.scan().await;
    let view = playscale::curation::read_structure(&f.app.db, &item)
        .await
        .unwrap()
        .unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM media_versions WHERE id=?")
        .bind(&version.id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(state(view), playscale_core::identity::Availability::Stale);
    let replaced: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id=?")
        .bind(&bound)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let reviewed = std::collections::BTreeMap::from([(1u32, replaced.clone())]);
    assert!(
        playscale::curation::confirm_replacement(
            &f.app,
            &version.id,
            revision as u64 + 1,
            &reviewed
        )
        .await
        .is_err(),
        "stale review rejected"
    );
    // Reviewed one replacement, but the file changed again before confirming.
    f.write(&bound_path, b"replaced again");
    f.scan().await;
    assert!(matches!(
        playscale::curation::confirm_replacement(&f.app, &version.id, revision as u64, &reviewed)
            .await,
        Err(playscale::curation::CurationError::Rejected(
            playscale_core::identity::IdentityError::ReviewedContentChanged(_)
        ))
    ));
    let current: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id=?")
        .bind(&bound)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let reviewed = std::collections::BTreeMap::from([(1u32, current)]);
    playscale::curation::confirm_replacement(&f.app, &version.id, revision as u64, &reviewed)
        .await
        .unwrap();
    let view = playscale::curation::read_structure(&f.app.db, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state(view.clone()),
        playscale_core::identity::Availability::Available
    );
    assert_eq!(
        view.editions[0].timelines[0].versions[0].equivalence,
        playscale_core::identity::Equivalence::Declared
    );
    let _ = file;
}

#[tokio::test]
async fn reassigning_a_file_moves_its_version_within_the_work() {
    let f = Fixture::new().await;
    f.write("cut.mp4", b"directors cut");
    f.scan().await;
    let (file, item, _, edition) = f.file("cut.mp4").await;
    let router = api::router(f.app.clone(), None);
    let created = router
        .clone()
        .oneshot(admin_request(
            "POST",
            &format!("/api/v1/items/{item}/editions"),
            json!({"label":"Director's Cut"}),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let target: String =
        sqlx::query_scalar("SELECT id FROM editions WHERE label='Director''s Cut'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let moved = router
        .oneshot(admin_request(
            "PUT",
            &format!("/api/v1/files/{file}/edition"),
            json!({"expected_edition_id":edition,"edition_id":target}),
        ))
        .await
        .unwrap();
    assert_eq!(moved.status(), StatusCode::OK);
    let view = playscale::curation::read_structure(&f.app.db, &item)
        .await
        .unwrap()
        .unwrap();
    let holder = view
        .editions
        .iter()
        .find(|e| e.timelines.iter().any(|t| !t.versions.is_empty()))
        .unwrap();
    assert_eq!(holder.id, target);
    let mismatched: i64 = sqlx::query_scalar("SELECT count(*) FROM version_edition_mismatch")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(mismatched, 0);
}

#[tokio::test]
async fn reassignment_keeps_reviewed_content_with_its_copy() {
    let f = Fixture::new().await;
    f.write("a.mp4", b"reviewed A");
    f.write("b.mp4", b"reviewed A");
    f.scan().await;
    let (first, item, _, edition) = f.file("a.mp4").await;
    let version: (String, String) = sqlx::query_as(
        "SELECT version_id,file_id FROM version_files b JOIN media_files f ON f.id=b.file_id WHERE f.edition_id=?",
    )
    .bind(&edition)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    // Replace the bound file's bytes (now B) while a copy keeps reviewed A.
    let bound_path: String = sqlx::query_scalar("SELECT relative_path FROM media_files WHERE id=?")
        .bind(&version.1)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    f.write(&bound_path, b"different B");
    f.scan().await;
    let mut tx = playscale::db::begin_write(&f.app.db).await.unwrap();
    playscale::curation::create_edition(&mut tx, "other", &item, "Other")
        .await
        .unwrap();
    playscale::curation::reassign_file(&mut tx, &version.1, "other")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // The original version stays in the old edition, re-bound to the A copy.
    let (timeline, file, pinned): (String, String, String) = sqlx::query_as(
        "SELECT v.timeline_id,b.file_id,b.file_revision FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE v.id=?",
    )
    .bind(&version.0)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(timeline, edition);
    assert_ne!(file, version.1);
    let copy_revision: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id=?")
        .bind(&file)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(copy_revision, pinned);
    let _ = first;
}

// ---------------------------------------------------------------------------
// Sidecar NFO

#[tokio::test]
async fn sidecar_nfo_contributes_and_is_withdrawn_only_by_its_origin() {
    let f = Fixture::new().await;
    f.write("heat.mkv", b"heat bytes");
    f.write(
        "heat.nfo",
        br#"<movie><title>Heat</title><year>1995</year><uniqueid type="tmdb" default="true">949</uniqueid><actor><name>Al Pacino</name></actor></movie>"#,
    );
    f.scan().await;
    let (_, item, ..) = f.file("heat.mkv").await;
    let (title, external): (String, Option<String>) = sqlx::query_as(
        "SELECT i.title,m.external_id FROM items i JOIN metadata_documents m ON m.item_id=i.id AND m.source='nfo' WHERE i.id=?",
    )
    .bind(&item)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(title, "Heat");
    assert_eq!(external.as_deref(), Some("tmdb:949"));
    playscale::maintenance::refresh_search(&f.app)
        .await
        .unwrap();
    let hits = playscale::search::search(&f.app.db, "pacino", None, 10, None)
        .await
        .unwrap();
    assert_eq!(hits.items[0].item_id, item);

    // A byte-identical copy without a sidecar does not withdraw it.
    f.write("backup/heat.mkv", b"heat bytes");
    f.scan().await;
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM metadata_documents WHERE item_id=? AND source='nfo'",
    )
    .bind(&item)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(kept, 1);
    // A hostile replacement is ignored, keeping the published contribution.
    f.write("heat.nfo", br#"<!DOCTYPE movie [<!ENTITY x SYSTEM "file:///etc/passwd">]><movie><title>&x;</title></movie>"#);
    f.scan().await;
    let title: String = sqlx::query_scalar("SELECT title FROM items WHERE id=?")
        .bind(&item)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(title, "Heat");
    // Removing the supplying sidecar withdraws the contribution.
    std::fs::remove_file(f.root.join("heat.nfo")).unwrap();
    f.scan().await;
    let (title, left): (String, i64) = sqlx::query_as(
        "SELECT title,(SELECT count(*) FROM metadata_documents WHERE item_id=items.id AND source='nfo') FROM items WHERE id=?",
    )
    .bind(&item)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(
        (title.as_str(), left),
        ("heat", 0),
        "back to the scanned title"
    );
}

#[tokio::test]
async fn sidecar_fifo_pins_and_merges_are_handled() {
    let f = Fixture::new().await;
    // A FIFO sidecar with no writer must not stall the scan.
    f.write("pipe.mkv", b"pipe bytes");
    let status = std::process::Command::new("mkfifo")
        .arg(f.root.join("pipe.nfo"))
        .status()
        .unwrap();
    assert!(status.success());
    let job = tokio::time::timeout(Duration::from_secs(20), f.scan())
        .await
        .unwrap();
    assert_eq!(job.phase, "completed");

    // A pinned (manual) NFO identity survives removal of its sidecar.
    f.write("pinned.mkv", b"pinned bytes");
    f.write(
        "pinned.nfo",
        br#"<movie><title>Pinned</title><uniqueid type="tmdb">1</uniqueid></movie>"#,
    );
    f.scan().await;
    let (_, pinned, ..) = f.file("pinned.mkv").await;
    sqlx::query("UPDATE items SET match_state='manual' WHERE id=?")
        .bind(&pinned)
        .execute(&f.app.db)
        .await
        .unwrap();
    std::fs::remove_file(f.root.join("pinned.nfo")).unwrap();
    f.scan().await;
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM metadata_documents WHERE item_id=? AND source='nfo'",
    )
    .bind(&pinned)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(kept, 1, "manual pin is not withdrawn by sidecar absence");

    // After a merge moves the contribution, removing the sidecar still withdraws it.
    f.write("source.mkv", b"source bytes");
    f.write("source.nfo", b"<movie><title>From Sidecar</title></movie>");
    f.write("target.mkv", b"target bytes");
    f.scan().await;
    let (_, source, ..) = f.file("source.mkv").await;
    let (_, target, ..) = f.file("target.mkv").await;
    let mut conn = f.app.db.acquire().await.unwrap();
    let mut expected = std::collections::BTreeMap::new();
    for id in [&source, &target] {
        let a = playscale::curation::load_aggregate(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        expected.insert(id.clone(), a.work.revision);
    }
    drop(conn);
    let plan = playscale::curation::preview_merge(
        &f.app.db,
        &playscale_core::identity::MergeRequest {
            sources: vec![source.clone()],
            target: target.clone(),
            expected,
        },
    )
    .await
    .unwrap();
    playscale::curation::commit_merge(&f.app, &plan)
        .await
        .unwrap();
    std::fs::remove_file(f.root.join("source.nfo")).unwrap();
    f.scan().await;
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM metadata_documents WHERE item_id=? AND source='nfo'",
    )
    .bind(&target)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn organization_reads_and_deletes_are_profile_scoped_and_revisioned() {
    let f = Fixture::new().await;
    work(&f, "a", "A", 2000).await;
    let filter = orgs::save_filter(
        &f.app,
        "default",
        None,
        "Year",
        vec![Term {
            field: Field::Year,
            operator: Operator::Gte,
            value: Value::Integer(1990),
        }],
    )
    .await
    .unwrap();
    let smart = orgs::save_collection(
        &f.app,
        "default",
        None,
        "Smart",
        CollectionKind::Smart,
        vec![],
        Some(filter.id.clone()),
    )
    .await
    .unwrap();
    let playlist = orgs::save_playlist(
        &f.app,
        "default",
        None,
        "P",
        vec![Entry {
            entry_id: "1".into(),
            timeline_id: "a-ed".into(),
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        orgs::list_filters(&f.app.db, "default")
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        orgs::list_collections(&f.app.db, "default").await.unwrap()[0],
        smart
    );
    assert_eq!(
        orgs::get_playlist(&f.app.db, "default", &playlist.id)
            .await
            .unwrap(),
        playlist
    );
    assert!(
        orgs::list_playlists(&f.app.db, "kid")
            .await
            .unwrap()
            .is_empty()
    );
    // A filter used by a smart collection cannot be deleted.
    assert!(matches!(
        orgs::delete_filter(&f.app, "default", &filter.id, filter.revision).await,
        Err(orgs::OrgError::InvalidReference(_))
    ));
    assert!(matches!(
        orgs::delete_collection(&f.app, "default", &smart.id, 0).await,
        Err(orgs::OrgError::Rejected(OrganizationError::StaleRevision))
    ));
    orgs::delete_collection(&f.app, "default", &smart.id, smart.revision)
        .await
        .unwrap();
    orgs::delete_filter(&f.app, "default", &filter.id, filter.revision)
        .await
        .unwrap();
    orgs::delete_playlist(&f.app, "default", &playlist.id, playlist.revision)
        .await
        .unwrap();
    assert!(
        orgs::list_collections(&f.app.db, "default")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn nfo_ownership_follows_contributions_through_merges_and_splits() {
    let f = Fixture::new().await;
    // Target has an API-supplied nfo contribution; source has a sidecar.
    f.write("source.mkv", b"source");
    f.write("source.nfo", b"<movie><title>Sidecar</title></movie>");
    f.write("target.mkv", b"target");
    f.scan().await;
    let (_, source, ..) = f.file("source.mkv").await;
    let (_, target, ..) = f.file("target.mkv").await;
    sqlx::query("INSERT INTO metadata_documents VALUES (?,'nfo',1,NULL,'{\"values\":{\"title\":\"API\"},\"tags\":[],\"excluded_tags\":[]}',1)")
        .bind(&target)
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut conn = f.app.db.acquire().await.unwrap();
    let mut expected = std::collections::BTreeMap::new();
    for id in [&source, &target] {
        let a = playscale::curation::load_aggregate(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        expected.insert(id.clone(), a.work.revision);
    }
    drop(conn);
    let plan = playscale::curation::preview_merge(
        &f.app.db,
        &playscale_core::identity::MergeRequest {
            sources: vec![source.clone()],
            target: target.clone(),
            expected,
        },
    )
    .await
    .unwrap();
    playscale::curation::commit_merge(&f.app, &plan)
        .await
        .unwrap();
    std::fs::remove_file(f.root.join("source.nfo")).unwrap();
    f.scan().await;
    let api: String = sqlx::query_scalar("SELECT json_extract(document_json,'$.values.title') FROM metadata_documents WHERE item_id=? AND source='nfo'")
        .bind(&target)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(api, "API", "the target's own contribution is untouched");
}
