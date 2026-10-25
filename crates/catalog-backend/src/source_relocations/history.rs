//! Bounded interpretation of an already committed Motion94 relocation hop.
//!
//! These readers authenticate the relationship between immutable rows inserted
//! by the exact admission/command/ACK owners. Stored JSON is never a replacement
//! for the native receipt which the ACK owner required at insertion. In
//! particular, this module cannot construct a native receipt, execution grant,
//! catalog-upgrade lineage, or a current requester authorization.
use crate::{
    source_lifecycle::{LifecycleProof, mapping_digest},
    sources::SourceMapping,
};
use anyhow::{Context, ensure};
use playscale_core::{
    access::{Permission, Principal},
    source_lifecycle::{RenewalFacts, renew_mapping},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

// One complete joined operation, including every retained text/scalar cell and
// 256 bytes per owner row. This is a supported-record bound, not truncation.
const MAX_RECORD_BYTES: i64 = 4 * 1024 * 1024;
const JOIN: &str = " FROM catabolic_source_relocation_admissions a \
 JOIN catabolic_source_relocation_commands c ON c.operation_id=a.operation_id \
 JOIN catabolic_source_relocation_acks k ON k.operation_id=c.operation_id \
 WHERE a.operation_id=?1";
const BYTES: &str = "768 \
 +length(CAST(a.operation_id AS BLOB))+length(CAST(a.source_id AS BLOB)) \
 +length(CAST(a.catalog_id AS BLOB))+length(CAST(a.incarnation AS BLOB)) \
 +length(CAST(a.predecessor_json AS BLOB))+length(CAST(a.old_mapping_json AS BLOB)) \
 +length(CAST(a.new_mapping_json AS BLOB))+length(CAST(a.old_mapping_digest AS BLOB)) \
 +length(CAST(a.new_mapping_digest AS BLOB))+length(CAST(a.requester_json AS BLOB)) \
 +length(CAST(a.commitment_json AS BLOB))+length(CAST(a.admission_digest AS BLOB)) \
 +length(CAST(a.created_at AS BLOB))+length(CAST(c.operation_id AS BLOB)) \
 +length(CAST(c.command_json AS BLOB))+length(CAST(c.command_digest AS BLOB)) \
 +length(CAST(c.created_at AS BLOB))+length(CAST(k.operation_id AS BLOB)) \
 +length(CAST(k.native_effect_json AS BLOB))+length(CAST(k.native_effect_digest AS BLOB)) \
 +length(CAST(k.successor_json AS BLOB))+length(CAST(k.created_at AS BLOB))";
const COLUMNS: &str = "a.operation_id,a.source_id,a.catalog_id,a.incarnation,\
 a.predecessor_json,a.old_mapping_json,a.new_mapping_json,a.old_mapping_digest,\
 a.new_mapping_digest,a.requester_json,a.commitment_json,a.admission_digest,\
 c.command_json,c.command_digest,k.native_effect_json,k.native_effect_digest,k.successor_json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Requester {
    pub(super) requester_id: String,
    pub(super) credential_hash: Option<String>,
    pub(super) ingress_id: Option<String>,
    pub(super) accepted_principal: Principal,
}

/// Shared with the insertion owner. Hash compact serde_json encoding of this
/// Value, not struct-field order or native spaced JSON. Timestamps are audit
/// fields, deliberately excluded from the immutable admission commitment.
pub(super) fn admission_value(
    operation: &str,
    predecessor: &LifecycleProof,
    old_mapping: &SourceMapping,
    new_mapping: &SourceMapping,
    requester: &Requester,
    commitment: &Value,
) -> Value {
    json!({
        "contract_version": 1,
        "operation_id": operation,
        "source_id": predecessor.source_id,
        "catalog_id": predecessor.catalog_id,
        "incarnation": predecessor.incarnation,
        "predecessor": predecessor,
        "old_mapping": old_mapping,
        "new_mapping": new_mapping,
        "requester": requester,
        "commitment": commitment,
    })
}

