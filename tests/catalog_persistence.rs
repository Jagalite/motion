//! Real SQLite evidence for the catalog persistence boundary: verified upgrade
//! backups, failed-migration restore, newer-schema refusal, and merge/split SQL
//! application compared with the production core's reference application.
use playscale::{App, curation, db};
use playscale_core::identity::{
    self, Aggregate, IdentityError, MergeRequest, Resolved, SplitRequest,
};
use sqlx::SqlitePool;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio::sync::{Mutex, Semaphore};

fn app(dir: &Path, db: SqlitePool) -> App {
    App {
        health: Arc::new(playscale::operations::Health::new(false)),
        db,
        admin_token: Arc::new("test-secret-token".into()),
        origin: Arc::new("http://127.0.0.1:8787".into()),
        authority: Arc::new("127.0.0.1:8787".into()),
        ffprobe: Arc::new("ffprobe".into()),
        jobs: Arc::new(Mutex::new(())),
        streams: Arc::new(Semaphore::new(2)),
        event_streams: Arc::new(Semaphore::new(2)),
        storage: Arc::new(playscale::storage::Runtime::new(
            dir.join("state"),
            Default::default(),
        )),
        processing: Arc::new(playscale::processing::Runtime::new(
            dir.join("cache"),
            Default::default(),
        )),
        access: Arc::new(playscale::v2::Runtime::new(
            playscale_core::access::AccessMode::TrustedHousehold,
            playscale::v2::auth::random_key(),
        )),
    }
}

/// A migrator containing only the first `upto` production migrations, plus
/// optional extra SQL as the next version.
async fn migrator(dir: &Path, upto: i64, extra: Option<&str>) -> sqlx::migrate::Migrator {
    let target = dir.join(format!("migrations-{upto}-{}", extra.is_some()));
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if name.ends_with(".sql") && name[..4].parse::<i64>().unwrap() <= upto {
            std::fs::copy(&path, target.join(name)).unwrap();
        }
    }
    if let Some(sql) = extra {
        std::fs::write(target.join(format!("{:04}_extra.sql", upto + 1)), sql).unwrap();
    }
    sqlx::migrate::Migrator::new(target.as_path())
        .await
        .unwrap()
}

/// Populate a schema-9 (pre-identity) database the way the v1 server would.
async fn legacy_fixture(db: &SqlitePool) {
    for sql in [
        "INSERT INTO libraries (id,name,root,root_identity) VALUES ('lib','Movies','/media','1:2')",
        "INSERT INTO items (id,title,kind) VALUES ('film','Film','video'),('single','Single','video'),('empty','Empty','video')",
        "INSERT INTO item_origins VALUES ('film','Film'),('single','Single'),('empty','Empty')",
        "INSERT INTO editions (id,item_id,label) VALUES ('cut-a','film','Theatrical'),('cut-b','film','Extended'),('only','single','Original')",
        "INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json) VALUES
          ('fa','cut-a','lib','a.mkv','ra','s',1,'[]'),('fb','cut-b','lib','b.mkv','rb','s',1,'[]'),('fs','only','lib','s.mkv','rs','s',1,'[]')",
        "INSERT INTO progress VALUES ('default','film',10,1),('default','single',20,1),('default','empty',5,1)",
        "INSERT INTO metadata_documents VALUES ('film','tmdb',1,'603','{\"values\":{},\"tags\":[],\"excluded_tags\":[]}',1)",
    ] {
        sqlx::query(sql).execute(db).await.unwrap();
    }
}

async fn applied(path: &Path) -> Vec<i64> {
    let pool = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let versions = sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap();
    pool.close().await;
    versions
}

