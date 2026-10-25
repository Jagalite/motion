//! Actual graph-owner retirement for the generated receipt recovery fixture.
//! No Store reopening, SQL catalog mutation, fabricated preview or fake receipt.
use serde_json::{Value, json};

pub async fn retire_edition(
    pool: sqlx::SqlitePool,
    native_path: std::path::PathBuf,
    work_id: String,
    original_version: String,
) -> (String, String, Vec<Value>, Value) {
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    use catabolic_store::{
        catalog_commands::{
            CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
        },
        catalog_identity::CatalogManifest,
        catalog_identity_mutation::{
            IdentityAuthority, IdentityMutation, TimelineChange, TimelineState, VersionChange,
        },
        catalog_logical_edits::{
            AssociationClone, ContextReplacement, LogicalChange, LogicalEditAuthority,
            LogicalEditRequest, NewItem, VersionReplacement,
        },
        catalog_relationships::{
            AuthorizedRelationshipDelete, RelationshipAuthority, RelationshipEdge,
        },
    };
    use std::collections::{BTreeMap, BTreeSet};
    fn policy() -> ReceiptPolicy {
        ReceiptPolicy {
            retention: std::time::Duration::from_secs(3600),
            max_receipts: 1000,
            max_logical_bytes: 16 * 1024 * 1024,
            max_hold_records: 1000,
        }
    }
    fn result(outcome: CommandOutcome) -> Value {
        match outcome {
            CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => {
                result
            }
            other => panic!("actual retirement command refused: {other:?}"),
        }
    }
    let fence = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let mut reconciled =
        motion_catalog_backend::cutover::open_server(pool, native_path, "default".into())
            .await
            .unwrap();
    tokio::task::spawn_blocking(move || {
        let _fence = fence;
        let host = reconciled.admission.admitted_host().unwrap();
        let namespace = host.authorization().unwrap().namespace().to_owned();
        let identity = host.identity().unwrap();
        let generation = host.catalog_generation().unwrap();
        let (input, _) = host.capture_generated_input(generation, &original_version).unwrap();
        let edition = input.original.version.edition_item_id.clone();
        let old_timeline = input.original.version.timeline_id.clone();
        let current = host.projection_version_manifest(generation, &[original_version]).unwrap();
        let manifest: CatalogManifest = serde_json::from_value(current["manifest"].clone()).unwrap();
        assert_eq!(manifest.versions.len(), 2, "fixture must retire both original and generated versions");
        assert!(manifest.versions.iter().all(|v| v.version.edition_item_id == edition));
        let dependencies = host.capture_work_dependencies(generation, &work_id).unwrap();
        let facts = dependencies.dependencies();
        let kind = facts["owners"].as_array().unwrap().iter().find(|o| o["id"]==edition).unwrap()["kind"].as_str().unwrap().to_owned();
        let mut endpoints: BTreeSet<String> = manifest.versions.iter().map(|v| v.version.association_item_id.clone()).collect();
        endpoints.insert(edition.clone());
        // Capture all exact graph facts before removal disconnects the edition.
        let edges: Vec<Value> = facts["relationships"].as_array().unwrap().iter().filter(|e|
            e["active"]==1 && (endpoints.contains(e["source_id"].as_str().unwrap()) || endpoints.contains(e["target_id"].as_str().unwrap())))
            .cloned().collect();
        assert!(!edges.is_empty());
        let mut deletion_results = Vec::new();
        for (index,row) in edges.iter().enumerate() {
            let id = row["id"].as_str().unwrap().to_owned();
            let revision = facts["entity_revisions"].as_array().unwrap().iter().find(|r|
                r["entity_kind"]=="item_relationships" && r["entity_key"]==json!([id]) && r["deleted"]==0)
                .unwrap()["revision"].as_i64().unwrap();
            let edge = RelationshipEdge { source:row["source_id"].as_str().unwrap().into(), target:row["target_id"].as_str().unwrap().into(), kind:row["kind"].as_str().unwrap().into() };
            let grant = RelationshipAuthority { host_namespace:namespace.clone(), create_edges:BTreeSet::new(), kind_rules:BTreeMap::new(),
                delete_edges:[(id.clone(),AuthorizedRelationshipDelete {expected_revision:revision,edge:edge.clone()})].into() };
            let payload=json!({"operation":"relationships","mutation":{"action":"delete","id":id,"expected_revision":revision,"expected_edge":edge}});
            let command=CommandEnvelope::new(1,CommandKey::new(identity["catalog_id"].as_str().unwrap(),&namespace,&format!("fixture-retire-edge-{index}")).unwrap(),
                identity["catalog_incarnation"].as_str().unwrap(),host.catalog_generation().unwrap(),&payload.to_string()).unwrap();
            let (outcome,_) = host.execute_relationship_command_observed(&command,&grant,policy()).unwrap();
            deletion_results.push(result(outcome));
        }
        let children = ["fixture-retirement-left", "fixture-retirement-right"];
        let mut new_items=Vec::new(); let mut clones=Vec::new(); let mut timelines=Vec::new(); let mut versions=Vec::new();
        let mut replacements:BTreeMap<String,Vec<String>>=BTreeMap::new();
        for (child_index, child) in children.into_iter().enumerate() {
            new_items.push(NewItem{id:child.into(),kind:kind.clone()});
            let timeline=format!("{child}-timeline");
            timelines.push(TimelineChange{timeline_id:timeline.clone(),edition_item_id:child.into(),expected_revision:None,identity_state:TimelineState::Provisional,coverage:input.timeline.coverage});
            for (index,old) in manifest.versions.iter().enumerate() {
                // A real split partitions the retained version set: each old
                // binding has one exact whole-copy replacement, never two
                // overlapping copies claiming the same source coverage.
                if index != child_index { continue; }
                let owner=format!("{child}-owner-{index}");
                let owner_kind=facts["owners"].as_array().unwrap().iter().find(|o|o["id"]==old.version.association_item_id).unwrap()["kind"].as_str().unwrap().to_owned();
                new_items.push(NewItem{id:owner.clone(),kind:owner_kind});
                let mut version=old.version.clone();
                version.version_id=format!("{child}-version-{index}");
                replacements.entry(old.version.version_id.clone()).or_default().push(version.version_id.clone());
                version.edition_item_id=child.into(); version.association_item_id=owner.clone(); version.timeline_id=timeline.clone();
                for (part,binding) in version.bindings.iter_mut().enumerate() {
                    let association=format!("{child}-association-{index}-{part}");
                    clones.push(AssociationClone{source_association_id:binding.association_id.clone(),new_association_id:association.clone(),new_owner_item_id:owner.clone()});
                    binding.association_id=association; binding.binding_id=format!("{child}-binding-{index}-{part}");
                }
                versions.push(VersionChange{expected_revision:None,expected_timeline_revision:1,expected_binding_revisions:version.bindings.iter().map(|b|(b.binding_id.clone(),None)).collect(),version,equivalence:None});
            }
        }
        let retired_versions=manifest.versions.iter().map(|v| VersionReplacement {
            version_id:v.version.version_id.clone(), expected_revision:current["version_revisions"].as_array().unwrap().iter().find(|r|r["version_id"]==v.version.version_id).unwrap()["version_revision"].as_i64().unwrap(),
            replacement_version_ids:replacements.remove(&v.version.version_id).unwrap() }).collect();
        let new_ids:BTreeSet<String>=new_items.iter().map(|i|i.id.clone()).collect();
        let mut item_ids=endpoints; item_ids.extend(new_ids.clone());
        let logical=LogicalEditAuthority{host_namespace:namespace.clone(),item_ids,new_item_ids:new_ids};
        let authority=IdentityAuthority{authority_namespace:namespace.clone(),context_keys:[input.context.context_key.clone()].into(),allow_declared_evidence:false,allow_verified_evidence:false};
        let request=LogicalEditRequest{contract_version:1,change:LogicalChange::Split{source:edition.clone(),children:children.iter().map(|s|s.to_string()).collect()},new_items,association_clones:clones,
            contexts:vec![ContextReplacement{retired_versions,mutation:IdentityMutation{contract_version:1,profile:input.profile,context:input.context,timelines,versions,evidence:vec![]}}]};
        request.contexts[0].mutation.validate().expect("retirement identity replacement must satisfy the native typed contract");
        let preview=host.preview_logical_edit(host.catalog_generation().unwrap(),request,&logical,Some(&authority)).unwrap();
        assert!(!preview.reviewed.reference_map.implies_timeline_equivalence);
        let payload=json!({"operation":"logical_edit","reviewed":preview.reviewed});
        let command=CommandEnvelope::new(1,CommandKey::new(identity["catalog_id"].as_str().unwrap(),&namespace,"fixture-generated-edition-retirement").unwrap(),
            identity["catalog_incarnation"].as_str().unwrap(),host.catalog_generation().unwrap(),&payload.to_string()).unwrap();
        let grants=CatalogAuthority::new(TrustedCommandAuthority{host_namespace:&namespace,receipt_visibility:ReceiptVisibility::FullResult,allow_new_commands:true,identity_authority:Some(&authority)}).with_logical_edits(&logical);
        let retired=result(host.execute_catalog_command(&command,grants,policy()).unwrap());
        host.begin_drain(); while !host.drain(std::time::Duration::from_millis(50)).unwrap() {}
        (edition,old_timeline,deletion_results,retired)
    }).await.unwrap()
}

