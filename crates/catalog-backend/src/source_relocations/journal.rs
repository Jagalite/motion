//! Exact immutable94 journal custody, not fresh application authentication.
use super::{
    history::{self, Requester},
    insertion::{self, Kind},
};
use crate::{source_lifecycle::LifecycleProof, sources::SourceMapping};
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const FROM: &str = " FROM catabolic_source_relocation_admissions a JOIN catabolic_source_relocation_commands c USING(operation_id) WHERE a.operation_id=?";
const SIZE: &str = "512+length(CAST(a.operation_id AS BLOB))+length(CAST(a.source_id AS BLOB))+length(CAST(a.catalog_id AS BLOB))+length(CAST(a.incarnation AS BLOB))+length(CAST(a.predecessor_json AS BLOB))+length(CAST(a.old_mapping_json AS BLOB))+length(CAST(a.new_mapping_json AS BLOB))+length(CAST(a.old_mapping_digest AS BLOB))+length(CAST(a.new_mapping_digest AS BLOB))+length(CAST(a.requester_json AS BLOB))+length(CAST(a.commitment_json AS BLOB))+length(CAST(a.admission_digest AS BLOB))+length(CAST(c.command_json AS BLOB))+length(CAST(c.command_digest AS BLOB))";
const COLUMNS: &str = "a.operation_id,a.source_id,a.catalog_id,a.incarnation,a.predecessor_json,a.old_mapping_json,a.new_mapping_json,a.old_mapping_digest,a.new_mapping_digest,a.requester_json,a.commitment_json,a.admission_digest,c.command_json,c.command_digest";
#[derive(sqlx::FromRow)]
struct Row {
    operation_id: String,
    source_id: String,
    catalog_id: String,
    incarnation: String,
    predecessor_json: String,
    old_mapping_json: String,
    new_mapping_json: String,
    old_mapping_digest: String,
    new_mapping_digest: String,
    requester_json: String,
    commitment_json: String,
    admission_digest: String,
    command_json: String,
    command_digest: String,
}
pub(crate) struct JournalSourceCommand<'a> {
    db: &'a mut SqliteConnection,
    command: CommandEnvelope,
    requester_json: String,
    admission_digest: String,
    predecessor: LifecycleProof,
    old_mapping: SourceMapping,
}
impl JournalSourceCommand<'_> {
    pub(crate) fn command(&self) -> &CommandEnvelope {
        &self.command
    }
    pub(crate) fn writer_in(&mut self) -> &mut SqliteConnection {
        self.db
    }
    pub(super) fn source_id(&self) -> &str {
        &self.predecessor.source_id
    }
    pub(crate) async fn validate_writer_in(&mut self) -> anyhow::Result<()> {
        crate::source_lifecycle::require_retained_writer_in(self.db).await?;
        let r = read(self.db, self.command.key().operation_id())
            .await?
            .context("saved command disappeared")?;
        let c = validate(&r)?;
        ensure!(
            c.canonical_payload() == self.command.canonical_payload()
                && r.requester_json == self.requester_json
                && r.admission_digest == self.admission_digest,
            "saved command custody changed"
        );
        Ok(())
    }
    pub(super) async fn require_current_predecessor_in(&mut self) -> anyhow::Result<()> {
        self.validate_writer_in().await?;
        let (p, m) =
            crate::source_lifecycle::current_with_mapping_in(self.db, &self.predecessor.source_id)
                .await?;
        ensure!(
            p == self.predecessor && m == self.old_mapping,
            "relocation predecessor changed"
        );
        Ok(())
    }
}
async fn read(db: &mut SqliteConnection, op: &str) -> anyhow::Result<Option<Row>> {
    let (n, bytes): (i64, i64) =
        sqlx::query_as(&format!("SELECT count(*),coalesce(sum({SIZE}),0){FROM}"))
            .bind(op)
            .fetch_one(&mut *db)
            .await?;
    ensure!(n == 0 || n == 1, "ambiguous relocation journal");
    if n == 0 {
        let any:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_source_relocation_admissions WHERE operation_id=?1 UNION ALL SELECT 1 FROM catabolic_source_relocation_commands WHERE operation_id=?1)").bind(op).fetch_one(db).await?;
        ensure!(!any, "incomplete relocation journal");
        return Ok(None);
    }
    ensure!(
        (512..=MAX_BYTES as i64).contains(&bytes),
        "relocation journal exceeds bound"
    );
    Ok(Some(
        sqlx::query_as(&format!(
            "SELECT {COLUMNS}{FROM} AND ({SIZE})=? AND ({SIZE})<=?"
        ))
        .bind(op)
        .bind(bytes)
        .bind(MAX_BYTES as i64)
        .fetch_one(db)
        .await?,
    ))
}
fn validate(r: &Row) -> anyhow::Result<CommandEnvelope> {
    let prior: LifecycleProof = history::motion_json(&r.predecessor_json)?;
    let old: SourceMapping = history::motion_json(&r.old_mapping_json)?;
    let new: SourceMapping = history::motion_json(&r.new_mapping_json)?;
    let requester: Requester = history::motion_json(&r.requester_json)?;
    let prep: history::Preparation = history::motion_json(&r.commitment_json)?;
    let p = serde_json::to_value(prep)?;
    history::validate_mapping(&old)?;
    history::validate_mapping(&new)?;
    history::validate_requester(&requester)?;
    let mut allowed = old.clone();
    allowed.motion_root = new.motion_root.clone();
    allowed.motion_root_identity = new.motion_root_identity.clone();
    allowed.motion_binding_revision = old
        .motion_binding_revision
        .checked_add(1)
        .context("binding revision exhausted")?;
    for key in ["root", "device", "inode"] {
        allowed.native_binding[key] = new.native_binding[key].clone();
    }
    ensure!(
        allowed == new && r.old_mapping_digest != r.new_mapping_digest,
        "immutable mapping fields changed"
    );
    ensure!(
        prior.source_id == r.source_id
            && old.library_id == r.source_id
            && new.library_id == r.source_id
            && prior.catalog_id == r.catalog_id
            && prior.incarnation == r.incarnation
            && old.catalog.catalog_id == r.catalog_id
            && old.catalog.incarnation == r.incarnation
            && prior.state == playscale_core::source_lifecycle::State::Active
            && prior.mapping_digest == r.old_mapping_digest
            && crate::source_lifecycle::mapping_digest(&old)? == r.old_mapping_digest
            && crate::source_lifecycle::mapping_digest(&new)? == r.new_mapping_digest,
        "relocation predecessor differs"
    );
    ensure!(
        history::motion_hash(&history::admission_value(
            &r.operation_id,
            &prior,
            &old,
            &new,
            &requester,
            &p
        ))? == r.admission_digest,
        "admission digest differs"
    );
    let commitment = p["content_commitment"]
        .as_str()
        .context("missing commitment")?;
    ensure!(
        commitment.len() == 64
            && commitment
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "malformed commitment"
    );
    let c = crate::command_attempts::decode(&r.command_json)?;
    ensure!(
        c.key().operation_id() == r.operation_id
            && c.key().catalog_id() == r.catalog_id
            && c.key().host_namespace() == crate::HOST_NAMESPACE
            && c.admission_incarnation() == r.incarnation
            && c.digest().as_str() == r.command_digest
            && p["catalog_id"] == r.catalog_id
            && p["incarnation"] == r.incarnation
            && p["profile"] == old.profile
            && p["source"] == old.native_source
            && p["old_binding"] == old.native_binding
            && p["new_binding"] == new.native_binding
            && p["candidate_root"] == new.motion_root
            && p["expected_generation"].as_i64() == Some(c.expected_generation())
            && *c.payload()
                == json!({"operation":"source_succession","profile":old.profile,"source":old.native_source,"candidate_root":new.motion_root,"content_commitment":p["content_commitment"],"admission_digest":r.admission_digest}),
        "saved native command differs"
    );
    Ok(c)
}
pub(super) async fn load_in<'a>(
    db: &'a mut SqliteConnection,
    op: &str,
    requester: &Requester,
) -> anyhow::Result<Option<JournalSourceCommand<'a>>> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    let Some(r) = read(db, op).await? else {
        return Ok(None);
    };
    let saved = history::motion_json(&r.requester_json)?;
    history::validate_requester_link(&saved, requester)?;
    let command = validate(&r)?;
    Ok(Some(JournalSourceCommand {
        db,
        command,
        predecessor: history::motion_json(&r.predecessor_json)?,
        old_mapping: history::motion_json(&r.old_mapping_json)?,
        requester_json: r.requester_json,
        admission_digest: r.admission_digest,
    }))
}
/// Exact lexical spelling is the retry identity; replay never reopens roots.
pub(super) fn candidate_selector(p: &std::path::Path) -> anyhow::Result<&str> {
    use std::path::Component;
    ensure!(
        p.is_absolute()
            && p.components().all(|c| matches!(
                c,
                Component::RootDir | Component::Prefix(_) | Component::Normal(_)
            )),
        "candidate must be absolute normalized path"
    );
    let normalized: std::path::PathBuf = p.components().collect();
    ensure!(
        normalized.as_os_str() == p.as_os_str(),
        "candidate spelling not normalized"
    );
    p.to_str().context("candidate must be UTF-8")
}
pub(super) async fn recorded_command_in(
    db: &mut SqliteConnection,
    op: &str,
    expected: &LifecycleProof,
    requester: &Requester,
    candidate: &std::path::Path,
) -> anyhow::Result<Option<CommandEnvelope>> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    let path = candidate_selector(candidate)?;
    history::validate_requester(requester)?;
    let Some(r) = read(db, op).await? else {
        return Ok(None);
    };
    let c = validate(&r)?;
    history::validate_requester_link(&history::motion_json(&r.requester_json)?, requester)?;
    let prior: LifecycleProof = history::motion_json(&r.predecessor_json)?;
    ensure!(
        prior == *expected && c.payload()["candidate_root"].as_str() == Some(path),
        "operation already recorded for different request"
    );
    Ok(Some(c))
}
fn bounded_value(v: &impl serde::Serialize) -> anyhow::Result<Value> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(b.len())
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| std::io::Error::other("journal bound"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Counter(0), v)?;
    Ok(serde_json::to_value(v)?)
}
fn canonical(v: &impl serde::Serialize) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&bounded_value(v)?)?)
}
pub(super) struct PreparedIntent {
    row: Row,
    pub(super) command: CommandEnvelope,
}
impl PreparedIntent {
    pub(super) fn require_owned94_bound(&self)->anyhow::Result<()> {
        let r=&self.row;
        let fields=[&r.operation_id,&r.source_id,&r.catalog_id,&r.incarnation,&r.predecessor_json,&r.old_mapping_json,&r.new_mapping_json,&r.old_mapping_digest,&r.new_mapping_digest,&r.requester_json,&r.commitment_json,&r.admission_digest,&r.command_json,&r.command_digest];
        let bytes=fields.iter().try_fold(512usize,|n,s|n.checked_add(s.len())).context("owned94 row size overflow")?;
        ensure!(bytes<=128*1024,"owned94 new journal exceeds supported encoded cohort");
        Ok(())
    }
    pub(super) fn retained_bytes(&self) -> anyhow::Result<u64> {
        let r = &self.row;
        let fields = [&r.operation_id, &r.source_id, &r.catalog_id, &r.incarnation,
            &r.predecessor_json, &r.old_mapping_json, &r.new_mapping_json,
            &r.old_mapping_digest, &r.new_mapping_digest, &r.requester_json,
            &r.commitment_json, &r.admission_digest, &r.command_json, &r.command_digest];
        let bytes = fields.iter().try_fold(0u64, |n, s| n.checked_add(s.capacity() as u64)
            .ok_or_else(|| anyhow::anyhow!("prepared94 row accounting overflow")))?;
        bytes.checked_add(super::owned_operation::command_bytes(&self.command)?)
            .context("prepared94 command accounting overflow")
    }
}
pub(super) async fn insert_new_in(
    db: &mut SqliteConnection, op: &str, expected: &LifecycleProof,
    requester: &Requester,
    p: &catabolic_store::migration::offline_bootstrap::SourceRenewalPreparation,
) -> anyhow::Result<CommandEnvelope> {
    let prepared = prepare_new_in(db, op, expected, requester, p).await?;
    insert_prepared_intent_in(db, &prepared).await?;
    Ok(prepared.command)
}
pub(super) async fn prepare_new_in(
    db: &mut SqliteConnection,
    op: &str,
    expected: &LifecycleProof,
    requester: &Requester,
    p: &catabolic_store::migration::offline_bootstrap::SourceRenewalPreparation,
) -> anyhow::Result<PreparedIntent> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    ensure!(
        read(db, op).await?.is_none(),
        "saved relocation must use receipt-first recovery"
    );
    let (current, old) =
        crate::source_lifecycle::current_with_mapping_in(db, &expected.source_id).await?;
    ensure!(
        current == *expected && current.state == playscale_core::source_lifecycle::State::Active,
        "relocation CAS differs"
    );
    history::validate_requester(requester)?;
    ensure!(
        p.profile == old.profile
            && p.source == old.native_source
            && p.catalog_id == current.catalog_id
            && p.incarnation == current.incarnation
            && p.old_binding == old.native_binding,
        "preparation source differs"
    );
    let mut new = old.clone();
    new.motion_root = p.candidate_root.clone();
    new.motion_root_identity = format!(
        "{}:{}",
        p.new_binding["device"].as_u64().context("device absent")?,
        p.new_binding["inode"].as_u64().context("inode absent")?
    );
    new.motion_binding_revision = old
        .motion_binding_revision
        .checked_add(1)
        .context("binding exhausted")?;
    new.native_binding = p.new_binding.clone();
    let prep = bounded_value(p)?;
    let digest = history::motion_hash(&history::admission_value(
        op, &current, &old, &new, requester, &prep,
    ))?;
    let command=CommandEnvelope::new(1,CommandKey::new(&current.catalog_id,crate::HOST_NAMESPACE,op)?,&current.incarnation,p.expected_generation,&json!({"operation":"source_succession","profile":p.profile,"source":p.source,"candidate_root":p.candidate_root,"content_commitment":p.content_commitment,"admission_digest":digest}).to_string())?;
    let r = Row {
        operation_id: op.into(),
        source_id: current.source_id.clone(),
        catalog_id: current.catalog_id.clone(),
        incarnation: current.incarnation.clone(),
        predecessor_json: canonical(&current)?,
        old_mapping_json: canonical(&old)?,
        new_mapping_json: canonical(&new)?,
        old_mapping_digest: crate::source_lifecycle::mapping_digest(&old)?,
        new_mapping_digest: crate::source_lifecycle::mapping_digest(&new)?,
        requester_json: canonical(requester)?,
        commitment_json: canonical(&prep)?,
        admission_digest: digest,
        command_json: command.canonical_payload().into(),
        command_digest: command.digest().as_str().into(),
    };
    validate(&r)?;
    let bytes = [
        &r.operation_id,
        &r.source_id,
        &r.catalog_id,
        &r.incarnation,
        &r.predecessor_json,
        &r.old_mapping_json,
        &r.new_mapping_json,
        &r.old_mapping_digest,
        &r.new_mapping_digest,
        &r.requester_json,
        &r.commitment_json,
        &r.admission_digest,
        &r.command_json,
        &r.command_digest,
    ]
    .iter()
    .try_fold(512usize, |n, v| n.checked_add(v.len()))
    .context("journal size overflow")?;
    ensure!(bytes <= MAX_BYTES, "journal exceeds bound");
    Ok(PreparedIntent { row: r, command })
}
pub(super) async fn insert_prepared_intent_in(db: &mut SqliteConnection, prepared: &PreparedIntent) -> anyhow::Result<()> {
    let r = &prepared.row;
    let now = crate::now();
    insertion::insert(
        db,
        Kind::Admission,
        vec![
            json!(r.operation_id),
            json!(r.source_id),
            json!(r.catalog_id),
            json!(r.incarnation),
            json!(r.predecessor_json),
            json!(r.old_mapping_json),
            json!(r.new_mapping_json),
            json!(r.old_mapping_digest),
            json!(r.new_mapping_digest),
            json!(r.requester_json),
            json!(r.commitment_json),
            json!(r.admission_digest),
            json!(now),
        ],
    )
    .await?;
    insertion::insert(
        db,
        Kind::Command,
        vec![
            json!(r.operation_id),
            json!(r.command_json),
            json!(r.command_digest),
            json!(now),
        ],
    )
    .await?;
    Ok(())
}
pub(super) struct PreparedAck {
    operation: String,
    prior: LifecycleProof,
    old: SourceMapping,
    pub(super) successor: LifecycleProof,
    effect: String,
    effect_digest: String,
    successor_json: String,
    pub(super) saved: bool,
}
impl PreparedAck {
    pub(super) fn retained_bytes(&self) -> anyhow::Result<u64> {
        let own = (self.operation.capacity() + self.effect.capacity() + self.effect_digest.capacity() + self.successor_json.capacity()) as u64;
        own.checked_add(super::owned_operation::proof_bytes(&self.prior)?)
            .and_then(|n| n.checked_add(super::owned_operation::proof_bytes(&self.successor).ok()?))
            .and_then(|n| n.checked_add(super::owned_operation::mapping_bytes(&self.old).ok()?))
            .context("prepared94 ACK accounting overflow")
    }
}
pub(super) async fn acknowledge_in(
    db: &mut SqliteConnection, op: &str, requester: &Requester,
    receipt: &catabolic_store::migration::offline_bootstrap::SourceRenewalReceipt,
) -> anyhow::Result<LifecycleProof> {
    let prepared = prepare_ack_in(db, op, requester, receipt).await?;
    if !prepared.saved { insert_prepared_ack_in(db, &prepared).await?; }
    Ok(prepared.successor)
}
pub(super) async fn prepare_ack_in(
    db: &mut SqliteConnection,
    op: &str,
    requester: &Requester,
    receipt: &catabolic_store::migration::offline_bootstrap::SourceRenewalReceipt,
) -> anyhow::Result<PreparedAck> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    let r = read(db, op).await?.context("relocation intent absent")?;
    let command = validate(&r)?;
    history::validate_requester_link(&history::motion_json(&r.requester_json)?, requester)?;
    ensure!(
        command.canonical_payload() == receipt.command().canonical_payload(),
        "native receipt command differs"
    );
    let prior: LifecycleProof = history::motion_json(&r.predecessor_json)?;
    let old: SourceMapping = history::motion_json(&r.old_mapping_json)?;
    let (current, mapping) =
        crate::source_lifecycle::current_with_mapping_in(db, &r.source_id).await?;
    let successor = playscale_core::source_lifecycle::renew_mapping(
        &prior,
        &prior,
        &r.new_mapping_digest,
        Default::default(),
    )
    .map_err(|e| anyhow::anyhow!("renewal history: {e:?}"))?;
    let effect = canonical(receipt.effect_description())?;
    let effect_digest = history::motion_hash(receipt.effect_description())?;
    let successor_json = canonical(&successor)?;
    let record = history::Record {
        operation_id: r.operation_id,
        source_id: r.source_id,
        catalog_id: r.catalog_id,
        incarnation: r.incarnation,
        predecessor_json: r.predecessor_json,
        old_mapping_json: r.old_mapping_json,
        new_mapping_json: r.new_mapping_json,
        old_mapping_digest: r.old_mapping_digest,
        new_mapping_digest: r.new_mapping_digest,
        requester_json: r.requester_json,
        commitment_json: r.commitment_json,
        admission_digest: r.admission_digest,
        command_json: r.command_json,
        command_digest: r.command_digest,
        native_effect_json: effect.clone(),
        native_effect_digest: effect_digest.clone(),
        successor_json: canonical(&successor)?,
    };
    history::validate(op, &prior, &old, &successor, record)?;
    let saved:Option<(String,String,String)>=sqlx::query_as("SELECT native_effect_json,native_effect_digest,successor_json FROM catabolic_source_relocation_acks WHERE operation_id=?").bind(op).fetch_optional(&mut *db).await?;
    if let Some((raw, digest, next)) = saved {
        ensure!(
            raw == effect && digest == effect_digest && next == canonical(&successor)?,
            "saved ACK differs"
        );
        return Ok(PreparedAck { operation: op.to_owned(), prior, old, successor, effect, effect_digest, successor_json, saved: true });
    }
    ensure!(
        current == prior && mapping == old,
        "relocation ACK predecessor changed"
    );
    Ok(PreparedAck { operation: op.to_owned(), prior, old, successor, effect, effect_digest, successor_json, saved: false })
}
pub(super) async fn insert_prepared_ack_in(db: &mut SqliteConnection, prepared: &PreparedAck) -> anyhow::Result<()> {
    ensure!(!prepared.saved, "existing ACK cannot be inserted again");
    let op = prepared.operation.as_str();
    let prior = &prepared.prior;
    let old = &prepared.old;
    let successor = &prepared.successor;
    let effect = &prepared.effect;
    let effect_digest = &prepared.effect_digest;
    insertion::insert(
        db,
        Kind::Ack,
        vec![
            json!(op),
            json!(effect),
            json!(effect_digest),
            json!(prepared.successor_json),
            json!(crate::now()),
        ],
    )
    .await?;
    crate::source_lifecycle::append_relocation_in(db, &prior, &successor, &old, op).await?;
    let (actual, new) =
        crate::source_lifecycle::current_with_mapping_in(db, &prior.source_id).await?;
    ensure!(
        actual == *successor
            && crate::source_lifecycle::mapping_digest(&new)? == successor.mapping_digest,
        "terminal ACK history differs"
    );
    Ok(())
}