fn backups(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir.join("upgrade-backups"))
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn upgrade_backs_up_preserves_ids_and_attributes_progress_without_guessing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let old = db::connect_with(&path, &migrator(dir.path(), 9, None).await)
        .await
        .unwrap();
    legacy_fixture(&old).await;
    old.close().await;

    let pool = db::connect(&path).await.unwrap();
    let copies = backups(dir.path());
    assert_eq!(copies.len(), 1, "exactly one pre-upgrade backup");
    db::verify_backup(&copies[0], 9).await.unwrap();
    assert!(copies[0].to_str().unwrap().contains("pre-upgrade-9-to-"));

    // Identities and rows survive unchanged.
    let files: Vec<(String, String)> =
        sqlx::query_as("SELECT id,edition_id FROM media_files ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        files,
        [("fa", "cut-a"), ("fb", "cut-b"), ("fs", "only")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    let dangling = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(dangling.is_empty());
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");

    // Two playable cuts: preserved as ambiguous, never copied to both.
    let attribution: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT item_id,outcome,timeline_id,candidates_json FROM legacy_progress_attribution ORDER BY item_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        attribution,
        vec![
            ("empty".into(), "orphaned".into(), None, "[]".into()),
            (
                "film".into(),
                "ambiguous".into(),
                None,
                r#"[["cut-a",1],["cut-b",1]]"#.into()
            ),
            (
                "single".into(),
                "exact".into(),
                Some("only".into()),
                r#"[["only",1]]"#.into()
            ),
        ]
    );
    // 0015 backfill: one timeline per edition (same ID), one declared version
    // per (edition, revision), every binding inside its file's edition.
    let structure: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM timelines WHERE id=edition_id),(SELECT count(*) FROM editions),(SELECT count(*) FROM media_versions WHERE equivalence='declared'),(SELECT count(*) FROM version_edition_mismatch)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(structure, (3, 3, 3, 0));
    // 0017: each legacy root is one source and one mixed library, same ID.
    let paired: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM catalog_libraries c JOIN library_sources s ON s.library_id=c.id AND s.source_id=c.id WHERE c.id='lib' AND c.kind='mixed'),(SELECT count(*) FROM sources)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(paired, (1, 1));
    let progress: i64 = sqlx::query_scalar("SELECT count(*) FROM progress")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(progress, 3, "legacy rows retained");
    let receipts: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM catalog_receipts ORDER BY kind")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(receipts, ["migration:legacy_progress", "migration:schema"]);
    assert!(
        sqlx::query("DELETE FROM catalog_receipts")
            .execute(&pool)
            .await
            .is_err()
    );
    pool.close().await;

    // Reopening is a no-op: no second backup or attribution receipt.
    let pool = db::connect(&path).await.unwrap();
    assert_eq!(backups(dir.path()).len(), 1);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog_receipts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(receipts, 2);
}

#[tokio::test]
async fn failed_migration_keeps_old_schema_and_verified_backup_restores() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let old = db::connect_with(&path, &migrator(dir.path(), 9, None).await)
        .await
        .unwrap();
    legacy_fixture(&old).await;
    old.close().await;

    let broken = migrator(
        dir.path(),
        9,
        Some("CREATE TABLE half_applied (id TEXT); INSERT INTO no_such_table VALUES (1);"),
    )
    .await;
    let error = db::connect_with(&path, &broken).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("verified pre-upgrade backup"),
        "{error:#}"
    );
    assert_eq!(applied(&path).await, (1..=9).collect::<Vec<_>>());
    let check = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let partial: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name='half_applied'")
            .fetch_one(&check)
            .await
            .unwrap();
    assert_eq!(partial, 0, "failed migration rolled back");
    check.close().await;

    // Restore the verified backup into a fresh destination and upgrade it.
    let backup = backups(dir.path()).pop().unwrap();
    let restored = dir.path().join("restored").join("db.sqlite");
    std::fs::create_dir_all(restored.parent().unwrap()).unwrap();
    std::fs::copy(&backup, &restored).unwrap();
    let pool = db::connect(&restored).await.unwrap();
    let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM items ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(titles, ["Empty", "Film", "Single"]);
}

#[tokio::test]
async fn newer_schema_is_refused_before_any_backup_or_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let pool = db::connect(&path).await.unwrap();
    sqlx::query("INSERT INTO _sqlx_migrations (version,description,success,checksum,execution_time) VALUES (9999,'future',1,x'00',0)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let error = db::connect(&path).await.unwrap_err();
    assert!(format!("{error:#}").contains("unknown to this server"));
    assert!(backups(dir.path()).is_empty());
}

