//! Genuine paired-source fixtures: inventory-only and whole-original typed media.
//! No public player/manifest or external-probe qualification is implied.
use super::{coordinator, history::Requester, journal};
use crate::offline_control_upgrade::UpgradeReference;
use catabolic_runtime::helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact};
use catabolic_store::catalog_commands::ReceiptPolicy;
use serde_json::json;
use std::{
    borrow::Cow, fs::File, os::unix::fs::MetadataExt, path::PathBuf, sync::atomic::AtomicBool,
    time::Duration,
};
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn requester() -> Requester {
    Requester {
        requester_id: playscale_core::access::Principal::operator().id.clone(),
        credential_hash: None,
        ingress_id: None,
        accepted_principal: playscale_core::access::Principal::operator(),
    }
}
fn public_requester(r: &Requester) -> super::facade::SourceRelocationRequester {
    super::facade::SourceRelocationRequester::from_current_host(
        r.requester_id.clone(),
        r.credential_hash.clone(),
        r.ingress_id.clone(),
        r.accepted_principal.clone(),
    )
    .unwrap()
}
#[tokio::test]
async fn genuine_scanned_pair_commits_two_relocations_and_recovers_lost_ack_without_roots() {
    scanned_pair_case(ScannedCase::Renew).await;
}
#[tokio::test]
async fn genuine_scanned_pair_refuses_hidden_disabled_association_without_effects() {
    scanned_pair_case(ScannedCase::DisabledAssociation).await;
}
#[tokio::test]
async fn genuine_scanned_pair_refuses_partial_and_tampered_candidate_without_effects() {
    scanned_pair_case(ScannedCase::ChangedCandidate).await;
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScannedCase {
    Renew,
    DisabledAssociation,
    ChangedCandidate,
}
// Fixed test cases only: no callback or caller-supplied authority constructor.
async fn scanned_pair_case(case: ScannedCase) {
    let helper = PathBuf::from(
        std::env::var_os("MOTION_CATALOG_OBSERVER_TEST_BINARY")
            .expect("actual packaged observation helper required"),
    );
    let artifact = TrustedClosedObservationArtifact::after_packaging_verification(
        &helper,
        &catabolic_core::sha256(std::fs::read(&helper).unwrap()),
        ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let motion = root.path().join("motion.sqlite3");
    let native = root.path().join("native.sqlite3");
    let old = root.path().join("old");
    let next = root.path().join("next");
    let last = root.path().join("last");
    for dir in [&old, &next, &last] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("actual.txt"), b"actual retained file bytes").unwrap();
    }
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        // Export retains a writer while its source audit reads through the pool.
        // One connection would time out that audit and omit its descriptor.
        .max_connections(2)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("../../migrations");
    migrations.migrations = Cow::Owned(
        migrations
            .iter()
            .filter(|m| m.version <= 93)
            .cloned()
            .collect(),
    );
    migrations.run(&pool).await.unwrap();
    let md = std::fs::metadata(&old).unwrap();
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('physical','Physical',?,?,0,1,1,1,'[]',0)").bind(old.to_str().unwrap()).bind(format!("{}:{}",md.dev(),md.ino())).execute(&pool).await.unwrap();
    crate::initialize(native.clone()).await.unwrap();
    let backend = crate::NativeCatalog::bind(
        pool.clone(),
        native.clone(),
        crate::inspect(native.clone()).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend.register_motion_source("physical").await.unwrap();
    backend
        .scan_source(
            "physical",
            artifact,
            Default::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let native_source = catabolic_core::sha256("physical".as_bytes());
    let trust_source = native_source.clone();
    backend
        .with_store(true, move |store| {
            catabolic_store::observation::execute(
                store,
                "default",
                "trust",
                &json!({"source":trust_source,"overrides":{"uuid":"skip"},"apply":true}),
            )?;
            if case == ScannedCase::DisabledAssociation {
                let file: String = store.db.query_row(
                    "SELECT id FROM files WHERE location=?",
                    [&trust_source],
                    |r| r.get(0),
                )?;
                catabolic_store::curation::item_put(
                    store,
                    &json!({"id":"hidden-item","kind":"recording","metadata":{}}),
                )?;
                let association = catabolic_store::curation::dispatch(
                    store,
                    "default",
                    "associate",
                    &json!({"file_id":file,"item_id":"hidden-item","role":"primary"}),
                )?;
                catabolic_store::curation::dispatch(
                    store,
                    "default",
                    "association-disable",
                    &json!({"id":association["id"]}),
                )?;
                let inactive: i64 = store.db.query_row(
                    "SELECT count(*) FROM item_files WHERE active=0 AND file_id=?",
                    [&file],
                    |r| r.get(0),
                )?;
                assert_eq!(inactive, 1);
            }
            let contexts: i64 = store.db.query_row(
                "SELECT count(*) FROM catalog_segmentation_contexts",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(contexts, 0, "genuine unbound registry setup");
            Ok(())
        })
        .await
        .unwrap();
    backend
        .freeze_motion_source("actual-source94", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("actual-source94")
        .await
        .unwrap();
    backend.stage_import("actual-source94").await.unwrap();
    backend
        .prepare_motion_cutover("actual-source94")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(Duration::from_secs(30))
        .await
        .unwrap();
    let prepared = crate::imported_sources::prepare_imported_sources(&pool)
        .await
        .unwrap();
    let mut server = crate::cutover::open_server(pool.clone(), native.clone(), "default".into())
        .await
        .unwrap();
    let host = server.admission.admitted_host().unwrap();
    crate::imported_sources::acknowledge(
        pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
        host,
        prepared,
    )
    .await
    .unwrap();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let descriptors = crate::imported_sources::imported_sources_in(&mut tx)
        .await
        .unwrap();
    assert_eq!(
        descriptors.len(),
        1,
        "actual import must qualify the source"
    );
    assert_eq!(descriptors[0].descriptor.id, "physical");
    let original =
        crate::source_lifecycle::initialize_with_host(&mut tx, host, "physical", crate::now())
            .await
            .unwrap();
    let original_mapping = crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
        .await
        .unwrap()
        .1;
    tx.commit().await.unwrap();
    drop(server);
    drop(backend);
    let binding: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    let reference = UpgradeReference::build().await.unwrap();
    let upgraded = reference
        .upgrade(
            File::open(&motion).unwrap(),
            &motion,
            &catabolic_core::sha256(std::fs::read(&motion).unwrap()),
            &catabolic_core::sha256(binding.as_bytes()),
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    let mut session = upgraded
        .prepare_native(
            &native,
            catabolic_store::migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
            "source94-transition",
        )
        .unwrap()
        .apply(None)
        .unwrap()
        .save_command("source94-bootstrap", &[])
        .unwrap()
        .execute_unacknowledged(&[], policy())
        .unwrap()
        .acknowledge()
        .unwrap()
        .into_source_session()
        .await
        .unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let requester = requester();
    coordinator::commit_new_intent(
        &mut session,
        pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
        "actual-relocation-1",
        &original,
        &requester,
        &next,
        &AtomicBool::new(false),
    )
    .await
    .unwrap();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let pending:String=sqlx::query_scalar("SELECT new_mapping_json FROM catabolic_source_relocation_admissions WHERE operation_id='actual-relocation-1'").fetch_one(&mut *tx).await.unwrap();
    let first_mapping: crate::sources::SourceMapping = serde_json::from_str(&pending).unwrap();
    assert!(
        crate::source_lifecycle::require_historical_mapping_in(&mut tx, &first_mapping)
            .await
            .is_err()
    );
    if case != ScannedCase::Renew {
        // Read-only observations are assertions, never admission or authority.
        let state = || {
            let db = rusqlite::Connection::open_with_flags(
                &native,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let generation: i64 = db
                .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let effects: i64 = db
                .query_row("SELECT count(*) FROM catalog_source_successions", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let binding: String = db.query_row("SELECT root FROM bindings WHERE profile='default' AND kind='source' AND owner=?", [&native_source], |r| r.get(0)).unwrap();
            (generation, effects, binding)
        };
        let before = state();
        assert_eq!(before.1, 0);
        if case == ScannedCase::DisabledAssociation {
            let db = rusqlite::Connection::open_with_flags(
                &native,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let hidden: i64 = db.query_row("SELECT count(*) FROM item_files a JOIN files f ON f.id=a.file_id WHERE f.location=? AND a.active=0", [&native_source], |r| r.get(0)).unwrap();
            let contexts: i64 = db
                .query_row(
                    "SELECT count(*) FROM catalog_segmentation_contexts",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                hidden, 1,
                "real disabled association survives closed transfer"
            );
            assert_eq!(contexts, 0);
        }
        if case == ScannedCase::ChangedCandidate {
            std::fs::remove_file(next.join("actual.txt")).unwrap();
        }
        let failure = coordinator::dispatch_in(
            &mut session,
            &mut tx,
            "actual-relocation-1",
            &requester,
            policy(),
            &AtomicBool::new(false),
        )
        .await
        .err()
        .expect("real dispatch must refuse");
        if case == ScannedCase::DisabledAssociation {
            assert!(
                format!("{failure:#}")
                    .contains("inventory-only logical/provenance table is not empty: item_files"),
                "{failure:#}"
            );
        }
        assert_eq!(state(), before);
        let intents: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_relocation_admissions WHERE operation_id='actual-relocation-1'").fetch_one(&mut *tx).await.unwrap();
        let acks: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_relocation_acks")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(intents, 1, "durable original Motion intent is retained");
        assert_eq!(acks, 0, "failed native dispatch cannot publish an ACK");
        let token = journal::load_in(&mut tx, "actual-relocation-1", &requester)
            .await
            .unwrap()
            .unwrap();
        assert!(session.receipt_in(token).await.unwrap().is_none());
        session.verify_pair_in(&mut tx).await.unwrap();
        tx.rollback().await.unwrap();
        if case == ScannedCase::ChangedCandidate {
            std::fs::write(next.join("actual.txt"), b"tampered candidate bytes").unwrap();
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            assert!(
                coordinator::dispatch_in(
                    &mut session,
                    &mut tx,
                    "actual-relocation-1",
                    &requester,
                    policy(),
                    &AtomicBool::new(false)
                )
                .await
                .is_err()
            );
            assert_eq!(state(), before);
            let token = journal::load_in(&mut tx, "actual-relocation-1", &requester)
                .await
                .unwrap()
                .unwrap();
            assert!(session.receipt_in(token).await.unwrap().is_none());
            session.verify_pair_in(&mut tx).await.unwrap();
            tx.rollback().await.unwrap();
        }
        pool.close().await;
        session.close().unwrap();
        return;
    }
    let receipt = coordinator::dispatch_in(
        &mut session,
        &mut tx,
        "actual-relocation-1",
        &requester,
        policy(),
        &AtomicBool::new(false),
    )
    .await
    .unwrap();
    super::facade::require_same_receipt_effect(&receipt, receipt.effect_description()).unwrap();
    let mut changed_effect = receipt.effect_description().clone();
    let mut changed_policy: serde_json::Value =
        serde_json::from_str(changed_effect["policy_json"].as_str().unwrap()).unwrap();
    changed_policy["settings"]["uuid"] =
        serde_json::json!(if changed_policy["settings"]["uuid"] == "check" {
            "skip"
        } else {
            "check"
        });
    changed_effect["policy_json"] = serde_json::json!(catabolic_core::encode(&changed_policy));
    assert!(
        super::facade::require_same_receipt_effect(&receipt, &changed_effect).is_err(),
        "a different valid policy must fail genuine receipt equality, not merely shape validation"
    );
    assert_eq!(receipt.effect_description()["file_count"], 1);
    assert_eq!(
        receipt.effect_description()["context_count"],
        0,
        "unassociated inventory-only cohort"
    );
    tx.rollback().await.unwrap();
    // Genuine ACK insertion followed by a swallowed endpoint refusal cannot COMMIT.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::raw_sql("CREATE TEMP TRIGGER deny_source94_endpoint BEFORE INSERT ON main.catabolic_source_lifecycle WHEN NEW.operation_id='actual-relocation-1' BEGIN SELECT RAISE(ABORT,'test endpoint refusal'); END;").execute(&mut *tx).await.unwrap();
    assert!(
        journal::acknowledge_in(&mut tx, "actual-relocation-1", &requester, &receipt)
            .await
            .is_err()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_relocation_acks")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert!(tx.commit().await.is_err());
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_relocation_acks")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_temp_schema WHERE name='deny_source94_endpoint'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(n, 0);
    tx.rollback().await.unwrap();
    std::fs::rename(&old, root.path().join("old-unavailable")).unwrap();
    std::fs::rename(&next, root.path().join("next-unavailable")).unwrap();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        journal::recorded_command_in(&mut tx, "actual-relocation-1", &original, &requester, &next)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        journal::recorded_command_in(&mut tx, "actual-relocation-1", &original, &requester, &old)
            .await
            .is_err()
    );
    let mut different_requester = requester.clone();
    different_requester.credential_hash = Some("different-current-credential".into());
    assert!(journal::recorded_command_in(&mut tx, "actual-relocation-1", &original, &different_requester, &next).await.is_err(), "same operation must not accept a different original requester link");
    let mut changed = original.clone();
    changed.epoch += 1;
    assert!(
        journal::recorded_command_in(&mut tx, "actual-relocation-1", &changed, &requester, &next)
            .await
            .is_err()
    );
    assert!(
        journal::recorded_command_in(&mut tx, "absent", &original, &requester, &next)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    let first = coordinator::commit_recorded_ack(
        &mut session,
        pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
        "actual-relocation-1",
        &requester,
        policy(),
        &AtomicBool::new(true),
    )
    .await
    .unwrap();
    assert_eq!(first.epoch, original.epoch + 1);
    assert_eq!(
        coordinator::commit_recorded_ack(
            &mut session,
            pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
            "actual-relocation-1",
            &requester,
            policy(),
            &AtomicBool::new(true)
        )
        .await
        .unwrap(),
        first
    );
    std::fs::rename(root.path().join("next-unavailable"), &next).unwrap();
    coordinator::commit_new_intent(
        &mut session,
        pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
        "actual-relocation-2",
        &first,
        &requester,
        &last,
        &AtomicBool::new(false),
    )
    .await
    .unwrap();
    let mut facade = super::facade::SourceRelocationSession::from_stage(session);
    let public = public_requester(&requester);
    assert!(
        facade
            .prepare_existing_receipt(
                pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
                "actual-relocation-2",
                &public
            )
            .await
            .is_err()
    );
    // This negative deliberately ends SQLite's transaction outside SQLx below.
    // SQLx cannot reconcile its cached depth after that raw ROLLBACK, so never
    // return this intentionally damaged connection to the pool for recovery.
    let mut sabotaged_connection = pool.acquire().await.unwrap();
    sabotaged_connection.close_on_drop();
    let sabotaged_writer =
        sqlx::Transaction::begin(sabotaged_connection, Some(Cow::Borrowed("BEGIN IMMEDIATE")))
            .await
            .unwrap();
    let mut phase = facade
        .dispatch_recorded(
            sabotaged_writer,
            "actual-relocation-2",
            &public,
            policy(),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
    sqlx::query("ROLLBACK")
        .execute(phase.writer_in())
        .await
        .unwrap();
    assert!(phase.commit_existing_receipt_ack(&public).await.is_err());
    let phase = facade
        .prepare_existing_receipt(
            pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
            "actual-relocation-2",
            &public,
        )
        .await
        .unwrap();
    let second = phase.commit_existing_receipt_ack(&public).await.unwrap();
    let mut session = facade.inner;
    assert_eq!(second.epoch, first.epoch + 1);
    assert_eq!(
        coordinator::commit_recorded_ack(
            &mut session,
            pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
            "actual-relocation-1",
            &requester,
            policy(),
            &AtomicBool::new(true)
        )
        .await
        .unwrap(),
        first
    );
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let (current, current_mapping) =
        crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
            .await
            .unwrap();
    assert_eq!(current, second);
    assert_eq!(current_mapping.motion_root, last.to_str().unwrap());
    for mapping in [&original_mapping, &first_mapping, &current_mapping] {
        crate::source_lifecycle::require_historical_mapping_in(&mut tx, mapping)
            .await
            .unwrap();
    }
    let mut forged = current_mapping.clone();
    forged.motion_root.push_str("-not-recorded");
    assert!(
        crate::source_lifecycle::require_historical_mapping_in(&mut tx, &forged)
            .await
            .is_err()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_relocation_acks")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 2);
    tx.rollback().await.unwrap();
    // Database-only withdrawal keeps native facts borrowed through actual COMMIT.
    std::fs::rename(&last, root.path().join("last-unavailable")).unwrap();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let proof = session
        .withdrawal_in(
            &mut tx,
            &current_mapping.profile,
            &current_mapping.native_source,
            "physical",
        )
        .await
        .unwrap();
    assert_eq!(proof.retained_files(), 1);
    assert_eq!(proof.logical_memberships(), 0);
    let rolled = crate::source_lifecycle::withdraw_completed_in(
        &mut tx,
        &proof,
        "physical",
        second.source_revision,
        "actual-withdrawal",
        crate::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        rolled.state,
        playscale_core::source_lifecycle::State::Withdrawn
    );
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    drop(proof);
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert_eq!(
        crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
            .await
            .unwrap()
            .0,
        second
    );
    let proof = session
        .withdrawal_in(
            &mut tx,
            &current_mapping.profile,
            &current_mapping.native_source,
            "physical",
        )
        .await
        .unwrap();
    let withdrawn = crate::source_lifecycle::withdraw_completed_in(
        &mut tx,
        &proof,
        "physical",
        second.source_revision,
        "actual-withdrawal",
        crate::now(),
    )
    .await
    .unwrap();
    assert_eq!(withdrawn, rolled);
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    drop(proof);
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    for mapping in [&original_mapping, &first_mapping, &current_mapping] {
        crate::source_lifecycle::require_historical_mapping_in(&mut tx, mapping)
            .await
            .unwrap();
    }
    let proof = session
        .withdrawal_in(
            &mut tx,
            &current_mapping.profile,
            &current_mapping.native_source,
            "physical",
        )
        .await
        .unwrap();
    assert_eq!(
        crate::source_lifecycle::withdraw_completed_in(
            &mut tx,
            &proof,
            "physical",
            second.source_revision,
            "actual-withdrawal",
            crate::now()
        )
        .await
        .unwrap(),
        withdrawn
    );
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    drop(proof);
    session.close().unwrap();
    pool.close().await;
}
#[test]
fn relocation_candidate_identity_is_lexical_and_does_not_require_existing_roots() {
    use std::path::Path;
    assert!(
        journal::candidate_selector(Path::new("/source94-not-required-to-exist/candidate")).is_ok()
    );
    for path in ["relative", "/a/../b", "/a/./b", "/a//b", "/a/"] {
        assert!(
            journal::candidate_selector(Path::new(path)).is_err(),
            "{path}"
        );
    }
}

// Separate bound-media cohort; the original inventory-only fixture stays intact.
#[tokio::test]
async fn genuine_bound_media_pair_renews_complete_context_twice_and_blocks_library_withdrawal() {
    let helper = PathBuf::from(
        std::env::var_os("MOTION_CATALOG_OBSERVER_TEST_BINARY")
            .expect("actual packaged observation helper required"),
    );
    let artifact = TrustedClosedObservationArtifact::after_packaging_verification(
        &helper,
        &catabolic_core::sha256(std::fs::read(&helper).unwrap()),
        ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let motion = root.path().join("motion.sqlite3");
    let native = root.path().join("native.sqlite3");
    let old = root.path().join("old");
    let next = root.path().join("next");
    let last = root.path().join("last");
    // One-second PCM WAVE is real media, not a fabricated probe result. No
    // external probe or playable component assertion is claimed by this cohort.
    let samples = vec![0u8; 16_000];
    let mut wave = Vec::new();
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(36u32 + samples.len() as u32).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16u32.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&8000u32.to_le_bytes());
    wave.extend_from_slice(&16000u32.to_le_bytes());
    wave.extend_from_slice(&2u16.to_le_bytes());
    wave.extend_from_slice(&16u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    wave.extend_from_slice(&samples);
    for dir in [&old, &next, &last] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("actual.wav"), &wave).unwrap();
    }
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(2)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("../../migrations");
    migrations.migrations = Cow::Owned(
        migrations
            .iter()
            .filter(|m| m.version <= 93)
            .cloned()
            .collect(),
    );
    migrations.run(&pool).await.unwrap();
    let md = std::fs::metadata(&old).unwrap();
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('physical','Physical',?,?,0,1,1,1,'[]',0)").bind(old.to_str().unwrap()).bind(format!("{}:{}",md.dev(),md.ino())).execute(&pool).await.unwrap();
    crate::initialize(native.clone()).await.unwrap();
    let backend = crate::NativeCatalog::bind(
        pool.clone(),
        native.clone(),
        crate::inspect(native.clone()).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend.register_motion_source("physical").await.unwrap();
    backend
        .scan_source(
            "physical",
            artifact,
            Default::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let native_source = catabolic_core::sha256("physical".as_bytes());
    let trust_source = native_source.clone();
    backend
        .with_store(true, move |store| {
            catabolic_store::observation::execute(
                store,
                "default",
                "trust",
                &json!({"source":trust_source,"overrides":{"uuid":"skip"},"apply":true}),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let bound_source = native_source.clone();
    backend
        .with_store(true, move |store| curate_bound_media(store, &bound_source))
        .await
        .unwrap();

    backend
        .freeze_motion_source("bound-source94", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("bound-source94")
        .await
        .unwrap();
    backend.stage_import("bound-source94").await.unwrap();
    backend
        .prepare_motion_cutover("bound-source94")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(Duration::from_secs(30))
        .await
        .unwrap();
    let prepared = crate::imported_sources::prepare_imported_sources(&pool)
        .await
        .unwrap();
    let mut server = crate::cutover::open_server(pool.clone(), native.clone(), "default".into())
        .await
        .unwrap();
    let host = server.admission.admitted_host().unwrap();
    crate::imported_sources::acknowledge(
        pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
        host,
        prepared,
    )
    .await
    .unwrap();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let descriptors = crate::imported_sources::imported_sources_in(&mut tx)
        .await
        .unwrap();
    assert_eq!(
        descriptors.len(),
        1,
        "actual bound-media import must qualify the source"
    );
    assert_eq!(descriptors[0].descriptor.id, "physical");
    let original =
        crate::source_lifecycle::initialize_with_host(&mut tx, host, "physical", crate::now())
            .await
            .unwrap();
    let original_mapping = crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
        .await
        .unwrap()
        .1;
    tx.commit().await.unwrap();
    drop(server);
    drop(backend);
    let binding: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    let reference = UpgradeReference::build().await.unwrap();
    let upgraded = reference
        .upgrade(
            File::open(&motion).unwrap(),
            &motion,
            &catabolic_core::sha256(std::fs::read(&motion).unwrap()),
            &catabolic_core::sha256(binding.as_bytes()),
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    let mut session = upgraded
        .prepare_native(
            &native,
            catabolic_store::migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
            "bound94-transition",
        )
        .unwrap()
        .apply(None)
        .unwrap()
        .save_command("bound94-bootstrap", &[])
        .unwrap()
        .execute_unacknowledged(&[], policy())
        .unwrap()
        .acknowledge()
        .unwrap()
        .into_source_session()
        .await
        .unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let requester = requester();
    // Each preparation captures the complete typed closure; the second one
    // must authenticate the first permanent succession, not reuse stale tuples.
    let mut current = original.clone();
    for (operation, target) in [("bound-relocation-1", &next), ("bound-relocation-2", &last)] {
        coordinator::commit_new_intent(
            &mut session,
            pool.begin_with("BEGIN IMMEDIATE").await.unwrap(),
            operation,
            &current,
            &requester,
            target,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let receipt = coordinator::dispatch_in(
            &mut session,
            &mut tx,
            operation,
            &requester,
            policy(),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(receipt.effect_description()["file_count"], 1);
        assert_eq!(
            receipt.effect_description()["context_count"],
            1,
            "actual whole-original typed context must survive each renewal"
        );
        // Lose Motion ACK deliberately after the real native effect. The
        // current-image owner must not seal an unmatched permanent effect.
        tx.rollback().await.unwrap();
        let tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let failure = match session
            .into_current_image(tx, &AtomicBool::new(false))
            .await
        {
            Ok(_) => panic!("current image accepted native effect without Motion ACK"),
            Err(failure) => failure,
        };
        assert!(
            failure
                .error()
                .to_string()
                .contains("complete94 admission/command/ACK counts"),
            "{}",
            failure.error()
        );
        let (returned, mut tx, _) = failure.into_parts();
        session = returned;
        let token = journal::load_in(&mut tx, operation, &requester)
            .await
            .unwrap()
            .unwrap();
        let recovered = session.receipt_in(token).await.unwrap().unwrap();
        assert_eq!(recovered.effect_description(), receipt.effect_description());
        current = journal::acknowledge_in(&mut tx, operation, &requester, &recovered)
            .await
            .unwrap();
        let (actual, mapping) = crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
            .await
            .unwrap();
        assert_eq!(actual, current);
        assert_eq!(mapping.motion_root, target.to_str().unwrap());
        crate::source_lifecycle::require_historical_mapping_in(&mut tx, &original_mapping)
            .await
            .unwrap();
        session.verify_pair_in(&mut tx).await.unwrap();
        tx.commit().await.unwrap();
        let tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let mut image = match session
            .into_current_image(tx, &AtomicBool::new(false))
            .await
        {
            Ok(image) => image,
            Err(failure) => panic!("acknowledged current-image capture: {}", failure.error()),
        };
        assert_eq!(
            image.effect_count(),
            if operation.ends_with('1') { 1 } else { 2 }
        );
        assert_eq!(image.native_inventory_sha256().len(), 64);
        image.verify_current(&AtomicBool::new(false)).await.unwrap();
        let (returned, tx) = image.discard_copy();
        session = returned;
        tx.rollback().await.unwrap();
    }
    // Complete native library membership remains authoritative without the
    // named source root. This is a real joined-pair withdrawal refusal, not a
    // public manifest/player read or ordinary41 startup qualification.
    std::fs::rename(&last, root.path().join("bound-root-unavailable")).unwrap();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let proof = session
        .withdrawal_in(&mut tx, "default", &native_source, "physical")
        .await
        .unwrap();
    assert_eq!(proof.retained_files(), 1);
    assert_eq!(proof.logical_memberships(), 1);
    assert!(!proof.native_ownership_unresolved());
    proof.revalidate().unwrap();
    let (_, authenticated_mapping) =
        crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
            .await
            .unwrap();
    assert_eq!(authenticated_mapping.catalog.schema_version, 40);
    crate::source_lifecycle::verify_completed_withdrawal(&proof, &authenticated_mapping).unwrap();
    // Same exact current identity is still accepted, but this does not alter
    // the immutable historical mapping or ordinary schema admission.
    let mut current_envelope = authenticated_mapping.clone();
    current_envelope.catalog.schema_version = 41;
    crate::source_lifecycle::verify_completed_withdrawal(&proof, &current_envelope).unwrap();
    assert!(
        current_envelope
            .catalog
            .admit(&authenticated_mapping.catalog)
            .is_err()
    );
    let mut forgeries = Vec::new();
    for schema in [0, 39, 42] {
        let mut changed = authenticated_mapping.clone();
        changed.catalog.schema_version = schema;
        forgeries.push(changed);
    }
    let mut changed = authenticated_mapping.clone();
    changed.catalog.catalog_id.push_str("-foreign");
    forgeries.push(changed);
    let mut changed = authenticated_mapping.clone();
    changed.catalog.incarnation.push_str("-foreign");
    forgeries.push(changed);
    let mut changed = authenticated_mapping.clone();
    changed.library_id.push_str("-foreign");
    forgeries.push(changed);
    let mut changed = authenticated_mapping.clone();
    changed.profile.push_str("-foreign");
    forgeries.push(changed);
    let mut changed = authenticated_mapping.clone();
    changed.native_source.push_str("-foreign");
    forgeries.push(changed);
    let mut changed = authenticated_mapping.clone();
    changed.native_binding = serde_json::Value::Null;
    forgeries.push(changed);
    let mut changed = authenticated_mapping.clone();
    changed.native_policy = serde_json::Value::Null;
    forgeries.push(changed);
    for forged in forgeries {
        let error =
            crate::source_lifecycle::verify_completed_withdrawal(&proof, &forged).unwrap_err();
        assert!(
            error.to_string().contains("withdrawal identity")
                || error
                    .to_string()
                    .contains("completed withdrawal source differs"),
            "{error:#}"
        );
    }
    let error = crate::source_lifecycle::withdraw_completed_in(
        &mut tx,
        &proof,
        "physical",
        current.source_revision,
        "bound-withdrawal",
        crate::now(),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source withdrawal refused: Busy"),
        "{error:#}"
    );
    assert_eq!(
        crate::source_lifecycle::current_mapping_in(&mut tx, "physical")
            .await
            .unwrap()
            .0,
        current
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_source_lifecycle WHERE operation_id='bound-withdrawal'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(count, 0);
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    drop(proof);
    session.close().unwrap();
    pool.close().await;
}

fn curate_bound_media(store: &catabolic_store::Store, bound_source: &str) -> anyhow::Result<()> {
    use catabolic_core::{
        catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
        catalog_identity::{
            ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract,
            VersionState,
        },
    };
    use catabolic_store::{
        catalog_commands::{self, CommandOutcome, TrustedCommandAuthority},
        catalog_identity_mutation::{
            ContextChange, IdentityAuthority, IdentityMutation, TimelineChange, TimelineState,
            VersionChange,
        },
        curation,
    };
    // This context belongs to the same genuine Motion host that later issues
    // the relocation command; a foreign context namespace must still refuse.
    const HOST: &str = crate::HOST_NAMESPACE;
    // Ordinary40 mutation owners mint the context before the real import
    // and offline transition. No private component rows or41 Store escape.
    for (id, kind) in [
        ("bound-work", "recording"),
        ("bound-edition", "custom:motion_edition"),
        ("bound-version-item", "custom:motion_version"),
    ] {
        curation::item_put(
            store,
            &json!({"id":id,"kind":kind,"metadata":{"title":"Bound original"}}),
        )?;
    }
    curation::item_put(
        store,
        &json!({"id":"bound-library","kind":"custom:motion_library","metadata":{"title":"Bound library","motion_library":{"source_ids":["physical"]}}}),
    )?;
    curation::relate(
        store,
        &json!({"source":"bound-work","target":"bound-edition","kind":"custom:motion_edition"}),
    )?;
    curation::relate(
        store,
        &json!({"source":"bound-edition","target":"bound-version-item","kind":"custom:motion_version"}),
    )?;
    let captured = catabolic_store::catalog_import_typed::capture_source_occurrences(
        store,
        "default",
        &bound_source,
    )?;
    let files = captured["occurrences"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let file = files[0]["id"].as_str().unwrap().to_owned();
    curation::dispatch(
        store,
        "default",
        "associate",
        &json!({"file_id":file,"item_id":"bound-version-item","role":"primary","part":1,"metadata":{}}),
    )?;
    let association: String = store.db.query_row(
        "SELECT id FROM item_files WHERE item_id='bound-version-item' AND file_id=? AND active=1",
        [&file],
        |r| r.get(0),
    )?;
    let captured = catabolic_store::catalog_import_typed::capture_source_occurrences(
        store,
        "default",
        &bound_source,
    )?;
    let revision = captured["occurrences"][0]["exact_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let coverage = Some(IntervalUs::new(0, 1_000_000).unwrap());
    let version = VersionContract {
        contract_version: 1,
        version_id: "bound-version".into(),
        association_item_id: "bound-version-item".into(),
        edition_item_id: "bound-edition".into(),
        timeline_id: "bound-timeline".into(),
        state: VersionState::Ready,
        declared_coverage: coverage,
        bindings: vec![VersionBinding {
            binding_id: "bound-binding".into(),
            association_id: association,
            file: ExactFileRevision {
                file_id: file,
                revision,
            },
            part: 1,
            source: SourceExtent::WholeFile {
                duration_us: Some(1_000_000),
            },
            timeline_interval: coverage,
        }],
    };
    let mutation = IdentityMutation {
        contract_version: 1,
        profile: "default".into(),
        context: ContextChange {
            id: "bound-context".into(),
            authority_namespace: HOST.into(),
            context_key: "bound-context".into(),
            expected_revision: None,
        },
        timelines: vec![TimelineChange {
            timeline_id: "bound-timeline".into(),
            edition_item_id: "bound-edition".into(),
            expected_revision: None,
            identity_state: TimelineState::Provisional,
            coverage,
        }],
        versions: vec![VersionChange {
            expected_revision: None,
            expected_timeline_revision: 1,
            expected_binding_revisions: [("bound-binding".into(), None)].into(),
            version,
            equivalence: None,
        }],
        evidence: vec![],
    };
    let grant = IdentityAuthority {
        authority_namespace: HOST.into(),
        context_keys: ["bound-context".into()].into(),
        allow_declared_evidence: false,
        allow_verified_evidence: false,
    };
    let generation: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, "bound-identity").unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation,
        &json!({"operation":"identity","mutation":mutation}).to_string(),
    )
    .unwrap();
    let outcome = catalog_commands::execute(
        store,
        &command,
        TrustedCommandAuthority {
            host_namespace: HOST,
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: Some(&grant),
        },
        policy(),
    )?;
    assert!(matches!(outcome, CommandOutcome::Committed { .. }));
    let namespace: String = store.db.query_row(
        "SELECT authority_namespace FROM catalog_segmentation_contexts WHERE id='bound-context'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(namespace, crate::HOST_NAMESPACE);
    Ok(())
}