#[derive(sqlx::FromRow)]
pub(super) struct Record {
    pub(super) operation_id: String,
    pub(super) source_id: String,
    pub(super) catalog_id: String,
    pub(super) incarnation: String,
    pub(super) predecessor_json: String,
    pub(super) old_mapping_json: String,
    pub(super) new_mapping_json: String,
    pub(super) old_mapping_digest: String,
    pub(super) new_mapping_digest: String,
    pub(super) requester_json: String,
    pub(super) commitment_json: String,
    pub(super) admission_digest: String,
    pub(super) command_json: String,
    pub(super) command_digest: String,
    pub(super) native_effect_json: String,
    pub(super) native_effect_digest: String,
    pub(super) successor_json: String,
}

fn charge(count: i64, bytes: i64) -> anyhow::Result<()> {
    ensure!(count == 1, "relocation history is missing or ambiguous");
    ensure!(
        (768..=MAX_RECORD_BYTES).contains(&bytes),
        "complete relocation record exceeds 4 MiB supported bound"
    );
    Ok(())
}

/// The caller retains its lifecycle read/writer snapshot. Precharge runs before
/// any journal strings are fetched; the fetch repeats the exact byte predicate
/// so even a changed snapshot cannot bypass the materialization bound.
pub(super) async fn record_in(
    db: &mut SqliteConnection,
    operation: &str,
) -> anyhow::Result<Record> {
    let (count, bytes): (i64, i64) =
        sqlx::query_as(&format!("SELECT count(*),coalesce(sum({BYTES}),0){JOIN}"))
            .bind(operation)
            .fetch_one(&mut *db)
            .await?;
    charge(count, bytes)?;
    let record = sqlx::query_as::<_, Record>(&format!(
        "SELECT {COLUMNS}{JOIN} AND ({BYTES})=?2 AND ({BYTES})<=?3"
    ))
    .bind(operation)
    .bind(bytes)
    .bind(MAX_RECORD_BYTES)
    .fetch_one(&mut *db)
    .await?;
    Ok(record)
}

pub(crate) async fn mapping_in(
    db: &mut SqliteConnection,
    operation: &str,
    current: &LifecycleProof,
    mapping: &SourceMapping,
    expected_successor: &LifecycleProof,
) -> anyhow::Result<SourceMapping> {
    let record = record_in(db, operation).await?;
    validate(operation, current, mapping, expected_successor, record)
}

/// Synchronous adapter for the same complete operation and pure validator.
/// Neither adapter reads a native database or treats result expiry as absence.
pub(crate) fn mapping_sync(
    db: &rusqlite::Connection,
    operation: &str,
    current: &LifecycleProof,
    mapping: &SourceMapping,
    expected_successor: &LifecycleProof,
) -> anyhow::Result<SourceMapping> {
    let (count, bytes) = db.query_row(
        &format!("SELECT count(*),coalesce(sum({BYTES}),0){JOIN}"),
        [operation],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    charge(count, bytes)?;
    let record = db.query_row(
        &format!("SELECT {COLUMNS}{JOIN} AND ({BYTES})=?2 AND ({BYTES})<=?3"),
        rusqlite::params![operation, bytes, MAX_RECORD_BYTES],
        |row| {
            Ok(Record {
                operation_id: row.get(0)?,
                source_id: row.get(1)?,
                catalog_id: row.get(2)?,
                incarnation: row.get(3)?,
                predecessor_json: row.get(4)?,
                old_mapping_json: row.get(5)?,
                new_mapping_json: row.get(6)?,
                old_mapping_digest: row.get(7)?,
                new_mapping_digest: row.get(8)?,
                requester_json: row.get(9)?,
                commitment_json: row.get(10)?,
                admission_digest: row.get(11)?,
                command_json: row.get(12)?,
                command_digest: row.get(13)?,
                native_effect_json: row.get(14)?,
                native_effect_digest: row.get(15)?,
                successor_json: row.get(16)?,
            })
        },
    )?;
    validate(operation, current, mapping, expected_successor, record)
}

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn motion_hash(value: &Value) -> anyhow::Result<String> {
    Ok(catabolic_core::sha256(serde_json::to_vec(value)?))
}
pub(super) fn motion_json<T: DeserializeOwned + Serialize>(raw: &str) -> anyhow::Result<T> {
    let value: Value = serde_json::from_str(raw)?;
    ensure!(
        serde_json::to_string(&value)? == raw,
        "noncanonical Motion relocation JSON"
    );
    let typed: T = serde_json::from_value(value.clone())?;
    ensure!(
        serde_json::to_value(&typed)? == value,
        "Motion relocation JSON shape differs"
    );
    Ok(typed)
}
fn native_json(raw: &str, max: usize) -> anyhow::Result<Value> {
    ensure!(raw.len() <= max, "native relocation field exceeds bound");
    // serde's depth limit applies before the companion's canonical decoder.
    let _: Value = serde_json::from_str(raw)?;
    let value = catabolic_core::decode_json(raw)?;
    ensure!(
        value.is_object() && catabolic_core::encode(&value) == raw,
        "native relocation JSON is not a canonical object"
    );
    Ok(value)
}

// A descriptive copy of SourceRenewalPreparation, never a preparation token.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preparation {
    catalog_id: String,
    incarnation: String,
    expected_generation: i64,
    profile: String,
    source: String,
    candidate_root: String,
    content_commitment: String,
    old_binding: Value,
    new_binding: Value,
}

