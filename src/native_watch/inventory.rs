//! Descriptive internal administration only. No returned row is an admission grant.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigurationState {
    CurrentActorAndScope,
    NeedsReauthorization,
    StaleConfiguration,
    Disabled,
    BlockedLifecycle,
}
#[derive(Debug)]
pub(crate) struct ConfigurationView {
    pub id: String,
    pub revision: i64,
    pub state: ConfigurationState,
}
#[derive(Debug)]
pub(crate) struct SourceView {
    pub source_id: String,
    pub lifecycle: LifecycleProof,
    pub logical_library_ids: Vec<String>,
    pub configurations: Vec<ConfigurationView>,
    pub awaiting_authorization: bool,
}
#[derive(Debug)]
pub(crate) struct Inventory {
    pub sources: Vec<SourceView>,
    /// Configurations whose source is not in the qualified physical cohort.
    pub unresolved_configuration_ids: Vec<String>,
    /// Descriptive unregistered provenance, never a source binding or grant.
    pub historical_unregistered_source_ids: Vec<String>,
    pub historical_inventory_pending: bool,
}

/// Complete bounded physical inventory, never a truncated page presented as all.
/// The initial internal cohort is All-scope administration only. This does not
/// alter the public source DTO or authorize a restricted global enumeration.
pub(crate) async fn inventory(app: &App, caller: &Caller) -> anyhow::Result<Inventory> {
    caller
        .require(Permission::SourcesManage)
        .map_err(TransportDecision)?;
    ensure!(
        access::catalog_scope(&caller.principal) == access::CatalogScope::All,
        "complete watch inventory requires all-scope administration"
    );
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native watch owner missing")?;
    let selection = runtime.prepare_library_definitions(None).await?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await
        .map_err(TransportDecision)?;
    ensure!(
        access::catalog_scope(&principal) == access::CatalogScope::All,
        "watch inventory authority changed"
    );
    let sources = crate::v2::libraries::complete_source_inventory_in(app, &mut tx)
        .await
        .map_err(TransportDecision)?;
    let libraries = runtime
        .read_libraries(&mut tx, vec![selection], None)
        .await?;
    let mut memberships: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in libraries {
        let native_id = row[0].as_str().context("native library id missing")?;
        let id = native_id
            .strip_prefix("motion.library:")
            .context("native library identity differs")?;
        let metadata: Value =
            serde_json::from_str(row[2].as_str().context("native library metadata missing")?)?;
        let ids: Vec<String> =
            serde_json::from_value(metadata["motion_library"]["source_ids"].clone())?;
        ensure!(
            ids.len() <= 10000 && ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
            "invalid library source set"
        );
        for source in ids {
            memberships.entry(source).or_default().insert(id.to_owned());
        }
    }
    let (count, bytes): (i64,i64) = sqlx::query_as("SELECT count(*),COALESCE(sum(length(CAST(d.definition_json AS BLOB))),0) FROM catabolic_source_watch_definitions d WHERE d.revision=(SELECT max(n.revision) FROM catabolic_source_watch_definitions n WHERE n.id=d.id)").fetch_one(&mut *tx).await?;
    ensure!(
        count <= 10000 && bytes <= 8 * 1024 * 1024,
        "complete watch configuration inventory exceeds bound"
    );
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT id FROM catabolic_source_watch_definitions ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut configurations: BTreeMap<String, Vec<Definition>> = BTreeMap::new();
    for id in ids {
        let definition = load_in(&mut tx, &id)
            .await?
            .context("watch definition disappeared under writer")?;
        configurations
            .entry(definition.source_id.clone())
            .or_default()
            .push(definition);
    }
    let mut views = Vec::new();
    for (source_id, lifecycle) in sources {
        let library_ids = memberships.remove(&source_id).unwrap_or_default();
        let definitions = configurations.remove(&source_id).unwrap_or_default();
        let awaiting_authorization = definitions.is_empty()
            && lifecycle.state == playscale_core::source_lifecycle::State::Active;
        let mut statuses = Vec::new();
        for definition in definitions {
            ensure!(
                definition.catalog_id == lifecycle.catalog_id
                    && definition.incarnation == lifecycle.incarnation,
                "watch configuration belongs to another catalog"
            );
            let state = if !definition.enabled || definition.tombstoned {
                ConfigurationState::Disabled
            } else if lifecycle.state != playscale_core::source_lifecycle::State::Active {
                ConfigurationState::BlockedLifecycle
            } else if definition.lifecycle != lifecycle
                || !library_ids.contains(&definition.library_id)
            {
                ConfigurationState::StaleConfiguration
            } else {
                match crate::metadata_jobs::principal(
                    &mut tx,
                    &definition.requester_id,
                    definition.credential_hash.as_deref(),
                    definition.ingress_id.as_deref(),
                    Permission::SourcesManage,
                )
                .await
                {
                    Ok(p)
                        if p.id == definition.requester_id
                            && access::catalog_scope(&p).library(&definition.library_id) =>
                    {
                        ConfigurationState::CurrentActorAndScope
                    }
                    Ok(_) => ConfigurationState::NeedsReauthorization,
                    Err(error)
                        if matches!(
                            error.status,
                            axum::http::StatusCode::UNAUTHORIZED
                                | axum::http::StatusCode::FORBIDDEN
                                | axum::http::StatusCode::NOT_FOUND
                        ) =>
                    {
                        ConfigurationState::NeedsReauthorization
                    }
                    Err(error) => return Err(TransportDecision(error).into()),
                }
            };
            statuses.push(ConfigurationView {
                id: definition.id,
                revision: definition.revision,
                state,
            });
        }
        views.push(SourceView {
            source_id,
            lifecycle,
            logical_library_ids: library_ids.into_iter().collect(),
            configurations: statuses,
            awaiting_authorization,
        });
    }
    let unresolved_configuration_ids = configurations
        .into_values()
        .flatten()
        .map(|d| d.id)
        .collect();
    let prepared =
        motion_catalog_backend::imported_sources::historical::prepare_in(&mut tx).await?;
    let handle = tokio::runtime::Handle::current();
    let (historical, tx) = runtime
        .with_host(move |host| {
            let inventory = handle.block_on(prepared.verify_in(&mut tx, host))?;
            Ok((inventory, tx))
        })
        .await?;
    let physical: BTreeSet<_> = views.iter().map(|row| row.source_id.as_str()).collect();
    ensure!(
        historical
            .physical_source_ids
            .iter()
            .all(|id| physical.contains(id.as_str()))
            && historical
                .historical_source_ids
                .iter()
                .all(|id| !physical.contains(id.as_str())),
        "historical source inventory conflicts with physical descriptors"
    );
    tx.commit().await?;
    Ok(Inventory {
        sources: views,
        unresolved_configuration_ids,
        historical_unregistered_source_ids: historical.historical_source_ids,
        historical_inventory_pending: false,
    })
}
