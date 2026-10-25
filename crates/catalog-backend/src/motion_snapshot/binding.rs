//! Effective identity is validated by the existing upgrade owner while SQLx owns
//! the writer. Copy validation only compares exact retained evidence; it never
//! reenters SQLx while the raw handle lease is live or rewrites frozen binding.
use anyhow::{Context, ensure};
use playscale_core::catalog_backend::Identity;
use rusqlite::{Connection, OptionalExtension};
#[derive(PartialEq)]
pub(super) struct Pending {
    frozen: String,
    effective: Identity,
}
/// Only the genuine current96 owner may select the current41 copy identity.
/// Ordinary snapshot validation remains on its existing89 path.
pub(super) fn current_source_pair(
    db: &Connection,
    proof: &crate::native_bootstrap::SourcePairProof,
) -> anyhow::Result<Pending> {
    let commitments = proof.seal_commitments_in(db)?;
    let before = evidence(db)?;
    let frozen = before
        .frozen
        .context("current pair frozen binding missing")?;
    ensure!(
        catabolic_core::sha256(frozen.as_bytes()) == commitments.frozen_binding_sha256,
        "current pair frozen binding differs"
    );
    Ok(Pending {
        frozen,
        effective: commitments.identity,
    })
}
pub struct VerifiedBinding {
    effective: Identity,
    evidence_digest: String,
}
impl VerifiedBinding {
    pub fn effective_identity(&self) -> &Identity {
        &self.effective
    }
    pub fn evidence_sha256(&self) -> &str {
        &self.evidence_digest
    }
}
pub(super) async fn validate(db: &mut sqlx::SqliteConnection) -> anyhow::Result<Option<Pending>> {
    let size: Option<i64> = sqlx::query_scalar(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
    )
    .fetch_optional(&mut *db)
    .await?;
    let Some(size) = size else {
        return Ok(None);
    };
    ensure!(
        (1..=65536).contains(&size),
        "snapshot binding exceeds bounded identity cohort"
    );
    let (schema_count,schema_bytes):(i64,i64)=sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE tbl_name IN ('catabolic_binding','catabolic_native_upgrade_intent','catabolic_native_upgrade_ack')").fetch_one(&mut *db).await?;
    ensure!(
        schema_count <= 100 && (0..=1024 * 1024).contains(&schema_bytes),
        "snapshot binding schema bound exceeded"
    );
    let has_upgrade: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='catabolic_native_upgrade_ack')").fetch_one(&mut *db).await?;
    if has_upgrade {
        for (table, column) in [
            ("catabolic_native_upgrade_intent", "intent_json"),
            ("catabolic_native_upgrade_ack", "proof_json"),
        ] {
            let (count, bytes): (i64, i64) = sqlx::query_as(&format!(
                "SELECT count(*),coalesce(sum(length(CAST({column} AS BLOB))),0) FROM {table}"
            ))
            .fetch_one(&mut *db)
            .await?;
            ensure!(
                count <= 1 && (0..=2 * 1024 * 1024).contains(&bytes),
                "snapshot upgrade evidence bound exceeded"
            );
        }
    }
    let frozen: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&mut *db)
            .await?;
    let original: Identity = serde_json::from_str(&frozen)?;
    original
        .validate()
        .map_err(|e| anyhow::anyhow!("snapshot binding identity: {e:?}"))?;
    let effective = crate::native_upgrade::effective_identity_in(db, &original).await?;
    Ok(Some(Pending { frozen, effective }))
}
#[derive(serde::Serialize, PartialEq, Eq)]
struct Evidence {
    schema: Vec<(String, String, Option<String>)>,
    frozen: Option<String>,
    intent: Option<String>,
    acknowledgment: Option<String>,
}
fn evidence(db: &Connection) -> anyhow::Result<Evidence> {
    let (count,bytes):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE tbl_name IN ('catabolic_binding','catabolic_native_upgrade_intent','catabolic_native_upgrade_ack')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        count <= 100 && (0..=1024 * 1024).contains(&bytes),
        "snapshot binding schema bound exceeded"
    );
    let mut stmt=db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name IN ('catabolic_binding','catabolic_native_upgrade_intent','catabolic_native_upgrade_ack') ORDER BY type,name")?;
    let schema = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, Option<String>)>>>()?;
    let read = |table: &str, column: &str| -> anyhow::Result<Option<String>> {
        if !schema
            .iter()
            .any(|(kind, name, _)| kind == "table" && name == table)
        {
            return Ok(None);
        }
        let (count, bytes): (i64, i64) = db.query_row(
            &format!(
                "SELECT count(*),coalesce(sum(length(CAST({column} AS BLOB))),0) FROM {table}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            count <= 1 && (0..=2 * 1024 * 1024).contains(&bytes),
            "snapshot binding row bound exceeded"
        );
        Ok(db
            .query_row(&format!("SELECT {column} FROM {table}"), [], |r| r.get(0))
            .optional()?)
    };
    let frozen = read("catabolic_binding", "identity_json")?;
    let intent = read("catabolic_native_upgrade_intent", "intent_json")?;
    let acknowledgment = read("catabolic_native_upgrade_ack", "proof_json")?;
    Ok(Evidence {
        schema,
        frozen,
        intent,
        acknowledgment,
    })
}
pub(super) fn verify_copy(
    source: &Connection,
    copy: &Connection,
    pending: Option<&Pending>,
) -> anyhow::Result<Option<VerifiedBinding>> {
    let before = evidence(source)?;
    ensure!(
        before == evidence(copy)?,
        "copied binding/upgrade evidence differs"
    );
    match pending {
        None => {
            ensure!(
                before.frozen.is_none(),
                "unvalidated binding appeared during snapshot"
            );
            Ok(None)
        }
        Some(pending) => {
            ensure!(
                before.frozen.as_deref() == Some(pending.frozen.as_str()),
                "validated frozen binding changed"
            );
            let encoded = serde_json::to_vec(&before).context("encode exact binding evidence")?;
            Ok(Some(VerifiedBinding {
                effective: pending.effective.clone(),
                evidence_digest: catabolic_core::sha256(&encoded),
            }))
        }
    }
}

/// Independent copied evidence inspection. Same exact upgrade validators as
/// startup, without borrowing a live source or inventing a validated Pending.
pub(super) fn inspect(db: &Connection) -> anyhow::Result<VerifiedBinding> {
    let before = evidence(db)?;
    let frozen = before
        .frozen
        .as_ref()
        .context("paired Motion binding missing")?;
    let original: Identity = serde_json::from_str(frozen)?;
    original
        .validate()
        .map_err(|e| anyhow::anyhow!("inspected binding identity: {e:?}"))?;
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(include_str!(
        "../../../../migrations/0066_catabolic_command_journal.sql"
    ))?;
    let schema = |conn: &Connection| -> anyhow::Result<Vec<(String, String, Option<String>)>> {
        Ok(conn.prepare("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name='catabolic_binding' ORDER BY type,name")?
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    };
    ensure!(
        schema(db)? == schema(&expected)?,
        "inspected frozen binding guards changed"
    );
    let effective = crate::native_upgrade::effective_identity_sync(db, &original)?;
    let encoded = serde_json::to_vec(&before)?;
    Ok(VerifiedBinding {
        effective,
        evidence_digest: catabolic_core::sha256(&encoded),
    })
}