async fn snapshot(db: &SqlitePool, ids: &[&str]) -> BTreeMap<String, Aggregate> {
    let mut conn = db.acquire().await.unwrap();
    let mut out = BTreeMap::new();
    for id in ids {
        if let Some(a) = curation::load_aggregate(&mut conn, id).await.unwrap() {
            out.insert(id.to_string(), a);
        }
    }
    out
}

async fn catalog_fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    legacy_fixture(&pool).await;
    for sql in [
        // A duplicate work (same provider ID) holding a backup copy and an encode.
        "INSERT INTO items (id,title,kind) VALUES ('dup','Film (copy)','video')",
        "INSERT INTO item_origins VALUES ('dup','Film (copy)')",
        "INSERT INTO editions (id,item_id,label) VALUES ('dup-ed','dup','Original')",
        "INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json) VALUES
          ('fd1','dup-ed','lib','d1.mkv','rd','s',1,'[]'),('fd2','dup-ed','lib','backup/d1.mkv','rd','s',1,'[]'),('fd3','dup-ed','lib','d-720.mkv','re','s',1,'[]')",
        "INSERT INTO metadata_documents VALUES ('dup','imdb',1,'tt1','{\"values\":{},\"tags\":[],\"excluded_tags\":[]}',1)",
        "INSERT INTO renditions VALUES ('rend','dup','processing','job','fd3','re','fd1','rd','720p','{}',1)",
        "INSERT INTO item_structure (item_id,media_type) VALUES ('film','movie'),('dup','movie'),('single','movie')",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    backfill_structure(&pool).await;
    let app = app(dir.path(), pool);
    (dir, app)
}

/// Raw-SQL fixtures bypass the catalog writers; give them the same default
/// timelines and per-(edition, revision) versions migration 0015 creates.
async fn backfill_structure(db: &SqlitePool) {
    for sql in [
        "INSERT INTO timelines (id,edition_id) SELECT id,id FROM editions WHERE id NOT IN (SELECT id FROM timelines)",
        "INSERT INTO media_versions (id,timeline_id,origin,equivalence) SELECT min(f.id),f.edition_id,'original','declared' FROM media_files f WHERE NOT EXISTS (SELECT 1 FROM version_files b JOIN media_versions v ON v.id=b.version_id WHERE v.timeline_id=f.edition_id AND b.file_revision=f.revision) GROUP BY f.edition_id,f.revision",
        "INSERT INTO version_files (version_id,part,file_id,file_revision) SELECT v.id,1,f.id,f.revision FROM media_versions v JOIN media_files f ON f.id=v.id WHERE v.id NOT IN (SELECT version_id FROM version_files)",
    ] {
        sqlx::query(sql).execute(db).await.unwrap();
    }
}

