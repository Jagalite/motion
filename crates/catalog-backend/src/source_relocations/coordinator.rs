//! Fixed receipt-first dispatch. Actual Caller/drain custody belongs to host.
use super::{history::Requester, journal};
use crate::offline_control_upgrade::StageSourceSession;
use anyhow::{Context, ensure};
use catabolic_store::{
    catalog_commands::ReceiptPolicy, migration::offline_bootstrap::SourceRenewalReceipt,
};
use sqlx::SqliteConnection;
use std::sync::atomic::{AtomicBool, Ordering};
pub(super) async fn dispatch_in(
    session: &mut StageSourceSession,
    db: &mut SqliteConnection,
    op: &str,
    requester: &Requester,
    policy: ReceiptPolicy,
    cancel: &AtomicBool,
) -> anyhow::Result<SourceRenewalReceipt> {
    let token = journal::load_in(db, op, requester)
        .await?
        .context("committed original intent required")?;
    if let Some(receipt) = session.receipt_in(token).await? {
        return Ok(receipt);
    }
    let mut token = journal::load_in(db, op, requester)
        .await?
        .context("original intent disappeared")?;
    token.require_current_predecessor_in().await?;
    let source = token.source_id().to_owned();
    {
        let db = token.writer_in();
        let scans:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_demands WHERE source_id=? AND (phase IN ('queued','running','cancelling') OR status='pending')").bind(&source).fetch_one(&mut *db).await?;
        let discovery:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_discovery_intents WHERE source_id=? AND command_json IS NOT NULL AND state<>'committed'").bind(&source).fetch_one(&mut *db).await?;
        let jobs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM processing_jobs WHERE phase IN ('queued','running','cancelling')",
        )
        .fetch_one(&mut *db)
        .await?;
        let publications:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications WHERE command_json IS NOT NULL AND acknowledged=0").fetch_one(&mut *db).await?;
        ensure!(
            scans == 0 && discovery == 0 && jobs == 0 && publications == 0,
            "Motion owners not idle"
        );
        ensure!(
            source != crate::generated_source::SOURCE_ID,
            "private generated source cannot relocate"
        );
    }
    session.execute_in(token, policy, cancel).await?;
    let token = journal::load_in(db, op, requester)
        .await?
        .context("original intent disappeared")?;
    session
        .receipt_in(token)
        .await?
        .context("execution has no permanent receipt")
}
pub(super) async fn commit_new_intent(
    session: &mut StageSourceSession,
    mut tx: sqlx::Transaction<'static, sqlx::Sqlite>,
    op: &str,
    expected: &crate::source_lifecycle::LifecycleProof,
    requester: &Requester,
    candidate: &std::path::Path,
    cancel: &AtomicBool,
) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
    journal::candidate_selector(candidate)?;
    session.verify_pair_in(&mut tx).await?;
    let (current, mapping) =
        crate::source_lifecycle::current_with_mapping_in(&mut tx, &expected.source_id).await?;
    ensure!(current == *expected, "source CAS differs");
    ensure!(
        journal::load_in(&mut tx, op, requester).await?.is_none(),
        "saved intent requires receipt-first recovery"
    );
    let prep = session
        .prepare_in(
            &mut tx,
            &mapping.profile,
            &mapping.native_source,
            candidate,
            cancel,
        )
        .await?;
    ensure!(
        prep.candidate_root == journal::candidate_selector(candidate)?,
        "preparation changed candidate spelling"
    );
    let command = journal::insert_new_in(&mut tx, op, expected, requester, &prep).await?;
    session.verify_pair_in(&mut tx).await?;
    ensure!(
        !cancel.load(Ordering::Acquire),
        "intent cancelled before commit"
    );
    tx.commit().await?;
    Ok(command)
}
// Only internal fixtures may combine dispatch and ACK. Production uses the
// opaque phase so fresh actual Caller checks occur after native settlement.
#[cfg(test)]
pub(super) async fn commit_recorded_ack(
    session: &mut StageSourceSession,
    mut tx: sqlx::Transaction<'static, sqlx::Sqlite>,
    op: &str,
    requester: &Requester,
    policy: ReceiptPolicy,
    cancel: &AtomicBool,
) -> anyhow::Result<crate::source_lifecycle::LifecycleProof> {
    session.verify_pair_in(&mut tx).await?;
    let receipt = dispatch_in(session, &mut tx, op, requester, policy, cancel).await?;
    let proof = journal::acknowledge_in(&mut tx, op, requester, &receipt).await?;
    session.verify_pair_in(&mut tx).await?;
    tx.commit().await?;
    Ok(proof)
}