// Exact permanent native header returned by the real receipt-only owner. Its
// record_digest authenticates child cohorts only at that original owner; it
// cannot be recomputed from, or elevated into authority by, this description.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeEffect {
    profile: String,
    source_id: String,
    revision: i64,
    predecessor_digest: String,
    record_digest: String,
    catalog_id: String,
    incarnation: String,
    expected_generation: i64,
    committed_generation: i64,
    command_host_namespace: String,
    command_operation_id: String,
    payload_digest: String,
    admission_digest: String,
    old_binding_json: String,
    new_binding_json: String,
    policy_json: String,
    old_mapping_json: String,
    new_mapping_json: String,
    closure_digest: String,
    file_count: i64,
    context_count: i64,
    head_count: i64,
    scope_count: i64,
    evidence_count: i64,
    evidence_digest: String,
    result_digest: String,
    result_json: String,
}

pub(super) fn validate_requester(requester: &Requester) -> anyhow::Result<()> {
    let principal = &requester.accepted_principal;
    ensure!(
        principal.id == requester.requester_id && principal.allows(Permission::SourcesManage),
        "historical relocation requester scope differs"
    );
    match (&requester.credential_hash, &requester.ingress_id) {
        // These are opaque references in the existing authentication contract,
        // not new identifiers whose syntax this historical reader may invent.
        (Some(_), None) | (None, Some(_)) => {}
        (None, None) => ensure!(
            *principal == Principal::operator(),
            "relocation requester has no accepted credential or ingress"
        ),
        (Some(_), Some(_)) => anyhow::bail!("relocation requester references are ambiguous"),
    }
    Ok(())
}

pub(super) fn validate_requester_link(
    saved: &Requester,
    current: &Requester,
) -> anyhow::Result<()> {
    validate_requester(saved)?;
    validate_requester(current)?;
    ensure!(
        saved.requester_id == current.requester_id
            && saved.credential_hash == current.credential_hash
            && saved.ingress_id == current.ingress_id,
        "relocation requester linkage differs"
    );
    Ok(())
}
fn native_result(raw: &str, max: usize) -> anyhow::Result<Value> {
    ensure!(raw.len() <= max, "native result exceeds bound");
    let result = catabolic_core::catalog_command::CommandResult::new(raw)?;
    ensure!(
        result.canonical_json() == raw,
        "noncanonical native command result"
    );
    Ok(serde_json::from_str(raw)?)
}
pub(super) fn validate_mapping(mapping: &SourceMapping) -> anyhow::Result<()> {
    playscale_core::catalog_registration::SourceIdentity {
        library_id: mapping.library_id.clone(),
        profile: mapping.profile.clone(),
        native_source: mapping.native_source.clone(),
        catalog: mapping.catalog.clone(),
        motion_root: mapping.motion_root.clone(),
        motion_root_identity: mapping.motion_root_identity.clone(),
        motion_binding_revision: mapping.motion_binding_revision,
        exclusions: mapping.exclusions.clone(),
    }
    .validate()
    .map_err(|error| anyhow::anyhow!("relocation mapping identity: {error:?}"))?;
    let binding = &mapping.native_binding;
    let device = binding["device"]
        .as_u64()
        .context("binding device missing")?;
    let inode = binding["inode"].as_u64().context("binding inode missing")?;
    ensure!(
        binding.as_object().is_some_and(|object| object.len() == 6)
            && binding["profile"] == mapping.profile
            && binding["kind"] == "source"
            && binding["owner"] == mapping.native_source
            && binding["root"] == mapping.motion_root
            && mapping.motion_root_identity == format!("{device}:{inode}")
            && std::path::Path::new(&mapping.motion_root).is_absolute()
            && mapping.native_policy.is_object(),
        "relocation mapping binding identity differs"
    );
    Ok(())
}

