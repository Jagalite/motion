//! SQLite enforcement for the production source-admission policy. It does not
//! stop media reads, revoke native workers, or claim application reference audit.
use crate::NativeCatalog;
use anyhow::{Context, ensure};
use playscale_core::catalog_source::{Input, Mode, State};
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

const CONTROL_TABLES: &[&str] = &[
    "catabolic_binding",
    "catabolic_commands",
    "catabolic_import_snapshots",
    "catabolic_motion_exports",
    "catabolic_source_state",
    "catabolic_source_events",
    "catabolic_source_guards",
    "catabolic_export_freezes",
];
fn quote(id: &str) -> String {
    format!("\"{}\"", id.replace('"', "\"\""))
}
pub(crate) async fn read(db: &mut SqliteConnection) -> anyhow::Result<State> {
    let (revision, allowed, text): (i64, bool, String) = sqlx::query_as(
        "SELECT revision,writes_allowed,state_json FROM catabolic_source_state WHERE singleton=1",
    )
    .fetch_one(db)
    .await?;
    let state: State = serde_json::from_str(&text)?;
    ensure!(
        state.valid() && state.revision == revision as u64 && state.writes_allowed() == allowed,
        "invalid persisted source admission state"
    );
    Ok(state)
}
/// Read and validate source admission before application upgrade hooks run.
/// Absence means an older Motion schema; malformed state is never treated as live.
pub async fn admission(pool: &sqlx::SqlitePool) -> anyhow::Result<Option<State>> {
    let mut tx = pool.begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='catabolic_source_state')")
        .fetch_one(&mut *tx).await?;
    let state = if exists {
        Some(read(&mut tx).await?)
    } else {
        None
    };
    tx.commit().await?;
    Ok(state)
}
async fn expected_guards(
    db: &mut SqliteConnection,
) -> anyhow::Result<BTreeMap<String, (String, String)>> {
    // SQLite cannot install triggers on virtual tables. FTS is a disposable
    // projection; its ordinary shadow tables are guarded along with all source
    // and application tables. Internal sqlite_sequence is not directly writable
    // by ordinary domain operations except as an effect of guarded table writes.
    let tables:Vec<(String,String)>=sqlx::query_as("SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut *db).await?;
    let mut expected = BTreeMap::new();
    for (table, sql) in tables {
        if CONTROL_TABLES.contains(&table.as_str())
            || sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE")
        {
            continue;
        }
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let name = format!(
                "motion_source_freeze_{}_{}",
                action.to_ascii_lowercase(),
                table
            );
            let statement = format!(
                "CREATE TRIGGER {} BEFORE {action} ON {} WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END",
                quote(&name),
                quote(&table)
            );
            expected.insert(name, (table.clone(), statement));
        }
    }
    Ok(expected)
}
pub(crate) async fn verify(db: &mut SqliteConnection) -> anyhow::Result<()> {
    let expected = expected_guards(db).await?;
    let recorded: Vec<(String, String, String)> =
        sqlx::query_as("SELECT trigger_name,table_name,trigger_sql FROM catabolic_source_guards")
            .fetch_all(&mut *db)
            .await?;
    let actual:Vec<(String,String)>=sqlx::query_as("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND name LIKE 'motion_source_freeze_%'").fetch_all(&mut *db).await?;
    ensure!(
        recorded.len() == expected.len() && actual.len() == expected.len(),
        "source guard inventory changed"
    );
    let actual: BTreeMap<_, _> = actual.into_iter().collect();
    for (name, table, sql) in recorded {
        ensure!(
            expected.get(&name) == Some(&(table, sql.clone())) && actual.get(&name) == Some(&sql),
            "source guard definition changed: {name}"
        );
    }
    Ok(())
}
async fn install(db: &mut SqliteConnection) -> anyhow::Result<()> {
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_guards")
        .fetch_one(&mut *db)
        .await?;
    if recorded > 0 {
        return verify(db).await;
    }
    for (name, (table, sql)) in expected_guards(db).await? {
        sqlx::raw_sql(&sql).execute(&mut *db).await?;
        sqlx::query("INSERT INTO catabolic_source_guards VALUES(?,?,?)")
            .bind(name)
            .bind(table)
            .bind(sql)
            .execute(&mut *db)
            .await?;
    }
    verify(db).await
}
impl NativeCatalog {
    pub async fn source_state(&self) -> anyhow::Result<State> {
        let mut tx = self.journal.begin().await?;
        let state = read(&mut tx).await?;
        if !state.writes_allowed() {
            verify(&mut tx).await?;
        }
        tx.commit().await?;
        Ok(state)
    }
    pub async fn freeze_motion_source(
        &self,
        migration_id: &str,
        expected_revision: u64,
    ) -> anyhow::Result<State> {
        self.source_transition(Input::Freeze {
            migration_id: migration_id.into(),
            expected_revision,
        })
        .await
    }
    pub async fn abandon_motion_freeze(&self, freeze_revision: u64) -> anyhow::Result<State> {
        self.source_transition(Input::Abandon { freeze_revision })
            .await
    }
    /// Irreversible admission closure before native activation. This method is
    /// for the trusted cutover coordinator, not an activation capability itself.
    pub async fn seal_motion_freeze(&self, freeze_revision: u64) -> anyhow::Result<State> {
        self.source_transition(Input::Seal { freeze_revision })
            .await
    }
    async fn source_transition(&self, input: Input) -> anyhow::Result<State> {
        self.status().await?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let old = read(&mut tx).await?;
        let new = old
            .transition(&input)
            .map_err(|e| anyhow::anyhow!("source transition refused: {e:?}"))?;
        if old.writes_allowed() && !new.writes_allowed() {
            let (id, _) = new.active_freeze().context("missing freeze identity")?;
            let existing: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM catabolic_import_snapshots WHERE import_id=?)",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            ensure!(
                !existing,
                "freeze needs a new import identity; existing snapshot predates freeze"
            );
            install(&mut tx).await?;
        } else if !old.writes_allowed() {
            verify(&mut tx).await?;
        }
        if old != new {
            let encoded = serde_json::to_string(&new)?;
            let changed=sqlx::query("UPDATE catabolic_source_state SET revision=?,writes_allowed=?,state_json=? WHERE singleton=1 AND revision=?")
                .bind(new.revision as i64).bind(new.writes_allowed()).bind(&encoded).bind(old.revision as i64).execute(&mut *tx).await?;
            ensure!(
                changed.rows_affected() == 1,
                "source admission changed concurrently"
            );
            sqlx::query("INSERT INTO catabolic_source_events VALUES(?,?,?)")
                .bind(new.revision as i64)
                .bind(serde_json::to_string(&input)?)
                .bind(encoded)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(new)
    }
    /// Revalidate the durable source-side witness. Native worker drain, current
    /// validation receipt and reference acknowledgment are separate obligations.
    pub async fn require_frozen_export(&self, import_id: &str) -> anyhow::Result<State> {
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let state = read(&mut tx).await?;
        verify(&mut tx).await?;
        let bound: Option<i64> = sqlx::query_scalar(
            "SELECT freeze_revision FROM catabolic_export_freezes WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&mut *tx)
        .await?;
        ensure!(
            state
                .active_freeze()
                .is_some_and(|(id, r)| id == import_id && bound == Some(r as i64)),
            "export is not bound to the current source freeze"
        );
        tx.commit().await?;
        Ok(state)
    }
}
pub(crate) async fn capture_freeze(
    db: &mut SqliteConnection,
    import_id: &str,
) -> anyhow::Result<Option<u64>> {
    let state = read(db).await?;
    if matches!(state.mode, Mode::Live) {
        return Ok(None);
    }
    ensure!(
        matches!(state.mode, Mode::Frozen { .. }),
        "sealed source cannot capture new exports"
    );
    verify(db).await?;
    let (id, revision) = state.active_freeze().context("missing source freeze")?;
    ensure!(
        id == import_id,
        "source freeze belongs to another migration"
    );
    Ok(Some(revision))
}