/// Aggregates compare as structure, not storage order.
fn normalized(mut catalog: BTreeMap<String, Aggregate>) -> BTreeMap<String, Aggregate> {
    for aggregate in catalog.values_mut() {
        aggregate.editions.sort_by(|a, b| a.id.cmp(&b.id));
        for edition in &mut aggregate.editions {
            edition.timelines.sort_by(|a, b| a.id.cmp(&b.id));
            for timeline in &mut edition.timelines {
                timeline.versions.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
    }
    catalog
}

async fn assert_consistent(db: &SqlitePool) {
    let mismatched: i64 = sqlx::query_scalar("SELECT count(*) FROM version_edition_mismatch")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(mismatched, 0);
}

fn expected(snapshot: &BTreeMap<String, Aggregate>) -> BTreeMap<String, u64> {
    snapshot
        .iter()
        .map(|(id, a)| (id.clone(), a.work.revision))
        .collect()
}

#[tokio::test]
async fn merge_sql_matches_core_application_and_is_fenced() {
    let (_dir, app) = catalog_fixture().await;
    let before = snapshot(&app.db, &["film", "dup"]).await;
    // Copies of one revision are one version with two occurrences.
    assert_eq!(before["dup"].editions[0].timelines[0].versions.len(), 2);
    let request = MergeRequest {
        sources: vec!["dup".into()],
        target: "film".into(),
        expected: expected(&before),
    };
    let plan = curation::preview_merge(&app.db, &request).await.unwrap();
    let receipt = curation::commit_merge(&app, &plan).await.unwrap();
    assert_eq!(receipt.kind, "catalog:merge");

    let mut reference = before.clone();
    identity::apply_merge(&mut reference, &plan);
    let after = snapshot(&app.db, &["film", "dup"]).await;
    assert_eq!(
        normalized(after.clone()),
        normalized(reference),
        "SQL application equals core reference"
    );
    assert_consistent(&app.db).await;
    assert_eq!(
        after["film"].work.revision,
        before["film"].work.revision + 1
    );
    assert_eq!(
        curation::resolve(&app.db, "dup").await.unwrap(),
        Some(Resolved::Alias {
            from: "dup".into(),
            to: "film".into()
        })
    );
    let (rendition_item, imdb_item): (String, String) = sqlx::query_as(
        "SELECT (SELECT item_id FROM renditions WHERE id='rend'),(SELECT item_id FROM metadata_documents WHERE source='imdb')",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(
        (rendition_item.as_str(), imdb_item.as_str()),
        ("film", "film")
    );
    // Viewing history stays on the retired ID; nothing is copied or merged.
    let progress: Vec<String> = sqlx::query_scalar("SELECT item_id FROM progress ORDER BY 1")
        .fetch_all(&app.db)
        .await
        .unwrap();
    assert_eq!(progress, ["empty", "film", "single"]);

    // Replaying the committed plan is rejected without changes.
    let replay = curation::commit_merge(&app, &plan).await.unwrap_err();
    assert!(matches!(replay, curation::CurationError::Rejected(_)));
    assert_eq!(snapshot(&app.db, &["film", "dup"]).await, after);
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog_receipts WHERE kind='catalog:merge'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(receipts, 1);
}

#[tokio::test]
async fn stale_merge_preview_is_rejected_after_any_structural_write() {
    let (_dir, app) = catalog_fixture().await;
    let before = snapshot(&app.db, &["film", "dup"]).await;
    let plan = curation::preview_merge(
        &app.db,
        &MergeRequest {
            sources: vec!["dup".into()],
            target: "film".into(),
            expected: expected(&before),
        },
    )
    .await
    .unwrap();
    // An unrelated v1 writer path (file reassignment within the work) bumps the
    // structural revision through the schema triggers.
    sqlx::query("UPDATE media_files SET edition_id='cut-a' WHERE id='fb'")
        .execute(&app.db)
        .await
        .unwrap();
    let error = curation::commit_merge(&app, &plan).await.unwrap_err();
    assert!(matches!(
        error,
        curation::CurationError::Rejected(IdentityError::StaleRevision(ref id)) if id == "film"
    ));
    let aliases: i64 = sqlx::query_scalar("SELECT count(*) FROM item_aliases")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(aliases, 0, "no partial application");
}

#[tokio::test]
async fn merges_with_conflicting_identity_or_children_are_rejected() {
    let (_dir, app) = catalog_fixture().await;
    sqlx::query("INSERT INTO metadata_documents VALUES ('dup','tmdb',1,'604','{\"values\":{},\"tags\":[],\"excluded_tags\":[]}',1)")
        .execute(&app.db)
        .await
        .unwrap();
    let before = snapshot(&app.db, &["film", "dup"]).await;
    let request = MergeRequest {
        sources: vec!["dup".into()],
        target: "film".into(),
        expected: expected(&before),
    };
    assert!(matches!(
        curation::preview_merge(&app.db, &request).await,
        Err(curation::CurationError::Rejected(
            IdentityError::ExternalIdentityConflict(ref n)
        )) if n == "tmdb"
    ));
    sqlx::query(
        "INSERT INTO items (id,title,kind) VALUES ('show','Show','video'),('season','S1','video')",
    )
    .execute(&app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO item_structure (item_id,media_type,parent_id,number) VALUES ('show','series',NULL,NULL),('season','season','show',1)")
        .execute(&app.db)
        .await
        .unwrap();
    let request = MergeRequest {
        sources: vec!["show".into()],
        target: "film".into(),
        expected: expected(&snapshot(&app.db, &["film", "show"]).await),
    };
    assert!(matches!(
        curation::preview_merge(&app.db, &request).await,
        Err(curation::CurationError::Rejected(
            IdentityError::IncompatibleKinds(_)
        ))
    ));
}

#[tokio::test]
async fn split_sql_matches_core_application_and_replay_is_rejected() {
    let (_dir, app) = catalog_fixture().await;
    let before = snapshot(&app.db, &["dup"]).await;
    // Move the 720p encode (fd3) out; the copies of revision rd stay.
    let plan = curation::preview_split(
        &app.db,
        &SplitRequest {
            item: "dup".into(),
            versions: vec!["fd3".into()],
            new_title: "Mislabeled encode".into(),
            expected_revision: before["dup"].work.revision,
        },
    )
    .await
    .unwrap();
    curation::commit_split(&app, &plan).await.unwrap();
    let mut reference = before.clone();
    identity::apply_split(&mut reference, &plan);
    let after = snapshot(&app.db, &["dup", plan.new_item.as_str()]).await;
    assert_eq!(
        normalized(after.clone()),
        normalized(reference),
        "SQL application equals core reference"
    );
    assert_consistent(&app.db).await;
    let (moved, rendition_item): (String, String) = sqlx::query_as(
        "SELECT (SELECT e.item_id FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE f.id='fd3'),(SELECT item_id FROM renditions WHERE id='rend')",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(moved, plan.new_item);
    assert_eq!(rendition_item, plan.new_item, "rendition follows its file");
    // Originals are untouched: same files, paths and revisions.
    let files: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(files, 6);
    assert!(matches!(
        curation::commit_split(&app, &plan).await,
        Err(curation::CurationError::Rejected(
            IdentityError::StaleRevision(_)
        ))
    ));
}

#[tokio::test]
async fn concurrent_commits_over_one_target_apply_exactly_once() {
    let (_dir, app) = catalog_fixture().await;
    let before = snapshot(&app.db, &["film", "dup", "single"]).await;
    let mut plans = Vec::new();
    for source in ["dup", "single"] {
        let plan = curation::preview_merge(
            &app.db,
            &MergeRequest {
                sources: vec![source.into()],
                target: "film".into(),
                expected: [source, "film"]
                    .into_iter()
                    .map(|id| (id.to_string(), before[id].work.revision))
                    .collect(),
            },
        )
        .await
        .unwrap();
        plans.push(plan);
    }
    // Both previews must observe the same revision before either writer starts.
    // Otherwise the first task can finish while the second preview is reading.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(plans.len()));
    let tasks: Vec<_> = plans
        .into_iter()
        .map(|plan| {
            let app = app.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                curation::commit_merge(&app, &plan).await
            })
        })
        .collect();
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.unwrap());
    }
    assert_eq!(
        outcomes.iter().filter(|result| result.is_ok()).count(),
        1,
        "{outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(
                error,
                curation::CurationError::Rejected(IdentityError::StaleRevision(_))
            )),
        "losing writer must be fenced by revision: {outcomes:?}"
    );
    assert_consistent(&app.db).await;
}

