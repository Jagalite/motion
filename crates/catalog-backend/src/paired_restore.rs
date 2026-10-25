//! Historical paired-restore journal authentication. No ordinary admission,
//! filesystem installation, incarnation rotation or command dispatch lives here.
pub(crate) mod durable;
use anyhow::{Context, ensure};
use playscale_core::{
    catalog_backend::Identity,
    catalog_identity_lineage::{Transition, TransitionKind, TransitionStatus},
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    contract_version: u32,
    transition_id: String,
    predecessor: Option<String>,
    from: Identity,
    to: Identity,
    frozen_binding_sha256: String,
    pair_sha256: String,
    source_native_inventory_sha256: String,
    source_motion_inventory_sha256: String,
    source_motion_binding_sha256: String,
    staged_native_before_rotation_inventory_sha256: String,
    staged_motion_binding_sha256: String,
    install_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgment {
    contract_version: u32,
    transition_id: String,
    intent_sha256: String,
    native_origin_json: String,
    native_origin_sha256: String,
    native_staged_sha256: String,
    motion_before_ack_inventory_sha256: String,
    install_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOrigin {
    contract: u32,
    restore_id: String,
    catalog_id: String,
    schema_version: u32,
    from_incarnation: String,
    to_incarnation: String,
    parent_restore_id: Option<String>,
    pair_sha256: String,
    native_inventory_sha256: String,
    motion_binding_sha256: String,
}
fn hash(value: &str) -> String {
    catabolic_core::sha256(value.as_bytes())
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.contains('\0')
}
fn decode<T: serde::de::DeserializeOwned + Serialize>(text: &str) -> anyhow::Result<T> {
    let decoded: T = serde_json::from_str(text)?;
    ensure!(
        serde_json::to_string(&decoded)? == text,
        "paired restore journal is not canonical"
    );
    Ok(decoded)
}
fn schema(db: &Connection) -> anyhow::Result<Vec<(String, String, String)>> {
    let (count,bytes):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE tbl_name IN ('catabolic_binding','catabolic_paired_restore_intents','catabolic_paired_restore_acks')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        (0..=16).contains(&count) && (0..=65536).contains(&bytes),
        "paired restore journal schema exceeds bound"
    );
    Ok(db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name IN ('catabolic_binding','catabolic_paired_restore_intents','catabolic_paired_restore_acks') AND sql IS NOT NULL ORDER BY type,name")?.query_map([],|r| {
        let sql:String=r.get(2)?;
        Ok((r.get(0)?,r.get(1)?,sql))
    })?.collect::<rusqlite::Result<Vec<_>>>()?)
}
/// Only exact closed immutable journal rows become core transition inputs.
/// The shared resolver orders95/96; caller still matches actual observed native
/// identity and native ancestry before admitting ordinary work. In that comparison,
/// native parent_restore_id follows the previous restore, skipping intervening96;
/// it is not necessarily the mixed transition predecessor. The retained database
/// transaction spans every precharge and row read. Missing tables
/// are an explicit error here, not an inferred historical migration exception.
pub(crate) struct AuthenticatedJournal {
    transitions: Vec<Transition>,
    native_origins: std::collections::BTreeMap<String, String>,
}
impl AuthenticatedJournal {
    pub(crate) fn transitions(&self) -> &[Transition] {
        &self.transitions
    }
    pub(crate) fn native_origins(&self) -> &std::collections::BTreeMap<String, String> {
        &self.native_origins
    }
}
/// `frozen_binding_identity` is the immutable decoded catabolic_binding value,
/// which may still be schema39. It is NOT the acknowledged89 effective identity
/// used as the core lineage resolver root. Callers authenticate that distinct
/// root through native_upgrade before resolving these95/96 transitions; they
/// must never rewrite the frozen binding or its recorded digest.
pub(crate) fn authenticated_transitions(
    db: &Connection,
    frozen_binding_identity: &Identity,
) -> anyhow::Result<AuthenticatedJournal> {
    ensure!(
        !db.is_autocommit(),
        "paired restore journal requires retained stable transaction"
    );
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(include_str!(
        "../../../migrations/0066_catabolic_command_journal.sql"
    ))?;
    expected.execute_batch(include_str!("paired_restore/schema95.sql"))?;
    ensure!(
        schema(db)? == schema(&expected)?,
        "paired restore journal DDL differs"
    );
    let frozen_size: i64 = db.query_row(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        (1..=65536).contains(&frozen_size),
        "frozen restore binding exceeds bound"
    );
    let frozen: String = db.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        frozen.len() <= 65536,
        "frozen restore binding exceeds bound"
    );
    ensure!(
        serde_json::from_str::<Identity>(&frozen)? == *frozen_binding_identity,
        "paired restore original binding mismatch"
    );
    let frozen_hash = hash(&frozen);
    for (table, column) in [
        ("catabolic_paired_restore_intents", "intent_json"),
        ("catabolic_paired_restore_acks", "proof_json"),
    ] {
        let (count, bytes): (i64, i64) = db.query_row(
            &format!(
                "SELECT count(*),coalesce(sum(length(CAST({column} AS BLOB))),0) FROM {table}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            (0..=32).contains(&count) && (0..=4 * 1024 * 1024).contains(&bytes),
            "paired restore complete journal exceeds bound"
        );
    }
    let orphan:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_paired_restore_acks a LEFT JOIN catabolic_paired_restore_intents i USING(transition_id) WHERE i.transition_id IS NULL)",[],|r|r.get(0))?;
    ensure!(!orphan, "paired restore ACK has no intent");
    let mut statement=db.prepare("SELECT transition_id,intent_json FROM catabolic_paired_restore_intents ORDER BY transition_id")?;
    let mut rows = statement.query([])?;
    let mut result = Vec::new();
    let mut native_origins = std::collections::BTreeMap::new();
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let raw: String = row.get(1)?;
        let intent: Intent = decode(&raw)?;
        intent
            .from
            .validate()
            .map_err(|e| anyhow::anyhow!("restore source identity: {e:?}"))?;
        intent
            .to
            .validate()
            .map_err(|e| anyhow::anyhow!("restore target identity: {e:?}"))?;
        ensure!(
            intent.contract_version == 1
                && intent.transition_id == key
                && id(&key)
                && id(&intent.install_id)
                && intent.predecessor.as_deref().is_none_or(id)
                && intent.from.catalog_id == frozen_binding_identity.catalog_id
                && intent.to.catalog_id == frozen_binding_identity.catalog_id
                && intent.from.schema_version == intent.to.schema_version
                && matches!(intent.from.schema_version, 40 | 41)
                && intent.from.incarnation != intent.to.incarnation
                && intent.frozen_binding_sha256 == frozen_hash
                && [
                    &intent.pair_sha256,
                    &intent.source_native_inventory_sha256,
                    &intent.source_motion_inventory_sha256,
                    &intent.source_motion_binding_sha256,
                    &intent.staged_native_before_rotation_inventory_sha256,
                    &intent.staged_motion_binding_sha256,
                ]
                .into_iter()
                .all(|v| digest(v)),
            "invalid retained paired restore intent"
        );
        let ack: Option<String> = db
            .query_row(
                "SELECT proof_json FROM catabolic_paired_restore_acks WHERE transition_id=?",
                [&key],
                |r| r.get(0),
            )
            .optional()?;
        let status = if let Some(raw_ack) = ack {
            let ack: Acknowledgment = decode(&raw_ack)?;
            ensure!(
                ack.contract_version == 1
                    && ack.transition_id == key
                    && ack.install_id == intent.install_id
                    && ack.intent_sha256 == hash(&raw)
                    && ack.native_origin_sha256 == hash(&ack.native_origin_json)
                    && [
                        &ack.native_staged_sha256,
                        &ack.motion_before_ack_inventory_sha256
                    ]
                    .into_iter()
                    .all(|v| digest(v)),
                "paired restore ACK commitment mismatch"
            );
            let origin: NativeOrigin =
                decode(&ack.native_origin_json).context("paired restore exact native origin")?;
            ensure!(
                origin.contract == 1
                    && origin.restore_id == key
                    && origin.catalog_id == intent.from.catalog_id
                    && origin.schema_version == intent.from.schema_version
                    && origin.from_incarnation == intent.from.incarnation
                    && origin.to_incarnation == intent.to.incarnation
                    && origin.pair_sha256 == intent.pair_sha256
                    && origin.native_inventory_sha256
                        == intent.staged_native_before_rotation_inventory_sha256
                    && origin.motion_binding_sha256 == intent.staged_motion_binding_sha256
                    && origin.parent_restore_id.as_deref().is_none_or(id),
                "paired restore ACK native origin differs"
            );
            native_origins.insert(key.clone(), ack.native_origin_json);
            TransitionStatus::Acknowledged
        } else {
            TransitionStatus::Pending
        };
        result.push(Transition {
            id: key,
            predecessor: intent.predecessor,
            from: intent.from,
            to: intent.to,
            kind: TransitionKind::Restore,
            status,
        });
    }
    Ok(AuthenticatedJournal {
        transitions: result,
        native_origins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Connection, Identity, Intent) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!(
            "../../../migrations/0066_catabolic_command_journal.sql"
        ))
        .unwrap();
        db.execute_batch(include_str!("paired_restore/schema95.sql"))
            .unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let original = Identity {
            catalog_id: "catalog".into(),
            incarnation: "first".into(),
            schema_version: 40,
        };
        let frozen = serde_json::to_string(&original).unwrap();
        db.execute("INSERT INTO catabolic_binding VALUES(1,?)", [&frozen])
            .unwrap();
        let intent = Intent {
            contract_version: 1,
            transition_id: "restore1".into(),
            predecessor: None,
            from: original.clone(),
            to: Identity {
                incarnation: "second".into(),
                ..original.clone()
            },
            frozen_binding_sha256: hash(&frozen),
            pair_sha256: "a".repeat(64),
            source_native_inventory_sha256: "b".repeat(64),
            source_motion_inventory_sha256: "c".repeat(64),
            source_motion_binding_sha256: "d".repeat(64),
            staged_native_before_rotation_inventory_sha256: "b".repeat(64),
            staged_motion_binding_sha256: "d".repeat(64),
            install_id: "install1".into(),
        };
        (db, original, intent)
    }
    // A fixture-only exact-row grant, not the future restore effect owner.
    fn insert(db: &Connection, kind: &str, key: &str, text: String) {
        let (k, i, p) = (kind.to_owned(), key.to_owned(), text.clone());
        db.create_scalar_function(
            "motion_paired_restore_insert",
            3,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                Ok(i64::from(
                    ctx.get::<String>(0)? == k
                        && ctx.get::<String>(1)? == i
                        && ctx.get::<String>(2)? == p,
                ))
            },
        )
        .unwrap();
        let table = if kind == "intent" {
            "catabolic_paired_restore_intents"
        } else {
            "catabolic_paired_restore_acks"
        };
        db.execute(
            &format!("INSERT INTO {table} VALUES(?,?)"),
            rusqlite::params![key, text],
        )
        .unwrap();
        db.remove_function("motion_paired_restore_insert", 3)
            .unwrap();
    }
    fn ack(intent: &Intent) -> Acknowledgment {
        let origin = NativeOrigin {
            contract: 1,
            restore_id: intent.transition_id.clone(),
            catalog_id: intent.from.catalog_id.clone(),
            schema_version: intent.from.schema_version,
            from_incarnation: intent.from.incarnation.clone(),
            to_incarnation: intent.to.incarnation.clone(),
            parent_restore_id: None,
            pair_sha256: intent.pair_sha256.clone(),
            native_inventory_sha256: intent
                .staged_native_before_rotation_inventory_sha256
                .clone(),
            motion_binding_sha256: intent.staged_motion_binding_sha256.clone(),
        };
        let raw = serde_json::to_string(&origin).unwrap();
        Acknowledgment {
            contract_version: 1,
            transition_id: intent.transition_id.clone(),
            intent_sha256: hash(&serde_json::to_string(intent).unwrap()),
            native_origin_sha256: hash(&raw),
            native_origin_json: raw,
            native_staged_sha256: "e".repeat(64),
            motion_before_ack_inventory_sha256: "f".repeat(64),
            install_id: intent.install_id.clone(),
        }
    }
    #[test]
    fn journal_retains_pending_and_authenticates_exact_native_ack() {
        let (db, original, intent) = setup();
        insert(
            &db,
            "intent",
            &intent.transition_id,
            serde_json::to_string(&intent).unwrap(),
        );
        let pending = authenticated_transitions(&db, &original).unwrap();
        assert_eq!(pending.transitions()[0].status, TransitionStatus::Pending);
        assert!(pending.native_origins().is_empty());
        insert(
            &db,
            "ack",
            &intent.transition_id,
            serde_json::to_string(&ack(&intent)).unwrap(),
        );
        let current = authenticated_transitions(&db, &original).unwrap();
        assert_eq!(
            current.transitions()[0].status,
            TransitionStatus::Acknowledged
        );
        assert_eq!(current.native_origins().len(), 1);
        assert!(matches!(
            playscale_core::catalog_identity_lineage::resolve(&original, current.transitions())
                .unwrap(),
            playscale_core::catalog_identity_lineage::Resolution::Current { .. }
        ));
        assert!(
            db.execute("DELETE FROM catabolic_paired_restore_acks", [])
                .is_err()
        );
    }
    #[test]
    fn foreign_native_origin_changed_commitment_and_guard_tampering_refuse() {
        for mode in 0..3 {
            let (db, original, intent) = setup();
            insert(
                &db,
                "intent",
                &intent.transition_id,
                serde_json::to_string(&intent).unwrap(),
            );
            let mut proof = ack(&intent);
            if mode == 0 {
                let mut origin: NativeOrigin =
                    serde_json::from_str(&proof.native_origin_json).unwrap();
                origin.to_incarnation = "foreign".into();
                proof.native_origin_json = serde_json::to_string(&origin).unwrap();
                proof.native_origin_sha256 = hash(&proof.native_origin_json);
            } else if mode == 1 {
                proof.intent_sha256 = "0".repeat(64);
            }
            insert(
                &db,
                "ack",
                &intent.transition_id,
                serde_json::to_string(&proof).unwrap(),
            );
            if mode == 2 {
                db.execute_batch("DROP TRIGGER catabolic_paired_restore_acks_insert")
                    .unwrap();
            }
            assert!(authenticated_transitions(&db, &original).is_err());
        }
    }
}