// Historical shape validation is not fresh UUID admission. The genuine native
// receipt owner already enforced the policy (including explicit not_enrolled
// semantics); retain its exact bytes instead of requiring a weaker policy.
fn validate_native_trust_policy(raw: &str) -> anyhow::Result<()> {
    // Native trust policy is a different contract from Motion's retained scan
    // policy. Do not equate them or rewrite either as schema-upgrade evidence.
    let policy = native_json(raw, 65536)?;
    let settings = policy["settings"]
        .as_object()
        .context("native trust settings missing")?;
    ensure!(
        policy.as_object().is_some_and(|object| object.len() == 3)
            && matches!(policy["preset"].as_str(), Some("strict" | "path"))
            && policy["revision"]
                .as_i64()
                .is_some_and(|revision| revision >= 0)
            && settings.len() == 3
            && ["uuid", "device", "inode"].iter().all(|key| matches!(
                settings.get(*key).and_then(Value::as_str),
                Some("check" | "skip")
            )),
        "native relocation trust policy differs"
    );
    Ok(())
}

pub(super) fn validate(
    operation: &str,
    current: &LifecycleProof,
    mapping: &SourceMapping,
    expected_successor: &LifecycleProof,
    record: Record,
) -> anyhow::Result<SourceMapping> {
    let predecessor: LifecycleProof = motion_json(&record.predecessor_json)?;
    let old: SourceMapping = motion_json(&record.old_mapping_json)?;
    let new: SourceMapping = motion_json(&record.new_mapping_json)?;
    let requester: Requester = motion_json(&record.requester_json)?;
    let preparation: Preparation = motion_json(&record.commitment_json)?;
    let commitment = serde_json::to_value(&preparation)?;
    let successor: LifecycleProof = motion_json(&record.successor_json)?;
    validate_mapping(&old)?;
    validate_mapping(&new)?;
    validate_requester(&requester)?;
    ensure!(
        record.operation_id == operation
            && predecessor == *current
            && old == *mapping
            && record.source_id == current.source_id
            && record.catalog_id == current.catalog_id
            && record.incarnation == current.incarnation
            && old.library_id == current.source_id
            && old.catalog.catalog_id == current.catalog_id
            && old.catalog.incarnation == current.incarnation
            && record.old_mapping_digest == current.mapping_digest
            && record.old_mapping_digest == mapping_digest(&old)?
            && record.new_mapping_digest == mapping_digest(&new)?,
        "relocation predecessor or mapping commitment differs"
    );
    let mut allowed = old.clone();
    allowed.motion_root = new.motion_root.clone();
    allowed.motion_root_identity = new.motion_root_identity.clone();
    // Imported baselines have independent Motion mapping and native lifecycle
    // binding revisions. Each advances from its own exact predecessor.
    allowed.motion_binding_revision = old
        .motion_binding_revision
        .checked_add(1)
        .context("relocation mapping binding revision exhausted")?;
    allowed.native_binding["root"] = new.native_binding["root"].clone();
    allowed.native_binding["device"] = new.native_binding["device"].clone();
    allowed.native_binding["inode"] = new.native_binding["inode"].clone();
    ensure!(
        new == allowed,
        "relocation changed immutable mapping fields"
    );
    ensure!(
        record.admission_digest
            == motion_hash(&admission_value(
                operation,
                &predecessor,
                &old,
                &new,
                &requester,
                &commitment,
            ))?,
        "canonical relocation admission digest differs"
    );
    ensure!(
        preparation.catalog_id == record.catalog_id
            && preparation.incarnation == record.incarnation
            && preparation.expected_generation >= 0
            && preparation.profile == old.profile
            && preparation.source == old.native_source
            && preparation.candidate_root == new.motion_root
            && preparation.old_binding == old.native_binding
            && preparation.new_binding == new.native_binding
            && hex(&preparation.content_commitment),
        "relocation preparation commitment differs"
    );
    let command = crate::command_attempts::decode(&record.command_json)?;
    ensure!(
        command.key().operation_id() == operation
            && command.key().catalog_id() == record.catalog_id
            && command.key().host_namespace() == crate::HOST_NAMESPACE
            && command.admission_incarnation() == record.incarnation
            && command.expected_generation() == preparation.expected_generation
            && command.digest().as_str() == record.command_digest
            && *command.payload()
                == json!({
                    "operation": "source_succession",
                    "profile": preparation.profile,
                    "source": preparation.source,
                    "candidate_root": preparation.candidate_root,
                    "content_commitment": preparation.content_commitment,
                    "admission_digest": record.admission_digest,
                }),
        "relocation exact native command differs"
    );
    let effect: NativeEffect = motion_json(&record.native_effect_json)?;
    ensure!(
        record.native_effect_digest == motion_hash(&serde_json::to_value(&effect)?)?
            && effect.catalog_id == record.catalog_id
            && effect.incarnation == record.incarnation
            && effect.profile == old.profile
            && effect.source_id == old.native_source
            && effect.revision > 0
            && effect.expected_generation == command.expected_generation()
            && effect.committed_generation >= effect.expected_generation
            && effect.command_host_namespace == command.key().host_namespace()
            && effect.command_operation_id == command.key().operation_id()
            && effect.payload_digest == record.command_digest
            && effect.admission_digest == record.admission_digest
            && (0..=1024).contains(&effect.file_count)
            && (0..=64).contains(&effect.context_count)
            && (1..=16384).contains(&effect.head_count)
            && (1..=16384).contains(&effect.scope_count)
            && (0..=16384).contains(&effect.evidence_count)
            && [
                &effect.predecessor_digest,
                &effect.record_digest,
                &effect.payload_digest,
                &effect.admission_digest,
                &effect.closure_digest,
                &effect.evidence_digest,
                &effect.result_digest,
            ]
            .into_iter()
            .all(|digest| hex(digest)),
        "relocation native permanent effect commitment differs"
    );
    ensure!(
        native_json(&effect.old_binding_json, 65536)? == old.native_binding
            && native_json(&effect.new_binding_json, 65536)? == new.native_binding
            && native_json(&effect.old_mapping_json, 65536)?
                == json!({"profile":old.profile,"source_id":old.native_source,"binding":old.native_binding})
            && native_json(&effect.new_mapping_json, 65536)?
                == json!({"profile":new.profile,"source_id":new.native_source,"binding":new.native_binding}),
        "relocation native binding or mapping differs"
    );
    validate_native_trust_policy(&effect.policy_json)?;
    let result = native_result(&effect.result_json, MAX_RECORD_BYTES as usize)?;
    ensure!(
        effect.result_digest == catabolic_core::sha256(catabolic_core::encode(&result))
            && result
                == json!({
                    "source": old.native_source,
                    "profile": old.profile,
                    "succession_revision": effect.revision,
                    "binding": new.native_binding,
                    "files": effect.file_count,
                    "contexts": effect.context_count,
                    "content_commitment": preparation.content_commitment,
                    "admission_digest": record.admission_digest,
                }),
        "relocation permanent result digest or content differs"
    );
    // Zero current-owner facts here do not certify today's inventory. This is
    // only replay of the transition accepted by the immutable ACK owner; no
    // owner-authorizing capability or requester principal escapes this reader.
    let derived = renew_mapping(
        current,
        &predecessor,
        &record.new_mapping_digest,
        RenewalFacts::default(),
    )
    .map_err(|error| anyhow::anyhow!("persisted relocation transition: {error:?}"))?;
    ensure!(
        derived == successor && derived == *expected_successor,
        "relocation successor differs from deterministic lifecycle transition"
    );
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bounds and codec-shape checks only. These inputs do not represent an
    // authenticated native effect and never exercise a fabricated success ACK.
    #[test]
    fn complete_operation_budget_refuses_missing_ambiguous_and_oversized_records() {
        assert!(charge(1, 768).is_ok());
        assert!(charge(1, MAX_RECORD_BYTES).is_ok());
        for (count, bytes) in [
            (0, 0),
            (2, 768),
            (-1, 768),
            (1, -1),
            (1, 767),
            (1, MAX_RECORD_BYTES + 1),
            (1, i64::MAX),
        ] {
            assert!(charge(count, bytes).is_err());
        }
    }

    #[test]
    fn motion_codec_requires_canonical_lossless_values() {
        assert_eq!(motion_json::<Value>(r#"{"a":1}"#).unwrap(), json!({"a":1}));
        for raw in [r#"{ "a":1}"#, r#"{"a":1,"a":1}"#, r#"{"b":2,"a":1}"#] {
            assert!(motion_json::<Value>(raw).is_err());
        }
        assert!(motion_json::<Preparation>(r#"{"unexpected":true}"#).is_err());
        assert!(motion_json::<Requester>(r#"{"unexpected":true}"#).is_err());
        assert!(motion_json::<NativeEffect>(r#"{"unexpected":true}"#).is_err());
    }

    #[test]
    fn native_codec_preserves_native_spacing_and_field_bound() {
        let value = json!({"a":1,"b":2});
        let raw = catabolic_core::encode(&value);
        assert_eq!(native_json(&raw, raw.len()).unwrap(), value);
        assert!(native_json(&raw, raw.len() - 1).is_err());
        assert!(native_json(&value.to_string(), 65536).is_err());
        assert!(native_json("[]", 65536).is_err());
        assert!(native_json(r#"{"a": 1, "a": 1}"#, 65536).is_err());
    }
    #[test]
    fn native_trust_shape_preserves_check_and_skip_without_defaulting() {
        for preset in ["strict", "path"] {
            for uuid in ["check", "skip"] {
                let policy = json!({"preset":preset,"revision":0,"settings":{"uuid":uuid,"device":"check","inode":"skip"}});
                let raw = catabolic_core::encode(&policy);
                validate_native_trust_policy(&raw).unwrap();
                assert_eq!(native_json(&raw, 65536).unwrap(), policy);
                for malformed in [
                    json!({"preset":preset,"revision":0,"settings":{"uuid":"ignore","device":"check","inode":"skip"}}),
                    json!({"preset":preset,"revision":-1,"settings":{"uuid":uuid,"device":"check","inode":"skip"}}),
                    json!({"preset":"other","revision":0,"settings":{"uuid":uuid,"device":"check","inode":"skip"}}),
                    json!({"preset":preset,"revision":0,"settings":{"device":"check","inode":"skip"}}),
                    json!({"preset":preset,"revision":0,"settings":{"uuid":uuid,"device":"check","inode":"skip","extra":"skip"}}),
                ] {
                    assert!(
                        validate_native_trust_policy(&catabolic_core::encode(&malformed)).is_err()
                    );
                }
            }
        }
    }
    #[test]
    fn native_result_requires_exact_command_codec_not_evidence_codec() {
        let value = json!({"a":1,"b":2});
        let result =
            catabolic_core::catalog_command::CommandResult::new(&value.to_string()).unwrap();
        assert_eq!(
            native_result(result.canonical_json(), 65536).unwrap(),
            value
        );
        assert!(native_result(&catabolic_core::encode(&value), 65536).is_err());
        assert!(native_result("{\"a\":1,\"a\":1}", 65536).is_err());
    }
    #[test]
    fn requester_uses_global_sources_permission_not_source_as_library() {
        let mut principal = Principal::operator();
        principal.id = "scoped-device".into();
        principal.device_id = Some("device".into());
        principal.grant.permissions = [Permission::SourcesManage].into_iter().collect();
        principal.policy.library_ids = ["logical-library".into()].into_iter().collect();
        let saved = Requester {
            requester_id: principal.id.clone(),
            credential_hash: Some("credential".into()),
            ingress_id: None,
            accepted_principal: principal,
        };
        validate_requester(&saved).unwrap();
        let mut current = saved.clone();
        current.accepted_principal.policy_revision += 1;
        current.accepted_principal.policy.library_ids =
            ["other-logical-library".into()].into_iter().collect();
        validate_requester_link(&saved, &current).unwrap();
        current.credential_hash = Some("other-credential".into());
        assert!(validate_requester_link(&saved, &current).is_err());
        current = saved.clone();
        current.accepted_principal.grant.permissions =
            [Permission::CatalogRead].into_iter().collect();
        assert!(validate_requester_link(&saved, &current).is_err());
        current = saved.clone();
        current.requester_id = "other".into();
        assert!(validate_requester_link(&saved, &current).is_err());
        current = saved.clone();
        current.credential_hash = None;
        current.ingress_id = Some("ingress".into());
        assert!(validate_requester_link(&saved, &current).is_err());
    }
}