/// Remove one real structural edge through the admitted relationship owner.
/// Native identity heads and source bytes remain unchanged.
pub async fn detach_version(
    pool: sqlx::SqlitePool,
    native_path: std::path::PathBuf,
    work: String,
    version: String,
) -> Value {
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
    use catabolic_store::{
        catalog_commands::{CommandOutcome, ReceiptPolicy},
        catalog_identity::CatalogManifest,
        catalog_relationships::{
            AuthorizedRelationshipDelete, RelationshipAuthority, RelationshipEdge,
        },
    };
    let fence = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let mut reconciled =
        motion_catalog_backend::cutover::open_server(pool, native_path, "default".into())
            .await
            .unwrap();
    tokio::task::spawn_blocking(move || {
        let _fence=fence;
        let host=reconciled.admission.admitted_host().unwrap();
        let generation=host.catalog_generation().unwrap();
        let identity=host.identity().unwrap();
        let namespace=host.authorization().unwrap().namespace().to_owned();
        let current=host.projection_version_manifest(generation,&[version.clone()]).unwrap();
        let manifest:CatalogManifest=serde_json::from_value(current["manifest"].clone()).unwrap();
        let selected=manifest.versions.iter().find(|v|v.version.version_id==version).unwrap();
        let dependencies=host.capture_work_dependencies(generation,&work).unwrap();
        let facts=dependencies.dependencies();
        let edges:Vec<_>=facts["relationships"].as_array().unwrap().iter().filter(|e|
            e["active"]==1 && e["source_id"]==selected.version.edition_item_id && e["target_id"]==selected.version.association_item_id && e["kind"]=="custom:motion_version").collect();
        assert_eq!(edges.len(),1,"must independently identify the exact structural version edge");
        let row=edges[0];
        let id=row["id"].as_str().unwrap().to_owned();
        let revision=facts["entity_revisions"].as_array().unwrap().iter().find(|r|
            r["entity_kind"]=="item_relationships" && r["entity_key"]==json!([id]) && r["deleted"]==0).unwrap()["revision"].as_i64().unwrap();
        let edge=RelationshipEdge{source:selected.version.edition_item_id.clone(),target:selected.version.association_item_id.clone(),kind:"custom:motion_version".into()};
        let authority=RelationshipAuthority{host_namespace:namespace.clone(),create_edges:Default::default(),kind_rules:Default::default(),
            delete_edges:[(id.clone(),AuthorizedRelationshipDelete{expected_revision:revision,edge:edge.clone()})].into()};
        let payload=json!({"operation":"relationships","mutation":{"action":"delete","id":id,"expected_revision":revision,"expected_edge":edge}});
        let command=CommandEnvelope::new(1,CommandKey::new(identity["catalog_id"].as_str().unwrap(),&namespace,"fixture-detach-version-edge").unwrap(),
            identity["catalog_incarnation"].as_str().unwrap(),generation,&payload.to_string()).unwrap();
        let (outcome,_)=host.execute_relationship_command_observed(&command,&authority,ReceiptPolicy{retention:std::time::Duration::from_secs(3600),max_receipts:1000,max_logical_bytes:16*1024*1024,max_hold_records:1000}).unwrap();
        let result=match outcome {CommandOutcome::Committed{result,..}=>result,other=>panic!("actual structural deletion refused: {other:?}")};
        host.begin_drain();while !host.drain(std::time::Duration::from_millis(50)).unwrap(){}
        result
    }).await.unwrap()
}