#[tokio::test]
async fn whole_timeline_split_matches_core_application() {
    let (_dir, app) = catalog_fixture().await;
    // A second timeline (e.g. an alternate ordering) in film's Theatrical edition.
    for sql in [
        "INSERT INTO timelines (id,edition_id) VALUES ('cut-a-alt','cut-a')",
        "INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json) VALUES ('fx','cut-a','lib','alt.mkv','rx','s',1,'[]'),('fx-copy','cut-a','lib','copy/alt.mkv','rx','s',1,'[]')",
        "INSERT INTO media_versions (id,timeline_id,origin,equivalence) VALUES ('fx','cut-a-alt','original','declared')",
        "INSERT INTO version_files (version_id,part,file_id,file_revision) VALUES ('fx',1,'fx','rx')",
    ] {
        sqlx::query(sql).execute(&app.db).await.unwrap();
    }
    let before = snapshot(&app.db, &["film"]).await;
    let plan = curation::preview_split(
        &app.db,
        &SplitRequest {
            item: "film".into(),
            versions: vec!["fx".into()],
            new_title: "Alternate".into(),
            expected_revision: before["film"].work.revision,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        plan.moves.as_slice(),
        [identity::SplitMove::Timelines { timelines, .. }] if timelines == &["cut-a-alt".to_string()]
    ));
    curation::commit_split(&app, &plan).await.unwrap();
    let mut reference = before.clone();
    identity::apply_split(&mut reference, &plan);
    let after = snapshot(&app.db, &["film", plan.new_item.as_str()]).await;
    assert_eq!(normalized(after), normalized(reference));
    assert_consistent(&app.db).await;
    // The copy of the moved content follows it; the other cut's files stay.
    let editions: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,edition_id FROM media_files WHERE id IN ('fa','fx','fx-copy') ORDER BY id",
    )
    .fetch_all(&app.db)
    .await
    .unwrap();
    assert_eq!(editions[0].1, "cut-a");
    assert_ne!(editions[1].1, "cut-a");
    assert_eq!(editions[1].1, editions[2].1);
}