/// Service94 admits this smaller concrete cohort before the legacy decoder runs.
/// This checks SQL byte counts only; transitive graph/scratch has a separate share.
pub(super) async fn require_owned94_record_bound(db: &mut SqliteConnection, op: &str) -> anyhow::Result<()> {
    let (n, bytes): (i64, i64) = sqlx::query_as(&format!("SELECT count(*),coalesce(sum({SIZE}),0){FROM}"))
        .bind(op).fetch_one(&mut *db).await?;
    ensure!((n == 0 || n == 1) && (0..=131072).contains(&bytes), "service94 journal exceeds supported 128KiB encoded cohort");
    Ok(())
}

// Fixed post-insertion verification. No fresh requester authority is invented.
pub(super) async fn verify_prepared_intent_in(db:&mut SqliteConnection,prepared:&PreparedIntent)->anyhow::Result<()> {
    let r=read(db,&prepared.row.operation_id).await?.context("inserted94 intent disappeared")?;
    let command=validate(&r)?;
    ensure!(command.canonical_payload()==prepared.command.canonical_payload()
        && r.requester_json==prepared.row.requester_json && r.admission_digest==prepared.row.admission_digest,
        "inserted94 exact command/requester changed");
    Ok(())
}
pub(super) async fn verify_prepared_ack_in(db:&mut SqliteConnection,prepared:&PreparedAck)->anyhow::Result<()> {
    let (effect,digest,next):(String,String,String)=sqlx::query_as("SELECT native_effect_json,native_effect_digest,successor_json FROM catabolic_source_relocation_acks WHERE operation_id=?").bind(&prepared.operation).fetch_one(&mut *db).await?;
    ensure!(effect==prepared.effect && digest==prepared.effect_digest && next==prepared.successor_json,"inserted94 exact ACK changed");
    let (actual,mapping)=crate::source_lifecycle::current_with_mapping_in(db,&prepared.prior.source_id).await?;
    ensure!(actual==prepared.successor && crate::source_lifecycle::mapping_digest(&mapping)?==prepared.successor.mapping_digest,"inserted94 successor changed");
    Ok(())
}

/// Called only after the whole-history borrowed transitive preflight. Measures
/// actual saved typed inputs for the finite native result/receipt destinations;
/// it does not open a candidate, prepare native succession or infer permission.
pub(super) async fn saved_graph94_in(db:&mut SqliteConnection,op:&str,deadline:std::time::Instant,cancel:&std::sync::atomic::AtomicBool)->anyhow::Result<u64> {
    let r=read(db,op).await?.context("saved94 intent absent")?;
    let old:SourceMapping=history::motion_json(&r.old_mapping_json)?;
    let new:SourceMapping=history::motion_json(&r.new_mapping_json)?;
    ensure!(super::owned_operation::mapping_bytes(&old)?<=128*1024 && super::owned_operation::mapping_bytes(&new)?<=128*1024,"saved94 source graph exceeds supported cohort");
    let old=super::owned_operation::allocation::serialized(&old,256*1024*1024,deadline,cancel)?.total()?;
    let new=super::owned_operation::allocation::serialized(&new,256*1024*1024,deadline,cancel)?.total()?;
    old.checked_add(new).context("saved94 graph size overflow")
}
