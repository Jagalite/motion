//! Search projection: normalization, aliases after merge, library scope,
//! keyset paging, staleness and injection resistance.
use playscale::{App, curation, db, libraries, maintenance, search};
use playscale_core::{identity::MergeRequest, sources::LibraryKind};
use serde_json::json;
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
    (dir, app)
}

async fn work(app: &App, id: &str, title: &str, metadata: serde_json::Value) {
    for (sql, binds) in [
        (
            "INSERT INTO items (id,title,kind) VALUES (?,?,'video')",
            vec![id, title],
        ),
        ("INSERT INTO item_origins VALUES (?,?)", vec![id, title]),
        (
            "INSERT INTO item_structure (item_id,media_type) VALUES (?,'movie')",
            vec![id],
        ),
    ] {
        let mut q = sqlx::query(sql);
        for b in binds {
            q = q.bind(b);
        }
        q.execute(&app.db).await.unwrap();
    }
    sqlx::query("INSERT INTO metadata_documents VALUES (?,'local',1,NULL,?,1)")
        .bind(id)
        .bind(metadata.to_string())
        .execute(&app.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn search_finds_normalized_titles_people_tags_and_aliases() {
    let (_dir, app) = fixture().await;
    work(
        &app,
        "amelie",
        "Amélie",
        json!({"values":{"cast":[{"name":"Audrey Tautou"}]},"tags":["Favorite"],"excluded_tags":[]}),
    )
    .await;
    work(
        &app,
        "dup",
        "Le Fabuleux Destin d'Amélie Poulain",
        json!({"values":{},"tags":[],"excluded_tags":[]}),
    )
    .await;
    let before = search::search(&app.db, "tau", None, 10, None)
        .await
        .unwrap();
    assert!(before.stale, "unindexed works are reported");
    maintenance::refresh_search(&app).await.unwrap();
    let hits = |page: search::Page| {
        page.items
            .into_iter()
            .map(|h| h.item_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        hits(
            search::search(&app.db, "tau", None, 10, None)
                .await
                .unwrap()
        ),
        ["amelie"]
    );
    assert_eq!(
        hits(
            search::search(&app.db, "FAVORITE", None, 10, None)
                .await
                .unwrap()
        ),
        ["amelie"]
    );
    assert_eq!(
        hits(
            search::search(&app.db, "AMÉLIE  fabuleux", None, 10, None)
                .await
                .unwrap()
        ),
        ["dup"]
    );
    // Query syntax is inert.
    assert!(
        search::search(&app.db, "title:x OR NEAR(\"y\")", None, 10, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        search::search(&app.db, "!!!", None, 10, None)
            .await
            .unwrap()
            .total,
        0
    );

    // After a merge the retired title finds the surviving work, once.
    let mut conn = app.db.acquire().await.unwrap();
    let mut expected = std::collections::BTreeMap::new();
    for id in ["dup", "amelie"] {
        let a = curation::load_aggregate(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        expected.insert(id.to_string(), a.work.revision);
    }
    drop(conn);
    let plan = curation::preview_merge(
        &app.db,
        &MergeRequest {
            sources: vec!["dup".into()],
            target: "amelie".into(),
            expected,
        },
    )
    .await
    .unwrap();
    curation::commit_merge(&app, &plan).await.unwrap();
    let page = search::search(&app.db, "fabuleux", None, 10, None)
        .await
        .unwrap();
    assert!(!page.stale, "merge refreshed affected documents");
    assert_eq!(hits(page), ["amelie"]);
}

#[tokio::test]
async fn search_pages_by_keyset_and_respects_library_scope() {
    let (dir, app) = fixture().await;
    for (id, title) in [("a", "Star One"), ("b", "Star Two"), ("c", "Star Three")] {
        work(
            &app,
            id,
            title,
            json!({"values":{},"tags":[],"excluded_tags":[]}),
        )
        .await;
    }
    maintenance::refresh_search(&app).await.unwrap();
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = search::search(&app.db, "star", None, 1, cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(page.total, 3);
        seen.extend(page.items.into_iter().map(|h| h.item_id));
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    seen.sort();
    assert_eq!(seen, ["a", "b", "c"], "every match exactly once");
    assert!(matches!(
        search::search(&app.db, "star", None, 1, Some("zz")).await,
        Err(search::SearchError::InvalidCursor)
    ));
    assert!(matches!(
        search::search(&app.db, "star", None, 0, None).await,
        Err(search::SearchError::InvalidLimit)
    ));

    // Scope: only works with files in the given libraries' sources.
    let root = dir.path().join("media");
    std::fs::create_dir(&root).unwrap();
    let source = db::add_library(&app.db, "Media", &root).await.unwrap();
    sqlx::query("INSERT INTO editions (id,item_id,label) VALUES ('a-ed','a','Original')")
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json) VALUES ('fa','a-ed',?,'a.mkv','r','s',1,'[]')")
        .bind(&source.id)
        .execute(&app.db)
        .await
        .unwrap();
    let library = libraries::save_library(
        &app,
        None,
        "Lib",
        LibraryKind::Movies,
        "",
        std::slice::from_ref(&source.id),
    )
    .await
    .unwrap();
    let scoped = search::search(
        &app.db,
        "star",
        Some(std::slice::from_ref(&library.id)),
        10,
        None,
    )
    .await
    .unwrap();
    assert_eq!(scoped.total, 1, "counts are scoped too");
    assert_eq!(scoped.items[0].item_id, "a");
    let none = search::search(&app.db, "star", Some(&[]), 10, None)
        .await
        .unwrap();
    assert_eq!(none.total, 0);
}
