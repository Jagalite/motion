//! SQLite enforcement for the production source-admission policy. It does not
//! stop media reads, revoke native workers, or claim application reference audit.
use crate::NativeCatalog;
use anyhow::{Context, ensure};
use playscale_core::catalog_source::{Input, Mode, State};
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

const CONTROL_TABLES: &[&str] = &[
    "catabolic_source_relocation_admissions",
    "catabolic_source_relocation_commands",
    "catabolic_source_relocation_acks",
    "catabolic_source_lifecycle",
    "catabolic_scan_schedule_adoptions",
    "catabolic_scan_schedule_definitions",
    "catabolic_scan_schedule_cursors",
    "catabolic_scan_schedule_occurrences",
    "catabolic_source_watch_definitions",
    "catabolic_source_watch_admissions",
    "catabolic_generated_source",
    "catabolic_generated_publications",
    "catabolic_processing_admissions",
    "catabolic_aggregate_issuance",
    "catabolic_native_upgrade_intent",
    "catabolic_native_upgrade_ack",
    "catabolic_paired_restore_intents",
    "catabolic_paired_restore_acks",
    "catabolic_native_bootstrap_intents",
    "catabolic_native_bootstrap_commands",
    "catabolic_native_bootstrap_acks",
    "catabolic_command_attempts",
    "catabolic_imported_source_descriptors",
    "catabolic_scan_requests",
    "catabolic_scan_demands",
    "catabolic_binding",
    "catabolic_source_mappings",
    "catabolic_source_registry",
    "catabolic_source_registration_intents",
    "catabolic_discovery_intents",
    "catabolic_occurrence_mappings",
    "catabolic_cutover",
    "catabolic_cutover_sets",
    "catabolic_cutover_parts",
    "catabolic_projection_state",
    "catabolic_revision_bridges",
    "catabolic_projection_items",
    "catabolic_projection_versions",
    "catabolic_projection_files",
    "catabolic_projection_file_libraries",
    "catabolic_projection_generated_libraries",
    "catabolic_reference_audits",
    "catabolic_server_scopes",
    "catabolic_export_plans",
    "catabolic_export_plan_parts",
    "catabolic_commands",
    "catabolic_import_snapshots",
    "catabolic_motion_exports",
    "catabolic_source_state",
    "catabolic_source_events",
    "catabolic_source_guards",
    "catabolic_export_freezes",
];
// Explicit ownership allowlist. Unknown future tables remain write-closed after
// activation until their ownership is reviewed. Catalog tables/projections never
// regain legacy write authority because the host phase becomes activated.
const APPLICATION_TABLES: &[&str] = &[
    "change_events",
    "maintenance_state",
    "api_schedules",
    "schedule_dispatch_progress",
    "profiles",
    "server_viewing_histories",
    "offline_sequences",
    "playback_monitor_sessions",
    "processing_jobs",
    "renditions",
    "preview_jobs",
    "previews",
    "preview_sheet_artifacts",
    "preview_retention_cursor",
    "artwork_assets",
    "playback_sessions",
    "playback_preferences",
    "viewing_state",
    "progress",
    "timeline_viewing",
    "viewing_sessions",
    "viewing_events",
    "viewing_histories",
    "legacy_progress_attribution",
    "markers",
    "collections",
    "collection_items",
    "playlists",
    "playlist_entries",
    "queues",
    "queue_entries",
    "saved_filters",
    "downloads",
    "download_transfers",
    "download_subtitle_artifacts",
    "offline_events",
    "offline_lineage",
    "offline_event_receipts",
    "delivery_owners",
    "delivery_sessions",
    "delivery_admissions",
    "preparations",
    "content_tickets",
    "credentials",
    "devices",
    "pairings",
    "server_identity",
    "idempotency_records",
    "access_users",
    "access_invitations",
    "invitation_attempts",
    "access_owner_setup",
    "access_library_grants",
    "access_device_grants",
    "operational_metrics",
    "operational_metric_failures",
    "operational_metric_counters",
    "signin_settings",
    "signin_providers",
    "signin_identities",
    "signin_families",
    "signin_transactions",
    "signin_requests",
    "signin_proofs",
    "signin_audit",
    "signin_limits",
    "signin_outbox",
    "signin_mail_tests",
    "registration_activations",
    "backup_jobs",
    "restore_baselines",
    "restore_baseline_epoch",
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
    Ok(expected_guard_definitions(tables))
}
fn expected_guard_definitions(tables: Vec<(String, String)>) -> BTreeMap<String, (String, String)> {
    let has_projection = tables
        .iter()
        .any(|(name, _)| name == "catabolic_projection_state");
    let has_sets = tables
        .iter()
        .any(|(name, _)| name == "catabolic_cutover_sets");
    let has_cutover = tables.iter().any(|(name, _)| name == "catabolic_cutover");
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
            let activation_exception = if has_cutover
                && has_sets
                && APPLICATION_TABLES.contains(&table.as_str())
            {
                " AND NOT EXISTS(SELECT 1 FROM catabolic_cutover WHERE singleton=1 AND phase='activated') AND NOT EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE singleton=1 AND phase='activated')"
            } else if has_cutover && APPLICATION_TABLES.contains(&table.as_str()) {
                " AND NOT EXISTS(SELECT 1 FROM catabolic_cutover WHERE singleton=1 AND phase='activated')"
            } else {
                ""
            };
            let projection_exception = if has_projection
                && crate::projection::TABLES.contains(&table.as_str())
            {
                format!(" AND motion_native_projection('{}')<>1", table)
            } else if has_projection && table == "libraries" && action == "INSERT" {
                " AND motion_native_source_projection(NEW.id,NEW.name,NEW.root,NEW.root_identity,NEW.managed,NEW.enabled,NEW.revision,NEW.binding_revision,NEW.exclusions_json,NEW.scan_barrier)<>1".to_owned()
            } else {
                String::new()
            };
            let statement = format!(
                "CREATE TRIGGER {} BEFORE {action} ON {} WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0{activation_exception}{projection_exception} BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END",
                quote(&name),
                quote(&table)
            );
            expected.insert(name, (table.clone(), statement));
        }
    }
    expected
}
/// Fixed97 guard extension for an already validated sealed93 upgrade.
/// Existing guard rows and definitions are never replaced.
pub(crate) fn append_preview97_guards_in(
    db: &rusqlite::Connection,
) -> anyhow::Result<Vec<Vec<rusqlite::types::Value>>> {
    ensure!(
        !db.is_autocommit(),
        "preview guards require retained writer"
    );
    let additions = preview97_guard_definitions_in(db)?;
    let mut rows = Vec::new();
    for (name, (table, sql)) in additions {
        db.execute_batch(&sql)?;
        db.execute("INSERT INTO catabolic_source_guards(trigger_name,table_name,trigger_sql) VALUES(?,?,?)", rusqlite::params![name,table,sql])?;
        rows.push(vec![
            rusqlite::types::Value::Integer(db.last_insert_rowid()),
            name.into(),
            table.into(),
            sql.into(),
        ]);
    }
    Ok(rows)
}
/// Read-only fixed guard definitions after independent closed97 schema admission.
pub(crate) fn preview97_guard_definitions_in(
    db: &rusqlite::Connection,
) -> anyhow::Result<Vec<(String, (String, String))>> {
    let count: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    ensure!(count <= 512, "preview guard table inventory exceeded");
    let tables = db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let additions: Vec<_> = expected_guard_definitions(tables)
        .into_iter()
        .filter(|(_, (table, _))| table == "preview_sheet_artifacts")
        .collect();
    ensure!(additions.len() == 3, "exact preview guard set missing");
    Ok(additions)
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
    let schema: i64 =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&mut *db)
            .await?;
    for (name, table, sql) in recorded {
        ensure!(
            expected.get(&name) == Some(&(table.clone(), sql.clone()))
                && guard_definition_matches(
                    schema,
                    &table,
                    &sql,
                    actual.get(&name).map(String::as_str)
                ),
            "source guard definition changed: {name}"
        );
    }
    Ok(())
}
/// Exact synchronous companion for retained offline control owners. It reuses
/// the same guard producer, never a name-prefix authorization shortcut.
pub(crate) fn verify_sync(db: &rusqlite::Connection) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "source guard verification requires retained transaction"
    );
    let state_bytes: i64 = db.query_row(
        "SELECT length(CAST(state_json AS BLOB)) FROM catabolic_source_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        (1..=65536).contains(&state_bytes),
        "source state exceeds offline bound"
    );
    let (revision, allowed, text): (i64, bool, String) = db.query_row(
        "SELECT revision,writes_allowed,state_json FROM catabolic_source_state WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let state: State = serde_json::from_str(&text)?;
    ensure!(
        state.valid()
            && i64::try_from(state.revision)? == revision
            && state.writes_allowed() == allowed
            && !allowed
            && matches!(state.mode, Mode::Sealed { .. }),
        "invalid retained sealed source admission"
    );
    let (tables, bytes): (i64, i64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        (0..=512).contains(&tables) && (0..=2 * 1024 * 1024).contains(&bytes),
        "source table inventory exceeds offline bound"
    );
    let tables = db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String,String)>>>()?;
    let expected = expected_guard_definitions(tables);
    let (count, bytes): (i64, i64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(trigger_name AS BLOB))+length(CAST(table_name AS BLOB))+length(CAST(trigger_sql AS BLOB))),0) FROM catabolic_source_guards",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        usize::try_from(count)? == expected.len() && (0..=4 * 1024 * 1024).contains(&bytes),
        "source recorded guard inventory changed"
    );
    let (actual_count, actual_bytes): (i64,i64) = db.query_row("SELECT count(*),coalesce(sum(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE type='trigger' AND name LIKE 'motion_source_freeze_%'", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        (0..=4 * 1024 * 1024).contains(&actual_bytes),
        "source actual guards exceed offline bound"
    );
    ensure!(
        usize::try_from(actual_count)? == expected.len(),
        "source actual guard inventory changed"
    );
    let schema: i64 = db.query_row(
        "SELECT max(version) FROM _sqlx_migrations WHERE success=1",
        [],
        |r| r.get(0),
    )?;
    let mut query = db.prepare("SELECT g.trigger_name,g.table_name,g.trigger_sql,s.sql FROM catabolic_source_guards g LEFT JOIN sqlite_schema s ON s.type='trigger' AND s.name=g.trigger_name")?;
    for row in query.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (name, table, recorded, actual) = row?;
        ensure!(
            expected.get(&name) == Some(&(table.clone(), recorded.clone()))
                && guard_definition_matches(schema, &table, &recorded, actual.as_deref()),
            "source guard definition changed: {name}"
        );
    }
    Ok(())
}
pub(crate) fn guard_definition_matches(
    schema: i64,
    table: &str,
    recorded: &str,
    actual: Option<&str>,
) -> bool {
    actual == Some(recorded)
        || (schema >= 84
            && table == "_sqlx_migrations"
            && actual == Some(migration_ledger_guard(recorded).as_str()))
}
fn migration_ledger_guard(sql: &str) -> String {
    sql.replace(
        " BEGIN SELECT",
        " AND motion_native_projection('_sqlx_migrations')<>1 BEGIN SELECT",
    )
}
async fn install(db: &mut SqliteConnection) -> anyhow::Result<()> {
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_guards")
        .fetch_one(&mut *db)
        .await?;
    if recorded > 0 {
        return verify(db).await;
    }
    for (name, (table, sql)) in expected_guards(db).await? {
        let installed: Option<String> =
            sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?")
                .bind(&name)
                .fetch_optional(&mut *db)
                .await?;
        if let Some(installed) = installed {
            // Schema84 restores these guards atomically when rebuilding the
            // disposable file cache. Never adopt an unknown preinstalled guard.
            ensure!(
                (table == "media_files" && installed == sql)
                    || (table == "_sqlx_migrations" && installed == migration_ledger_guard(&sql)),
                "unqualified preinstalled source guard: {name}"
            );
        } else {
            sqlx::raw_sql(&sql).execute(&mut *db).await?;
        }
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
        let has_plans: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='catabolic_export_plan_parts')")
            .fetch_one(&mut *tx).await?;
        let plan_root: Option<String> = if has_plans {
            sqlx::query_scalar("SELECT p.root_import_id FROM catabolic_export_plan_parts p JOIN catabolic_import_snapshots s ON s.import_id=p.import_id AND s.input_digest=p.snapshot_digest JOIN catabolic_export_plans plan ON plan.root_import_id=p.root_import_id WHERE p.import_id=?")
                .bind(import_id).fetch_optional(&mut *tx).await?
        } else {
            None
        };
        let freeze_owner = plan_root.as_deref().unwrap_or(import_id);
        ensure!(
            state
                .active_freeze()
                .is_some_and(|(id, r)| id == freeze_owner && bound == Some(r as i64)),
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

#[cfg(test)]
mod ledger_guard_tests {
    use super::*;
    #[test]
    fn only_exact_reviewed_ledger_wrapper_matches_new_schema() {
        let original = "CREATE TRIGGER guard BEFORE INSERT ON _sqlx_migrations WHEN 1 BEGIN SELECT RAISE(ABORT,'frozen'); END";
        let wrapped = migration_ledger_guard(original);
        assert!(guard_definition_matches(
            83,
            "_sqlx_migrations",
            original,
            Some(original)
        ));
        assert!(!guard_definition_matches(
            83,
            "_sqlx_migrations",
            original,
            Some(&wrapped)
        ));
        assert!(guard_definition_matches(
            84,
            "_sqlx_migrations",
            original,
            Some(&wrapped)
        ));
        assert!(!guard_definition_matches(
            84,
            "items",
            original,
            Some(&wrapped)
        ));
        assert!(!guard_definition_matches(
            84,
            "_sqlx_migrations",
            original,
            None
        ));
        assert!(!guard_definition_matches(
            84,
            "_sqlx_migrations",
            original,
            Some(&wrapped.replace("<>1", "=1"))
        ));
    }
}
