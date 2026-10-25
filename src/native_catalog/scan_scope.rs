//! Shared effect-free native library/source selection proof for explicit and
//! scheduled scan acceptance. Callers retain current authorization and the
//! Motion writer through this proof and durable demand insertion. This module
//! does not create a principal, schedule, demand, or native mutation.
use crate::{App, v2::Problem};
use axum::http::StatusCode;
use catabolic_store::catalog_scope::{SavedQueryRef, TrustedSelection};
use motion_catalog_backend::sources::SourceMapping;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn unavailable(e: impl std::fmt::Display) -> Problem {
    tracing::warn!(error=%e,"native scan proof rejected");
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_scan_unavailable",
        "Native scan scope is unavailable or changed",
    )
}
pub(crate) async fn sources_in(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    library: &str,
    grant: TrustedSelection,
    requested: Option<&[String]>,
) -> Result<(Value, Vec<SourceMapping>), Problem> {
    let runtime = app
        .native_catalog
        .as_ref()
        .ok_or_else(|| unavailable("native scan owner missing"))?;
    let rows = runtime
        .read_libraries(conn, vec![grant], Some(format!("motion.library:{library}")))
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<crate::native_catalog::ReadFailure>()
                .is_some_and(|failure| {
                    failure.0
                        == catabolic_store::catalog_scope::ScopeErrorCode::NotFoundOrNotVisible
                })
            {
                Problem::not_found()
            } else {
                unavailable(error)
            }
        })?;
    let row = rows.first().ok_or_else(Problem::not_found)?;
    let metadata: Value = serde_json::from_str(
        row[2]
            .as_str()
            .ok_or_else(|| unavailable("missing native library metadata"))?,
    )
    .map_err(unavailable)?;
    let definition = &metadata["motion_library"];
    if definition["contract_version"] != 1
        || definition["id"] != library
        || row[0] != format!("motion.library:{library}")
        || row[1] != "custom:motion_library"
    {
        return Err(unavailable("invalid native library definition"));
    }
    let revision = definition["catalog_revision"]
        .as_u64()
        .and_then(|r| i64::try_from(r).ok())
        .ok_or_else(|| unavailable("invalid library revision"))?;
    let defined: Vec<String> =
        serde_json::from_value(definition["source_ids"].clone()).map_err(unavailable)?;
    let expected: BTreeSet<_> = defined.iter().cloned().collect();
    if defined.is_empty() || defined.len() > 100 || expected.len() != defined.len() {
        return Err(Problem::invalid(
            "invalid_sources",
            "Select a library with 1-100 distinct sources",
        ));
    }
    type ScopeRow = (String, String, String, String, String, i64, String);
    let scopes: Vec<ScopeRow> = sqlx::query_as("SELECT catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=?")
        .bind(runtime.profile()).bind(library).bind(revision).fetch_all(&mut *conn).await?;
    if scopes.len() != 1 {
        return Err(unavailable(
            "library has no unique current verified native source scope",
        ));
    }
    let (catalog, incarnation, digest, raw, query_id, query_revision, query_digest) =
        scopes.into_iter().next().unwrap();
    if crate::v2::sha256(raw.as_bytes()) != digest {
        return Err(unavailable("source scope digest differs"));
    }
    let mappings: Vec<SourceMapping> = serde_json::from_str(&raw).map_err(unavailable)?;
    let actual: BTreeSet<_> = mappings.iter().map(|m| m.library_id.clone()).collect();
    if mappings.len() != actual.len() || actual != expected {
        return Err(unavailable(
            "native library and verified source mappings differ",
        ));
    }
    for mapping in &mappings {
        motion_catalog_backend::sources::check_mapping_in(conn, mapping)
            .await
            .map_err(unavailable)?;
    }
    let reference = SavedQueryRef::new(query_id.clone(), query_revision, query_digest.clone())
        .map_err(|error| unavailable(format!("native selection reference: {error:?}")))?;
    let checked = mappings.clone();
    let expected_catalog = catalog.clone();
    let expected_incarnation = incarnation.clone();
    runtime
        .with_host(move |host| {
            let identity = host.identity()?;
            anyhow::ensure!(
                identity["catalog_id"] == expected_catalog
                    && identity["catalog_incarnation"] == expected_incarnation,
                "library scope native identity changed"
            );
            host.validate_selection_reference(&reference)?;
            for mapping in checked {
                anyhow::ensure!(
                    mapping.catalog.catalog_id == expected_catalog
                        && mapping.catalog.incarnation == expected_incarnation
                        && identity["profile"] == mapping.profile,
                    "source scope belongs to another native owner"
                );
                anyhow::ensure!(
                    host.source_binding(&mapping.native_source)? == mapping.native_binding
                        && host.source_observation_policy(&mapping.native_source)?
                            == mapping.native_policy,
                    "source scope binding/policy changed"
                );
            }
            Ok(())
        })
        .await
        .map_err(unavailable)?;
    let selected = if let Some(ids) = requested {
        if ids.is_empty()
            || ids.len() > 100
            || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
            || ids.iter().any(|id| !expected.contains(id))
        {
            return Err(Problem::invalid(
                "invalid_sources",
                "Select 1-100 distinct sources belonging to this library",
            ));
        }
        mappings
            .into_iter()
            .filter(|m| ids.contains(&m.library_id))
            .collect()
    } else {
        mappings
    };
    Ok((
        json!({"contract_version":1,"library_id":library,"library_revision":revision,"catalog_id":catalog,"incarnation":incarnation,"source_digest":digest,"query_id":query_id,"query_revision":query_revision,"query_digest":query_digest}),
        selected,
    ))
}
