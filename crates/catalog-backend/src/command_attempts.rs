//! Immutable bounded attempts. Callers hold the Motion writer fence, commit the
//! returned next intent before dispatch, and reauthorize on every retry.
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use catabolic_runtime::host::StaleGenerationWitness;
use sqlx::SqliteConnection;

pub fn decode(saved: &str) -> anyhow::Result<CommandEnvelope> {
    let value: serde_json::Value = serde_json::from_str(saved)?;
    let envelope = CommandEnvelope::new(
        1,
        serde_json::from_value(value["key"].clone())?,
        value["admission_incarnation"]
            .as_str()
            .context("missing incarnation")?,
        value["expected_generation"]
            .as_i64()
            .context("missing generation")?,
        &value["payload"].to_string(),
    )?;
    ensure!(
        envelope.canonical_payload() == saved,
        "noncanonical command intent"
    );
    Ok(envelope)
}
const ORIGINAL_SQL: &str = "SELECT envelope_json FROM catabolic_commands WHERE operation_id=?1 UNION ALL SELECT command_json FROM catabolic_discovery_intents WHERE operation_id=?1 AND command_json IS NOT NULL LIMIT 2";
const CHAIN_SQL: &str = "SELECT attempt,original_envelope_json,prior_envelope_json,envelope_json,stale_witness_json FROM catabolic_command_attempts WHERE business_id=?1 ORDER BY attempt LIMIT 32";
type AttemptRow = (i64, String, String, String, String);
fn validate_original(saved: &[String], original: &CommandEnvelope) -> anyhow::Result<()> {
    ensure!(
        saved.len() == 1 && saved[0] == original.canonical_payload(),
        "original business intent is missing or ambiguous"
    );
    Ok(())
}
async fn original_in(db: &mut SqliteConnection, original: &CommandEnvelope) -> anyhow::Result<()> {
    let saved: Vec<String> = sqlx::query_scalar(ORIGINAL_SQL)
        .bind(original.key().operation_id())
        .fetch_all(&mut *db)
        .await?;
    validate_original(&saved, original)
}
fn successor(
    original: &CommandEnvelope,
    prior: &CommandEnvelope,
    attempt: u32,
    generation: i64,
) -> anyhow::Result<CommandEnvelope> {
    let key =
        playscale_core::catalog_command_attempt::operation_id(original.digest().as_str(), attempt)
            .map_err(|e| anyhow::anyhow!("attempt identity: {e:?}"))?;
    Ok(CommandEnvelope::new(
        1,
        CommandKey::new(
            original.key().catalog_id(),
            original.key().host_namespace(),
            key,
        )?,
        original.admission_incarnation(),
        generation,
        &prior.payload().to_string(),
    )?)
}
/// Loads and validates the entire (at most 31 row) chain. Never infers native
/// execution from absence. Recovery must reconcile this exact latest envelope.
pub async fn latest_in(
    db: &mut SqliteConnection,
    original: &CommandEnvelope,
) -> anyhow::Result<(u32, CommandEnvelope)> {
    original_in(db, original).await?;
    let rows: Vec<AttemptRow> = sqlx::query_as(CHAIN_SQL)
        .bind(original.key().operation_id())
        .fetch_all(&mut *db)
        .await?;
    validate_chain(original, rows)
}
/// Synchronous read of exactly the same immutable business intent and bounded
/// canonical ancestry as `latest_in`. The caller owns its Motion transaction;
/// this does not infer a receipt, authorize execution, or mint a stale witness.
pub fn latest_sync(
    db: &rusqlite::Connection,
    original: &CommandEnvelope,
) -> anyhow::Result<(u32, CommandEnvelope)> {
    let saved = db
        .prepare(ORIGINAL_SQL)?
        .query_map([original.key().operation_id()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    validate_original(&saved, original)?;
    let rows = db
        .prepare(CHAIN_SQL)?
        .query_map([original.key().operation_id()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<AttemptRow>>>()?;
    validate_chain(original, rows)
}
fn validate_chain(
    original: &CommandEnvelope,
    rows: Vec<AttemptRow>,
) -> anyhow::Result<(u32, CommandEnvelope)> {
    ensure!(rows.len() <= 31, "attempt chain exceeds bound");
    let mut prior = original.clone();
    let mut ordinal = 0;
    for (attempt, base, previous, next, witness) in rows {
        ensure!(
            base == original.canonical_payload() && previous == prior.canonical_payload(),
            "attempt ancestry mismatch"
        );
        let proof: serde_json::Value = serde_json::from_str(&witness)?;
        ensure!(
            proof["envelope"].as_str() == Some(prior.canonical_payload()),
            "stale witness intent mismatch"
        );
        let generation = proof["current_generation"]
            .as_i64()
            .context("missing stale generation")?;
        ordinal = playscale_core::catalog_command_attempt::next_attempt(
            ordinal,
            prior.expected_generation(),
            generation,
        )
        .map_err(|e| anyhow::anyhow!("invalid retry transition: {e:?}"))?;
        ensure!(attempt == i64::from(ordinal), "noncontiguous attempt chain");
        let expected = successor(original, &prior, ordinal, generation)?;
        ensure!(
            expected.canonical_payload() == next,
            "attempt changed native business payload"
        );
        prior = expected;
    }
    Ok((ordinal, prior))
}
/// Exact historical journal evidence, not execution or receipt authority. The
/// matched attempt may precede the latest attempt; native commit proof must
/// independently authenticate it. The caller retains its Motion writer.
pub struct HistoricalEnvelope {
    pub original: CommandEnvelope,
    pub matched: CommandEnvelope,
}
/// A bounded description of one retained Motion journal snapshot. This is not
/// execution, receipt, absence, or current-generation authority. A bootstrap
/// builder prepares it once under its retained writer, then independently checks
/// every matched envelope against the native permanent receipt commitment.
pub struct HistoricalIndex {
    entries: Vec<(String, CommandEnvelope)>,
    keys: std::collections::BTreeMap<(String, String, String), Vec<usize>>,
    originals: std::collections::BTreeMap<String, Vec<usize>>,
    chains: std::collections::BTreeMap<String, Vec<AttemptRow>>,
    validated: std::collections::BTreeSet<String>,
    #[cfg(test)]
    validations: usize,
}
fn historical_key(key: &CommandKey) -> (String, String, String) {
    (
        key.catalog_id().to_owned(),
        key.host_namespace().to_owned(),
        key.operation_id().to_owned(),
    )
}
pub struct HistoricalEnvelopeRef<'a> {
    pub original: &'a CommandEnvelope,
    pub matched: &'a CommandEnvelope,
}
impl HistoricalIndex {
    /// Returns exact historical evidence, including validation of all later
    /// retries. Never substitutes the latest attempt for the requested key.
    pub fn resolve_ref(
        &mut self,
        key: &CommandKey,
    ) -> anyhow::Result<Option<HistoricalEnvelopeRef<'_>>> {
        let Some(matches) = self.keys.get(&historical_key(key)) else {
            return Ok(None);
        };
        ensure!(matches.len() == 1, "historical command key is ambiguous");
        let (business, matched) = &self.entries[matches[0]];
        let bases = self
            .originals
            .get(business)
            .context("historical original intent is missing")?;
        ensure!(bases.len() == 1, "historical original intent is ambiguous");
        let original = &self.entries[bases[0]].1;
        ensure!(
            original.key().operation_id() == business.as_str(),
            "historical original key differs from journal"
        );
        if !self.validated.contains(business) {
            let rows = self.chains.get(business).cloned().unwrap_or_default();
            validate_chain(original, rows)?;
            #[cfg(test)]
            {
                self.validations += 1;
            }
            self.validated.insert(business.clone());
        }
        Ok(Some(HistoricalEnvelopeRef { original, matched }))
    }
    pub fn resolve(&mut self, key: &CommandKey) -> anyhow::Result<Option<HistoricalEnvelope>> {
        Ok(self.resolve_ref(key)?.map(|found| HistoricalEnvelope {
            original: found.original.clone(),
            matched: found.matched.clone(),
        }))
    }
}
/// Complete-scan finite cohort, charged before journal strings are fetched.
/// All original and retry envelopes are decoded canonically once. Full chains
/// remain available for exact per-business validation, including later retries.
/// Oversized inventories refuse rather than representing a prefix as absence.
pub async fn historical_index_in(db: &mut SqliteConnection) -> anyhow::Result<HistoricalIndex> {
    let (count, bytes): (i64,i64) = sqlx::query_as("SELECT count(*),coalesce(sum(bytes),0) FROM (SELECT length(CAST(operation_id AS BLOB))+length(CAST(envelope_json AS BLOB)) AS bytes FROM catabolic_commands UNION ALL SELECT length(CAST(operation_id AS BLOB))+length(CAST(command_json AS BLOB)) AS bytes FROM catabolic_discovery_intents WHERE command_json IS NOT NULL UNION ALL SELECT length(CAST(business_id AS BLOB))+length(CAST(original_envelope_json AS BLOB))+length(CAST(prior_envelope_json AS BLOB))+length(CAST(envelope_json AS BLOB))+length(CAST(stale_witness_json AS BLOB)) AS bytes FROM catabolic_command_attempts)").fetch_one(&mut *db).await?;
    ensure!(
        count <= 10000 && bytes <= 16 * 1024 * 1024,
        "historical command inventory exceeds bounded cohort"
    );
    let originals: Vec<(String,String)> = sqlx::query_as("SELECT operation_id,envelope_json FROM catabolic_commands UNION ALL SELECT operation_id,command_json FROM catabolic_discovery_intents WHERE command_json IS NOT NULL").fetch_all(&mut *db).await?;
    let attempts: Vec<(String,i64,String,String,String,String)> = sqlx::query_as("SELECT business_id,attempt,original_envelope_json,prior_envelope_json,envelope_json,stale_witness_json FROM catabolic_command_attempts ORDER BY business_id,attempt").fetch_all(&mut *db).await?;
    let mut index = HistoricalIndex {
        entries: Vec::new(),
        keys: Default::default(),
        originals: Default::default(),
        chains: Default::default(),
        validated: Default::default(),
        #[cfg(test)]
        validations: 0,
    };
    for (business, raw) in originals {
        let envelope = decode(&raw)?;
        let position = index.entries.len();
        index
            .keys
            .entry(historical_key(envelope.key()))
            .or_default()
            .push(position);
        index
            .originals
            .entry(business.clone())
            .or_default()
            .push(position);
        index.entries.push((business, envelope));
    }
    for (business, attempt, base, prior, raw, witness) in attempts {
        let envelope = decode(&raw)?;
        let position = index.entries.len();
        index
            .keys
            .entry(historical_key(envelope.key()))
            .or_default()
            .push(position);
        index.entries.push((business.clone(), envelope));
        let chain = index.chains.entry(business).or_default();
        ensure!(chain.len() < 31, "attempt chain exceeds bound");
        chain.push((attempt, base, prior, raw, witness));
    }
    Ok(index)
}
/// Convenience for a single lookup. Multi-subject builders must retain one
/// `historical_index_in` result rather than scanning the journal per subject.
pub async fn historical_in(
    db: &mut SqliteConnection,
    key: &CommandKey,
) -> anyhow::Result<Option<HistoricalEnvelope>> {
    historical_index_in(db).await?.resolve(key)
}

/// Witness is native-minted and non-deserializable. A stored audit proof alone
/// cannot authorize a new retry. Returns a pending intent, not permission to run.
pub async fn record_next_in(
    db: &mut SqliteConnection,
    original: &CommandEnvelope,
    current: &CommandEnvelope,
    witness: StaleGenerationWitness,
) -> anyhow::Result<CommandEnvelope> {
    let (ordinal, latest) = latest_in(db, original).await?;
    ensure!(
        latest.canonical_payload() == current.canonical_payload()
            && witness.envelope() == current.canonical_payload(),
        "stale decision is not for latest attempt"
    );
    let next = playscale_core::catalog_command_attempt::next_attempt(
        ordinal,
        current.expected_generation(),
        witness.current_generation(),
    )
    .map_err(|e| anyhow::anyhow!("retry refused: {e:?}"))?;
    let envelope = successor(original, current, next, witness.current_generation())?;
    sqlx::query("INSERT INTO catabolic_command_attempts(business_id,attempt,original_envelope_json,prior_envelope_json,envelope_json,stale_witness_json,created_at) VALUES(?,?,?,?,?,?,CAST(strftime('%s','now') AS INTEGER))")
 .bind(original.key().operation_id()).bind(i64::from(next)).bind(original.canonical_payload()).bind(current.canonical_payload()).bind(envelope.canonical_payload()).bind(serde_json::to_string(&witness)?).execute(&mut *db).await?;
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn original() -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new("catalog", "host", "business").unwrap(),
            "incarnation",
            3,
            r#"{"operation":"fixture","stable_id":"same"}"#,
        )
        .unwrap()
    }
    fn database(original: &CommandEnvelope) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE catabolic_commands(operation_id TEXT,envelope_json TEXT); CREATE TABLE catabolic_discovery_intents(operation_id TEXT,command_json TEXT); CREATE TABLE catabolic_command_attempts(business_id TEXT,attempt INTEGER,original_envelope_json TEXT,prior_envelope_json TEXT,envelope_json TEXT,stale_witness_json TEXT);").unwrap();
        db.execute(
            "INSERT INTO catabolic_commands VALUES(?,?)",
            rusqlite::params![original.key().operation_id(), original.canonical_payload()],
        )
        .unwrap();
        db
    }
    fn row(original: &CommandEnvelope) -> AttemptRow {
        let next = successor(original, original, 1, 4).unwrap();
        (
            1,
            original.canonical_payload().into(),
            original.canonical_payload().into(),
            next.canonical_payload().into(),
            serde_json::json!({"envelope":original.canonical_payload(),"current_generation":4})
                .to_string(),
        )
    }
    #[test]
    fn sync_attempt_read_requires_original_and_exact_canonical_ancestry() {
        let original = original();
        let db = database(&original);
        assert_eq!(
            latest_sync(&db, &original).unwrap().1.canonical_payload(),
            original.canonical_payload()
        );
        let (attempt, base, prior, next, witness) = row(&original);
        db.execute(
            "INSERT INTO catabolic_command_attempts VALUES(?,?,?,?,?,?)",
            rusqlite::params![
                original.key().operation_id(),
                attempt,
                base,
                prior,
                next,
                witness
            ],
        )
        .unwrap();
        let actual = latest_sync(&db, &original).unwrap();
        assert_eq!(actual.0, 1);
        assert_eq!(
            actual.1.canonical_payload(),
            successor(&original, &original, 1, 4)
                .unwrap()
                .canonical_payload()
        );
        db.execute(
            "UPDATE catabolic_command_attempts SET prior_envelope_json='wrong'",
            [],
        )
        .unwrap();
        assert!(latest_sync(&db, &original).is_err());
        db.execute("DELETE FROM catabolic_command_attempts", [])
            .unwrap();
        db.execute(
            "INSERT INTO catabolic_discovery_intents VALUES(?,?)",
            rusqlite::params![original.key().operation_id(), original.canonical_payload()],
        )
        .unwrap();
        assert!(latest_sync(&db, &original).is_err());
    }
    #[test]
    fn pure_attempt_chain_rejects_payload_changes_and_overflow() {
        let original = original();
        let mut altered = row(&original);
        altered.3 = original.canonical_payload().into();
        assert!(validate_chain(&original, vec![altered]).is_err());
        assert!(validate_chain(&original, vec![row(&original); 32]).is_err());
        let mut future = row(&original);
        future.4 =
            serde_json::json!({"envelope":original.canonical_payload(),"current_generation":2})
                .to_string();
        assert!(validate_chain(&original, vec![future]).is_err());
    }
    #[tokio::test]
    async fn historical_lookup_selects_exact_older_attempt_and_validates_later_ancestry() {
        use sqlx::Connection;
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql("CREATE TABLE catabolic_commands(operation_id TEXT,envelope_json TEXT); CREATE TABLE catabolic_discovery_intents(operation_id TEXT,command_json TEXT); CREATE TABLE catabolic_command_attempts(business_id TEXT,attempt INTEGER,original_envelope_json TEXT,prior_envelope_json TEXT,envelope_json TEXT,stale_witness_json TEXT);").execute(&mut db).await.unwrap();
        let original = original();
        sqlx::query("INSERT INTO catabolic_commands VALUES(?,?)")
            .bind(original.key().operation_id())
            .bind(original.canonical_payload())
            .execute(&mut db)
            .await
            .unwrap();
        let first = successor(&original, &original, 1, 4).unwrap();
        let second = successor(&original, &first, 2, 5).unwrap();
        for (ordinal, prior, next) in [(1, &original, &first), (2, &first, &second)] {
            sqlx::query("INSERT INTO catabolic_command_attempts VALUES(?,?,?,?,?,?)")
                .bind(original.key().operation_id()).bind(ordinal).bind(original.canonical_payload())
                .bind(prior.canonical_payload()).bind(next.canonical_payload())
                .bind(serde_json::json!({"envelope":prior.canonical_payload(),"current_generation":next.expected_generation()}).to_string())
                .execute(&mut db).await.unwrap();
        }
        let mut index = historical_index_in(&mut db).await.unwrap();
        for expected in [&original, &first, &second] {
            let found = index.resolve(expected.key()).unwrap().unwrap();
            assert_eq!(
                found.matched.canonical_payload(),
                expected.canonical_payload()
            );
            assert_eq!(
                found.original.canonical_payload(),
                original.canonical_payload()
            );
        }
        assert_eq!(
            index.validated.len(),
            1,
            "one chain validated for all exact references"
        );
        assert_eq!(index.validations, 1, "complete chain decoded only once");
        let unknown = CommandKey::new("catalog", "host", "unknown").unwrap();
        assert!(index.resolve(&unknown).unwrap().is_none());
        assert!(historical_in(&mut db, &unknown).await.unwrap().is_none());
        sqlx::query(
            "UPDATE catabolic_command_attempts SET prior_envelope_json='wrong' WHERE attempt=2",
        )
        .execute(&mut db)
        .await
        .unwrap();
        assert!(historical_in(&mut db, first.key()).await.is_err());
        assert!(historical_in(&mut db, original.key()).await.is_err());
        sqlx::query("DELETE FROM catabolic_command_attempts")
            .execute(&mut db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO catabolic_discovery_intents VALUES(?,?)")
            .bind(original.key().operation_id())
            .bind(original.canonical_payload())
            .execute(&mut db)
            .await
            .unwrap();
        assert!(historical_in(&mut db, original.key()).await.is_err());
    }
}