#[tokio::test]
async fn coverage_is_backfilled_for_demands_resolved_before_0022() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let old = db::connect_with(&path, &migrator(dir.path(), 21, None).await)
        .await
        .unwrap();
    for sql in [
        "INSERT INTO libraries (id,name,root,root_identity) VALUES ('lib','L','/m','1:2')",
        "INSERT INTO catalog_libraries (id,name,kind) VALUES ('lib','L','mixed')",
        "INSERT INTO jobs (id,library_id,phase,created_at,complete_directories,incomplete_directories,outcome) VALUES ('job','lib','completed',1,7,2,'partial')",
        "INSERT INTO scan_requests VALUES ('req','lib',0,1)",
        "INSERT INTO scan_demands (id,request_id,source_id,barrier,verify,status,job_id) VALUES ('d','req','lib',1,0,'partial','job')",
    ] {
        sqlx::query(sql).execute(&old).await.unwrap();
    }
    old.close().await;
    let pool = db::connect(&path).await.unwrap();
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT complete_directories,incomplete_directories FROM scan_demands WHERE id='d'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (7, 2));
}

#[tokio::test]
async fn restore_into_verifies_upgrades_and_refuses_existing_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let old = db::connect_with(&path, &migrator(dir.path(), 9, None).await)
        .await
        .unwrap();
    legacy_fixture(&old).await;
    old.close().await;
    // Use the schema-9 database itself as the backup to restore.
    let destination = dir.path().join("restored").join("motion.sqlite");
    let receipt = db::restore_into(&path, &destination).await.unwrap();
    assert_eq!(receipt.restored_from_version, 9);
    assert!(receipt.schema_version > 9);
    let pool = SqlitePool::connect(&format!("sqlite://{}", destination.display()))
        .await
        .unwrap();
    let (titles, receipts): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM items),(SELECT count(*) FROM catalog_receipts WHERE kind='migration:restore')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((titles, receipts), (3, 1));
    pool.close().await;
    // A second restore into the same destination is refused.
    assert!(db::restore_into(&path, &destination).await.is_err());
    // A corrupt backup is refused before anything is written.
    let corrupt = dir.path().join("corrupt.sqlite");
    std::fs::write(&corrupt, b"not a database").unwrap();
    assert!(
        db::restore_into(&corrupt, &dir.path().join("other.sqlite"))
            .await
            .is_err()
    );
    assert!(!dir.path().join("other.sqlite").exists());
}
