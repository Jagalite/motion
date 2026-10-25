//! Service-retained Motion94. The borrowed token is a fixed host-auth seam, not
//! an actor, a native getter, or a reusable scalar permission. No95 transition.
mod connection;
pub(crate) mod producer;
use producer::RelocationProducer;
mod authorization;
mod capture_handoff;
mod capture_ingress;
pub(crate) use capture_ingress::RetainedCaptureIngress94;
pub(crate) mod allocation;
use super::{facade::{SourceRelocationRequester, SourceRelocationSession, SourceSessionCloseOperation}, journal};
use crate::{source_lifecycle::LifecycleProof, sources::SourceMapping};
use anyhow::{Context as _, ensure};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::{catalog_commands::{CommandOutcome, CommandStoreError}, migration::offline_bootstrap::{SourceRenewalPreparation, SourceRenewalReceipt}};
use connection::{Purpose, RetainedConnection};
use sqlx::SqliteConnection;
use std::{path::PathBuf, sync::atomic::{AtomicBool, Ordering}, task::{Context, Poll}, time::Instant};

pub use connection::CommitState as SourceRelocation94CommitState;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRelocation94Purpose { Intent, Dispatch, Ack, Reconcile }
impl SourceRelocation94Purpose { fn index(self)->usize {match self {Self::Intent=>0,Self::Dispatch=>1,Self::Ack=>2,Self::Reconcile=>3}} fn fixed(self) -> Purpose { match self { Self::Intent=>Purpose::Intent, Self::Dispatch=>Purpose::Dispatch, Self::Ack=>Purpose::Ack, Self::Reconcile=>Purpose::Reconcile } } }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRelocation94Phase { Admitted, Opening, ReadyIntent, PendingIntentCommit, IntentSaved, DispatchAttempted, ReceiptPendingUnretired, ReceiptPending, PendingAckCommit, AcknowledgedUnretired, Ready94, ClosingTargetForCapture, CaptureInputReady, Interrupted, Settling, Closed }

/// Request facts, never authority. Only actual retained96 may admit this owner.
pub struct SourceRelocation94Request { operation: String, expected: LifecycleProof, candidate: PathBuf }
impl SourceRelocation94Request {
    pub fn relocate(operation: String, expected: LifecycleProof, candidate: PathBuf) -> anyhow::Result<Self> {
        ensure!(!operation.is_empty() && operation.len() <= 1024, "owned94 operation exceeds cohort");
        journal::candidate_selector(&candidate)?;
        ensure!(candidate.capacity() <= 16*1024 && proof_bytes(&expected)? <= 16*1024, "owned94 request exceeds cohort");
        Ok(Self { operation, expected, candidate })
    }
}
pub(crate) struct RelocationAccounting { pub(crate) host: u64 }
pub(super) struct Admission {
    request: SourceRelocation94Request, deadline: Instant, accounting: RelocationAccounting, prospective: u64,
    original_cancel: usize, capture_ingress: Option<capture_ingress::Admission>,
}
// Supported logical cohort, not a hard RSS/allocator or SQLx-worker bound. The
// journal is restricted to128KiB BEFORE its legacy decoder. The fixed
// transitive visitor separately charges actual rows, nodes, parser/encoding
// scratch and typed clones; prospective new graphs are measured before use.

const OBJECT: u64 = 128 * 1024;
// Exact generated schema envelope:96 scalar/key/container nodes. The19
// simultaneous conservative tree destinations are2 preparation bindings,6
// intent validation/canonical representations,1 native result,4 retained
// receipts, and6 ACK validation/canonical representations.
const GENERATED_NODES: u64 = 96;
const GENERATED_TREES: u64 = 2 + 6 + 1 + 4 + 6;
const LIMIT: u64 = 256 * 1024 * 1024;
// One new immediate finish_saved claim remains in its unpolled borrowed frame.
// Do not reuse the existing four transient rollback/result slots for this field.
const SAVED_COMPLETION_CLAIM_STORAGE: usize = std::mem::size_of::<anyhow::Result<()>>();
fn alive(deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(!cancel.load(Ordering::Acquire) && Instant::now() < deadline, "owned94 cancelled or original deadline expired"); Ok(())
}
fn add(parts: &[u64]) -> anyhow::Result<u64> { parts.iter().try_fold(0u64, |n,v| n.checked_add(*v).filter(|n| *n <= LIMIT).context("owned94 logical allocation cohort exceeded")) }
fn retained_error<T>(r: &anyhow::Result<T>) -> anyhow::Result<&T> { r.as_ref().map_err(|e| anyhow::anyhow!("retained94 operation failed: {e:#}")) }
impl SourceRelocationSession {
    pub fn admit_owned94(&mut self, request: SourceRelocation94Request, deadline: Instant, host_storage: u64, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(self.owned94.is_none(), "owned94 already admitted");
        alive(deadline,cancel)?;
        let path=self.inner.path94();
        ensure!(path.as_os_str().len()<=4096,"owned94 staged target path exceeds supported cohort");
        // Original witness path is already counted by the source proof. This
        // separate share covers the operation clone, options filename, worker
        // CString and simultaneous verification path temporaries (plus NUL).
        let target_paths=(path.capacity() as u64).checked_mul(5).and_then(|n|n.checked_add(1)).context("owned94 staged path accounting overflow")?;
        let storage = add(&[host_storage,target_paths,authorization::Guard::allocation_reservation(), OwnedSourceRelocation94::inline_storage_reservation(),
            RetainedConnection::fixed_frame_bytes().context("owned94 fixed frame size overflow")? as u64,
            request.operation.capacity() as u64, request.candidate.capacity() as u64, proof_bytes(&request.expected)?])?;
        let accounting = self.inner.admit94(storage,deadline,cancel)?;
        // Native refresh accepted this exact operation before its Box allocation.
        self.owned94 = Some(Box::new(Admission { request, deadline, accounting, prospective:0, original_cancel: cancel as *const AtomicBool as usize, capture_ingress:None }));
        Ok(())
    }
    pub fn begin_owned94(mut self) -> Result<OwnedSourceRelocation94, Self> {
        let Some(admission) = self.owned94.take() else { return Err(self); };
        Ok(OwnedSourceRelocation94 { source:Some(self), close:None, admission, target:None,
            phase:SourceRelocation94Phase::Admitted, active:None, completed:false, guard:authorization::Guard::new(),
            command:None, preparation:None, preparation_attempted:false, receipt_attempted:0, prepared_intent:None, prepared_ack:None,
            native_outcome:None, receipts:Vec::with_capacity(4), intent_saved:false,
            insert_attempted:[false;4], insert_result:std::array::from_fn(|_|None), dispatch_ready:false, reconcile_used:false })
    }
}
/// The whole service source, original H/K provenance and close marker remain
/// here through Ready94, CaptureInputReady and all failure states. There is deliberately no source
/// extractor, consuming run/spawn, global recovery sink, retry or install API.
pub struct OwnedSourceRelocation94 {
    source: Option<SourceRelocationSession>, close: Option<SourceSessionCloseOperation>,
    admission: Box<Admission>, target: Option<RetainedConnection>, phase: SourceRelocation94Phase,
    active: Option<SourceRelocation94Purpose>, completed: bool, guard:authorization::Guard,
    command: Option<CommandEnvelope>, preparation: Option<RelocationProducer<SourceRenewalPreparation, catabolic_store::Error>>,
    preparation_attempted: bool, receipt_attempted: usize,
    prepared_intent: Option<journal::PreparedIntent>, prepared_ack: Option<journal::PreparedAck>,
    native_outcome: Option<RelocationProducer<CommandOutcome, CommandStoreError>>,
    receipts: Vec<RelocationProducer<Option<SourceRenewalReceipt>, catabolic_store::Error>>,
    intent_saved: bool, insert_attempted: [bool;4], insert_result: [Option<anyhow::Result<()>>;4],
    dispatch_ready: bool, reconcile_used: bool,
}
impl OwnedSourceRelocation94 {
    pub fn inline_storage_reservation() -> u64 { (std::mem::size_of::<Self>() + std::mem::size_of::<Admission>() + 4*std::mem::size_of::<RelocationProducer<Option<SourceRenewalReceipt>, catabolic_store::Error>>() + 8*std::mem::size_of::<RelocationTargetWriter<'static>>() + 4*std::mem::size_of::<anyhow::Result<()>>() + SAVED_COMPLETION_CLAIM_STORAGE + capture_handoff::frame_bytes()) as u64 }
    pub fn deadline(&self) -> Instant { self.admission.deadline }
    pub fn phase(&self) -> SourceRelocation94Phase { self.phase }
    pub fn source_close_admitted(&self) -> bool { self.source.as_ref().map(|s|s.inner.source_close_admitted94()).unwrap_or(self.close.is_some()) }
    /// Borrow-only observation of the actual retained result. Native output/error
    /// custody cannot be separated from its independent allocation guard.
    pub fn retained_outcome(&self) -> Option<Result<&CommandOutcome, &(dyn std::error::Error + Send + Sync)>> { self.native_outcome.as_ref().map(RelocationProducer::result) }
    pub fn receipt_observed(&self) -> bool { self.receipts.iter().any(|r| matches!(r.result(),Ok(Some(_)))) }
    pub fn intent_commit_state(&self) -> SourceRelocation94CommitState { self.target.as_ref().map(|t|t.commit_state(Purpose::Intent)).unwrap_or(SourceRelocation94CommitState::NotAttempted) }
    pub fn ack_commit_state(&self) -> SourceRelocation94CommitState { self.target.as_ref().map(|t| t.commit_state(if self.reconcile_used { Purpose::Reconcile } else { Purpose::Ack })).unwrap_or(SourceRelocation94CommitState::NotAttempted) }
    pub fn successor(&self) -> Option<&LifecycleProof> { if matches!(self.phase, SourceRelocation94Phase::AcknowledgedUnretired|SourceRelocation94Phase::Ready94|SourceRelocation94Phase::ClosingTargetForCapture|SourceRelocation94Phase::CaptureInputReady) { self.prepared_ack.as_ref().map(|p|&p.successor) } else {None} }
    pub fn clean_receipt_pending(&self) -> bool { self.phase==SourceRelocation94Phase::ReceiptPending && self.active.is_none() && self.receipt_observed() }
    pub async fn open(&mut self,cancel:&AtomicBool) -> anyhow::Result<()> {
        ensure!(matches!(self.phase,SourceRelocation94Phase::Admitted|SourceRelocation94Phase::Opening),"owned94 cannot open");
        alive(self.deadline(),cancel)?;
        if self.target.is_none() {
            self.phase=SourceRelocation94Phase::Opening;
            self.target=Some(RetainedConnection::opening(self.source.as_ref().unwrap().inner.path94().to_owned()));
        }
        self.target.as_mut().unwrap().wait_open().await?;
        alive(self.deadline(),cancel)?;
        self.phase=SourceRelocation94Phase::ReadyIntent; Ok(())
    }
    pub fn authorization(&mut self,purpose:SourceRelocation94Purpose) -> anyhow::Result<RelocationTargetWriter<'_>> {
        ensure!(self.active.is_none(),"owned94 prior authorization not normally retired");
        let allowed=match purpose { SourceRelocation94Purpose::Intent=>self.phase==SourceRelocation94Phase::ReadyIntent,
            SourceRelocation94Purpose::Dispatch=>self.phase==SourceRelocation94Phase::IntentSaved,
            SourceRelocation94Purpose::Ack=>self.clean_receipt_pending() && !self.reconcile_used,
            SourceRelocation94Purpose::Reconcile=>self.clean_receipt_pending() && !self.reconcile_used };
        ensure!(allowed,"owned94 purpose not admitted from this phase");
        ensure!(Instant::now()<self.deadline(),"owned94 deadline expired");
        if purpose==SourceRelocation94Purpose::Reconcile {self.reconcile_used=true;}
        self.active=Some(purpose); self.completed=false; 
        // Failure or a dropped waiter is fail-closed. Only fixed successful steps
        // plus explicit app reader retirement can leave this state.
        self.phase=SourceRelocation94Phase::Interrupted;
        Ok(RelocationTargetWriter { owner:self,purpose, began:false, saved_ready:false })
    }
    pub fn finish_authorization(&mut self,purpose:SourceRelocation94Purpose)->anyhow::Result<()> {
        ensure!(self.active==Some(purpose)&&self.completed,"owned94 authorization incomplete");
        self.phase=match purpose {SourceRelocation94Purpose::Intent=>SourceRelocation94Phase::IntentSaved,SourceRelocation94Purpose::Dispatch=>SourceRelocation94Phase::ReceiptPending,SourceRelocation94Purpose::Ack|SourceRelocation94Purpose::Reconcile=>SourceRelocation94Phase::Ready94};
        self.active=None; Ok(())
    }
    pub async fn settle_target(&mut self)->anyhow::Result<()> {
        self.phase=SourceRelocation94Phase::Settling;
        if self.target.is_some() {
            self.settle_retained_target().await?;
        } else {
            ensure!(!self.guard.restricted(), "owned94 restricted target absent");
        }
        Ok(())
    }
    // The same finite settlement/close path serves terminal cleanup and the
    // positive capture-input checkpoint. Only the outer one-shot claim may
    // classify its success as CaptureInputReady; this helper grants nothing.
    async fn settle_retained_target(&mut self)->anyhow::Result<()> {
        let target=self.target.as_mut().context("owned94 target absent")?;
        if self.guard.restricted() {
            self.guard.cleanup(target.connection_mut()?).await?;
        }
        target.settle().await?;
        target.close().await
    }
    pub fn close_source(&mut self)->anyhow::Result<()> {
        ensure!(self.phase==SourceRelocation94Phase::Settling,"owned94 target not settled");
        ensure!(self.target.as_ref().map(|t|t.settled()&&t.closed()).unwrap_or(true),"owned94 target not closed");
        if self.close.is_none() {
            let source=self.source.take().context("owned94 source absent")?;
            match source.begin_retained_close() {Ok(close)=>self.close=Some(close),Err(source)=>{self.source=Some(source);anyhow::bail!("owned94 retained close admission missing")}}
        }
        self.close.as_mut().unwrap().close()?;
        self.phase=SourceRelocation94Phase::Closed; Ok(())
    }
    pub fn closed(&self)->bool {self.phase==SourceRelocation94Phase::Closed}
    async fn restore_fixed_access(&mut self)->anyhow::Result<()> {
        self.guard.restore(self.target.as_mut().context("owned94 target absent")?.connection_mut()?).await
    }
    async fn refresh(&mut self,cancel:&AtomicBool)->anyhow::Result<u64> {
        self.restore_fixed_access().await?;
        let deadline=self.deadline();
        let extra=add(&[self.admission.prospective,self.retained_graph_bytes()?])?;
        self.source.as_mut().context("owned94 source closed")?.inner.refresh94(&self.admission.accounting,self.target.as_mut().context("owned94 target absent")?.connection_mut()?,extra,deadline,cancel).await
    }
    fn retained_graph_bytes(&self)->anyhow::Result<u64> {
        let mut n=0;
        if let Some(c)=&self.command {n=add(&[n,command_bytes(c)?])?;}
        if let Some(Ok(p))=self.preparation.as_ref().map(RelocationProducer::result) {
            n=add(&[n,value_bytes(&p.old_binding)?,value_bytes(&p.new_binding)?])?;
            for s in [&p.catalog_id,&p.incarnation,&p.profile,&p.source,&p.candidate_root,&p.content_commitment] {n=add(&[n,s.capacity() as u64])?;}
        }
        if let Some(p)=&self.prepared_intent {n=add(&[n,p.retained_bytes()?])?;}
        if let Some(p)=&self.prepared_ack {n=add(&[n,p.retained_bytes()?])?;}
        if let Some(Ok(CommandOutcome::Committed{result,..}|CommandOutcome::Replay{result,..}))=self.native_outcome.as_ref().map(RelocationProducer::result) {n=add(&[n,value_bytes(result)?])?;}
        for r in &self.receipts {if let Ok(Some(r))=r.result() {n=add(&[n,command_bytes(r.command())?,value_bytes(r.effect_description())?])?;}}
        Ok(n)
    }
    fn receipt(&self)->anyhow::Result<&SourceRenewalReceipt> {
        for result in self.receipts.iter().rev() {if let Some(receipt)=result.value()? {return Ok(receipt)}}
        anyhow::bail!("owned94 permanent native receipt absent")
    }
}
/// Same-WRITE purpose-bound authorization seam for the fixed application Caller.
/// Calling authorize_connection does NOT authorize an effect. The app keeps
/// genuine current+target snapshots alive and checks them immediately before
/// each first effect-capable poll (and synchronous native dispatch invocation).
/// The actual WRITE connection is temporarily SQL-read-restricted by a sticky
/// static authorizer owned here. SQL mutations/transaction endings are denied;
/// trusted code replacing the connection or raw hooks remains unsupported.
pub struct RelocationTargetWriter<'a> {owner:&'a mut OwnedSourceRelocation94,purpose:SourceRelocation94Purpose,began:bool,saved_ready:bool}
impl RelocationTargetWriter<'_> {
    pub async fn authorize_connection(&mut self,cancel:&AtomicBool)->anyhow::Result<&mut SqliteConnection> {
        if !self.began {
            self.owner.target.as_mut().context("owned94 target absent")?.begin(self.purpose.fixed()).await?;
            self.began=true;
        }
        self.verify(cancel).await?;
        self.owner.guard.restrict(self.owner.target.as_mut().unwrap().connection_mut()?).await?;
        self.owner.target.as_mut().unwrap().connection_mut()
    }
    pub async fn verify(&mut self,cancel:&AtomicBool)->anyhow::Result<()> {
        self.owner.refresh(cancel).await?;
        let o=&mut *self.owner;
        let db=o.target.as_mut().context("owned94 target absent")?.connection_mut()?;
        o.source.as_ref().context("owned94 source closed")?.inner.verify_pair_in(db).await?;
        if o.phase==SourceRelocation94Phase::PendingIntentCommit {
            journal::verify_prepared_intent_in(db,o.prepared_intent.as_ref().context("owned94 prepared intent absent")?).await?;
        }
        if o.phase==SourceRelocation94Phase::PendingAckCommit {
            journal::verify_prepared_ack_in(db,o.prepared_ack.as_ref().context("owned94 prepared ACK absent")?).await?;
        }
        Ok(())
    }
    /// Only this new target's actual SQLite DBSTATUS. Not native bytes or H/K.
    pub async fn retained_allocations(&mut self,cancel:&AtomicBool)->anyhow::Result<u64> {
        self.owner.refresh(cancel).await
    }
    pub async fn prepare_intent(&mut self,requester:&SourceRelocationRequester,cancel:&AtomicBool)->anyhow::Result<()> {
        ensure!(self.purpose==SourceRelocation94Purpose::Intent&&!self.owner.intent_saved&&!self.owner.preparation_attempted,"owned94 intent preparation cannot repeat");
        self.owner.refresh(cancel).await?; self.verify(cancel).await?;
        let requester_bytes=allocation::serialized(&requester.inner,LIMIT,self.owner.deadline(),cancel)?.total()?;
        self.owner.admission.prospective=add(&[self.owner.admission.prospective,requester_bytes])?;
        self.owner.refresh(cancel).await?;
        let o=&mut *self.owner; let request=&o.admission.request;
        let db=o.target.as_mut().unwrap().connection_mut()?;
        journal::require_owned94_record_bound(db,&request.operation).await?;
        // Saved intent is not success by itself: exact requester/CAS/candidate.
        let saved=journal::recorded_command_in(db,&request.operation,&request.expected,&requester.inner,&request.candidate).await?;
        if let Some(command)=saved {
            let graph=journal::saved_graph94_in(db,&request.operation,o.admission.deadline,cancel).await?;
            let predecessor=allocation::serialized(&request.expected,LIMIT,o.admission.deadline,cancel)?.total()?;
            let candidate=allocation::serialized(&journal::candidate_selector(&request.candidate)?,LIMIT,o.admission.deadline,cancel)?.total()?;
            let prospective=add(&[graph,predecessor,candidate,GENERATED_NODES*6*1024])?.checked_mul(GENERATED_TREES.div_ceil(6)).context("saved94 prospective graph overflow")?;
            o.admission.prospective=add(&[o.admission.prospective,prospective])?;
            o.command=Some(command);o.intent_saved=true;
            self.owner.refresh(cancel).await?;
            self.saved_ready=true;
            return Ok(())
        }
        let (current,mapping)=crate::source_lifecycle::current_with_mapping_in(db,&request.expected.source_id).await?;
        ensure!(current==request.expected,"owned94 source CAS differs");
        ensure!(mapping_bytes(&mapping)?<=OBJECT,"owned94 source mapping exceeds supported cohort");
        // Before native preparation, admit its fixed schema derived from this
        // actual mapping and lexical candidate. Named generated graph copies
        // cover old/new bindings, preparation, admission/canonical validation,
        // outcome and four retained receipt copies; native heavy work has its
        // separate original Completed operation budget.
        let mapped=allocation::serialized(&mapping,LIMIT,o.admission.deadline,cancel)?.total()?;
        let predecessor=allocation::serialized(&request.expected,LIMIT,o.admission.deadline,cancel)?.total()?;
        let candidate=allocation::serialized(&journal::candidate_selector(&request.candidate)?,LIMIT,o.admission.deadline,cancel)?.total()?;
        // Serializer includes actual escaped byte lengths, unlike a raw path
        // capacity multiplier. Its six-tree groups are rounded up to cover the
        // named nineteen simultaneous generated destinations.
        let prospective=add(&[mapped,predecessor,candidate,GENERATED_NODES*6*1024])?.checked_mul(GENERATED_TREES.div_ceil(6)).context("owned94 prospective graph overflow")?;
        o.admission.prospective=add(&[o.admission.prospective,prospective])?;
        self.owner.refresh(cancel).await?;
        let o=&mut *self.owner;let request=&o.admission.request;
        let db=o.target.as_mut().unwrap().connection_mut()?;
        alive(o.admission.deadline,cancel)?;
        o.preparation_attempted=true;
        o.preparation=Some(o.source.as_mut().unwrap().inner.prepare94(&mapping.profile,&mapping.native_source,&request.candidate,cancel));
        // Genuine preparation Result is retained before any postcheck await.
        let prep=o.preparation.as_ref().unwrap().value()?;
        ensure!(prep.candidate_root==journal::candidate_selector(&request.candidate)?,"owned94 candidate spelling changed");
        bounded_serialized(prep)?;
        o.source.as_ref().unwrap().inner.verify_pair_in(db).await?;
        o.prepared_intent=Some(journal::prepare_new_in(db,&request.operation,&request.expected,&requester.inner,prep).await?);
        o.prepared_intent.as_ref().unwrap().require_owned94_bound()?;
        ensure!(o.prepared_intent.as_ref().unwrap().retained_bytes()?<=OBJECT*8,"owned94 intent graph exceeds supported cohort");
        self.owner.refresh(cancel).await?; Ok(())
    }
    pub fn needs_insert(&self)->bool {match self.purpose {SourceRelocation94Purpose::Intent=>!self.owner.intent_saved,SourceRelocation94Purpose::Ack|SourceRelocation94Purpose::Reconcile=>self.owner.prepared_ack.as_ref().map(|p|!p.saved).unwrap_or(false),SourceRelocation94Purpose::Dispatch=>false}}
    /// App fences the poll, not just construction of this borrowed future.
    pub fn insert_intent<'a>(&'a mut self,cancel:&'a AtomicBool)->impl std::future::Future<Output=anyhow::Result<()>>+'a {
        let claim=(|| {
            ensure!(self.purpose==SourceRelocation94Purpose::Intent&&!self.owner.insert_attempted[self.purpose.index()]&&!self.owner.intent_saved,"owned94 intent insert cannot repeat");
            self.owner.guard.check_effect()?;
            alive(self.owner.deadline(),cancel)?;
            self.owner.insert_attempted[self.purpose.index()]=true;
            Ok::<(),anyhow::Error>(())
        })();
        async move {
            claim?;
            alive(self.owner.deadline(),cancel)?;
            let o=&mut *self.owner;
            o.insert_result[self.purpose.index()]=Some(journal::insert_prepared_intent_in(o.target.as_mut().unwrap().connection_mut()?,o.prepared_intent.as_ref().context("owned94 intent not prepared")?).await);
            retained_error(o.insert_result[self.purpose.index()].as_ref().unwrap())?;
            o.command=Some(o.prepared_intent.as_ref().unwrap().command.clone());
            o.phase=SourceRelocation94Phase::PendingIntentCommit; Ok(())
        }
    }
    pub fn poll_commit(&mut self,cx:&mut Context<'_>,cancel:&AtomicBool)->Poll<anyhow::Result<()>> {
        if let Err(e)=self.owner.guard.check_effect() {return Poll::Ready(Err(e));}
        if let Err(e)=alive(self.owner.deadline(),cancel) {return Poll::Ready(Err(e));}
        if !self.owner.insert_attempted[self.purpose.index()] || !matches!(self.owner.insert_result[self.purpose.index()],Some(Ok(()))) {return Poll::Ready(Err(anyhow::anyhow!("owned94 insertion incomplete")));}
        let purpose=self.purpose.fixed(); let target=self.owner.target.as_mut().unwrap();
        if target.commit_state(purpose)==SourceRelocation94CommitState::NotAttempted {if let Err(e)=target.claim_commit(purpose){return Poll::Ready(Err(e));}}
        // No await occurs between the host's dual fence and this first poll.
        match target.poll_commit_after_fence(cx) {Poll::Pending=>Poll::Pending,Poll::Ready(Err(e))=>Poll::Ready(Err(e)),Poll::Ready(Ok(()))=>{
            self.owner.completed=true; self.owner.phase=if self.purpose==SourceRelocation94Purpose::Intent {SourceRelocation94Phase::IntentSaved}else{SourceRelocation94Phase::AcknowledgedUnretired}; Poll::Ready(Ok(()))}}
    }
    pub fn finish_saved<'a>(&'a mut self,cancel:&'a AtomicBool)->impl std::future::Future<Output=anyhow::Result<()>>+'a {
        // Absence of an INSERT is not evidence of saved work. The same token
        // must have completed every preparation postcheck for this purpose.
        // Consume readiness before even an unpolled rollback waiter can drop.
        let prepared=std::mem::replace(&mut self.saved_ready,false);
        let claim=(|| {
            ensure!(self.began && self.owner.active==Some(self.purpose) && prepared
                && !self.owner.insert_attempted[self.purpose.index()],"owned94 saved path not proven");
            match self.purpose {
                SourceRelocation94Purpose::Intent=>ensure!(self.owner.intent_saved && self.owner.command.is_some(),"owned94 original saved intent absent"),
                SourceRelocation94Purpose::Ack|SourceRelocation94Purpose::Reconcile=>{
                    ensure!(self.owner.prepared_ack.as_ref().is_some_and(|p|p.saved),"owned94 prepared saved ACK absent");
                    let receipt=self.owner.receipt()?;
                    ensure!(self.owner.command.as_ref().is_some_and(|c|c.canonical_payload()==receipt.command().canonical_payload()),"owned94 saved ACK original receipt differs");
                }
                SourceRelocation94Purpose::Dispatch=>anyhow::bail!("owned94 Dispatch requires finish_dispatch"),
            }
            self.owner.guard.check_effect()?;
            alive(self.owner.deadline(),cancel)
        })();
        async move {
            claim?;
            self.owner.guard.check_effect()?;
            alive(self.owner.deadline(),cancel)?;
            self.owner.target.as_mut().unwrap().rollback(self.purpose.fixed()).await?;
            self.owner.guard.check_effect()?;
            alive(self.owner.deadline(),cancel)?;
            self.owner.completed=true; Ok(())
        }
    }
    pub async fn preflight_execute(&mut self,requester:&SourceRelocationRequester,cancel:&AtomicBool)->anyhow::Result<()> {
        if let Some(source)=self.owner.source.as_mut() { source.inner.invalidate_execute94(); }
        ensure!(self.purpose==SourceRelocation94Purpose::Dispatch&&!self.owner.dispatch_ready,"owned94 dispatch preflight cannot repeat");
        self.owner.refresh(cancel).await?;self.verify(cancel).await?;
        self.read_receipt(requester,cancel).await?;
        if self.owner.receipt_observed(){self.owner.dispatch_ready=true;return Ok(())}
        let o=&mut *self.owner;let db=o.target.as_mut().unwrap().connection_mut()?;
        let mut token=journal::load_in(db,&o.admission.request.operation,&requester.inner).await?.context("owned94 original intent absent")?;
        token.require_current_predecessor_in().await?;
        let source=token.source_id().to_owned();
        let db=token.writer_in();
        let scans:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_demands WHERE source_id=? AND (phase IN ('queued','running','cancelling') OR status='pending')").bind(&source).fetch_one(&mut *db).await?;
        let discovery:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_discovery_intents WHERE source_id=? AND command_json IS NOT NULL AND state<>'committed'").bind(&source).fetch_one(&mut *db).await?;
        let jobs:i64=sqlx::query_scalar("SELECT count(*) FROM processing_jobs WHERE phase IN ('queued','running','cancelling')").fetch_one(&mut *db).await?;
        let publications:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications WHERE command_json IS NOT NULL AND acknowledged=0").fetch_one(&mut *db).await?;
        ensure!(scans==0&&discovery==0&&jobs==0&&publications==0&&source!=crate::generated_source::SOURCE_ID,"Motion owners not idle or private source");
        // The full post-transfer capacity walk is preflight work. The app
        // subsequently refreshes both live auth tokens and performs its final
        // pure temporal fence; execute94 consumes this proof without a walk.
        o.source.as_mut().context("owned94 source closed")?.inner.preflight_execute94()?;
        o.dispatch_ready=true;alive(o.deadline(),cancel)
    }
    pub fn execute_authorized(&mut self,cancel:&AtomicBool)->anyhow::Result<()> {
        ensure!(self.purpose==SourceRelocation94Purpose::Dispatch&&self.owner.dispatch_ready&&self.owner.native_outcome.is_none()&&self.owner.phase!=SourceRelocation94Phase::DispatchAttempted,"owned94 native dispatch not admitted");
        alive(self.owner.deadline(),cancel)?;
        self.owner.guard.check_effect()?;
        if self.owner.receipt_observed(){return Ok(())}
        self.owner.phase=SourceRelocation94Phase::DispatchAttempted;
        let o=&mut *self.owner;
        o.native_outcome=Some(o.source.as_mut().unwrap().inner.execute94(o.command.as_ref().context("owned94 command absent")?,cancel));
        o.native_outcome.as_ref().unwrap().value()?;Ok(())
    }
    async fn read_receipt(&mut self,requester:&SourceRelocationRequester,cancel:&AtomicBool)->anyhow::Result<()> {
        ensure!(self.owner.receipt_attempted==self.owner.receipts.len()&&self.owner.receipts.len()<4,"owned94 fixed receipt slots exhausted or uncertain");
        alive(self.owner.deadline(),cancel)?;
        let o=&mut *self.owner;let db=o.target.as_mut().unwrap().connection_mut()?;
        journal::require_owned94_record_bound(db,&o.admission.request.operation).await?;
        let mut token=journal::load_in(db,&o.admission.request.operation,&requester.inner).await?.context("owned94 original intent absent")?;
        token.validate_writer_in().await?;
        let command=token.command().clone();
        if let Some(original)=&o.command {ensure!(original.canonical_payload()==command.canonical_payload(),"owned94 exact original command changed");}else{o.command=Some(command.clone());}
        // Install actual receipt/error BEFORE validation or pair postcheck await.
        o.receipt_attempted+=1;
        o.receipts.push(o.source.as_mut().unwrap().inner.receipt94(&command));
        o.receipts.last().unwrap().value()?;
        token.validate_writer_in().await?;
        o.source.as_ref().unwrap().inner.verify_pair_in(token.writer_in()).await?;
        Ok(())
    }
    pub async fn observe_receipt(&mut self,requester:&SourceRelocationRequester,cancel:&AtomicBool)->anyhow::Result<()> {
        ensure!(self.purpose==SourceRelocation94Purpose::Dispatch&&self.owner.dispatch_ready,"owned94 receipt observation wrong phase");
        self.read_receipt(requester,cancel).await?;self.owner.receipt()?;self.owner.refresh(cancel).await?;Ok(())
    }
    pub async fn finish_dispatch(&mut self,cancel:&AtomicBool)->anyhow::Result<()> {
        ensure!(self.purpose==SourceRelocation94Purpose::Dispatch&&self.owner.receipt_observed(),"owned94 permanent receipt absent");
        alive(self.owner.deadline(),cancel)?;
        self.owner.target.as_mut().unwrap().rollback(Purpose::Dispatch).await?;
        // Real same-handle positive autocommit observation is part of rollback.
        self.owner.phase=SourceRelocation94Phase::ReceiptPendingUnretired;self.owner.completed=true;Ok(())
    }
    pub async fn prepare_ack(&mut self,requester:&SourceRelocationRequester,cancel:&AtomicBool)->anyhow::Result<()> {
        ensure!(matches!(self.purpose,SourceRelocation94Purpose::Ack|SourceRelocation94Purpose::Reconcile)&&self.owner.prepared_ack.is_none(),"owned94 ACK preparation cannot repeat");
        self.owner.refresh(cancel).await?;self.verify(cancel).await?;
        // Reconcile only rereads original journal/requester and permanent receipt.
        // It cannot prepare a candidate or enter execute_authorized.
        self.read_receipt(requester,cancel).await?;
        let o=&mut *self.owner;
        let receipt=o.receipts.last().and_then(|r|r.result().ok()).and_then(|r|r.as_ref()).context("owned94 ACK requires permanent receipt")?;
        if let Some(prior)=o.receipts.iter().rev().skip(1).find_map(|r|r.result().ok().and_then(|r|r.as_ref())) { super::facade::require_same_receipt_effect(receipt,prior.effect_description())?; }
        o.prepared_ack=Some(journal::prepare_ack_in(o.target.as_mut().unwrap().connection_mut()?,&o.admission.request.operation,&requester.inner,receipt).await?);
        ensure!(o.prepared_ack.as_ref().unwrap().retained_bytes()?<=OBJECT*8,"owned94 ACK graph exceeds supported cohort");
        self.owner.refresh(cancel).await?;
        self.saved_ready=self.owner.prepared_ack.as_ref().unwrap().saved;Ok(())
    }
    pub fn insert_ack<'a>(&'a mut self,cancel:&'a AtomicBool)->impl std::future::Future<Output=anyhow::Result<()>>+'a {
        let claim=(|| {
            ensure!(matches!(self.purpose,SourceRelocation94Purpose::Ack|SourceRelocation94Purpose::Reconcile)&&!self.owner.insert_attempted[self.purpose.index()]&&self.needs_insert(),"owned94 ACK insertion cannot repeat");
            self.owner.guard.check_effect()?;
            alive(self.owner.deadline(),cancel)?;self.owner.insert_attempted[self.purpose.index()]=true;
            Ok::<(),anyhow::Error>(())
        })();
        async move {
            claim?;
            alive(self.owner.deadline(),cancel)?;
            let o=&mut *self.owner;
            o.insert_result[self.purpose.index()]=Some(journal::insert_prepared_ack_in(o.target.as_mut().unwrap().connection_mut()?,o.prepared_ack.as_ref().unwrap()).await);
            retained_error(o.insert_result[self.purpose.index()].as_ref().unwrap())?;
            o.phase=SourceRelocation94Phase::PendingAckCommit;Ok(())
        }
    }
}

// Capacity-aware retained graph accounting, separate from precharged scratch.
fn value_bytes(v:&serde_json::Value)->anyhow::Result<u64> {
    let own=1024u64;
    match v {serde_json::Value::String(s)=>add(&[own,s.capacity() as u64]),serde_json::Value::Array(a)=>{let mut n=add(&[own,(a.capacity()*std::mem::size_of::<serde_json::Value>()) as u64])?;for v in a {n=add(&[n,value_bytes(v)?])?;}Ok(n)},serde_json::Value::Object(m)=>{let mut n=own;for(k,v)in m {n=add(&[n,1024,k.capacity() as u64,value_bytes(v)?])?;}Ok(n)},_=>Ok(own)}
}
pub(super) fn proof_bytes(p:&LifecycleProof)->anyhow::Result<u64> {add(&[std::mem::size_of_val(p) as u64,p.source_id.capacity() as u64,p.catalog_id.capacity() as u64,p.incarnation.capacity() as u64,p.mapping_digest.capacity() as u64])}
pub(super) fn mapping_bytes(m:&SourceMapping)->anyhow::Result<u64> {
    let mut n=add(&[std::mem::size_of_val(m) as u64,m.library_id.capacity() as u64,m.profile.capacity() as u64,m.native_source.capacity() as u64,m.catalog.catalog_id.capacity() as u64,m.catalog.incarnation.capacity() as u64,m.motion_root.capacity() as u64,m.motion_root_identity.capacity() as u64,value_bytes(&m.native_binding)?,value_bytes(&m.native_policy)?,(m.exclusions.capacity()*std::mem::size_of::<String>()) as u64])?;
    for e in &m.exclusions {n=add(&[n,e.capacity() as u64])?;}Ok(n)
}
pub(super) fn command_bytes(c:&CommandEnvelope)->anyhow::Result<u64> {let mut n=value_bytes(c.payload())?;for s in [c.canonical_payload(),c.key().catalog_id(),c.key().host_namespace(),c.key().operation_id(),c.admission_incarnation(),c.digest().as_str()] {n=add(&[n,(s.len() as u64).checked_mul(2).context("owned94 command overflow")?])?;}Ok(n)}
fn bounded_serialized(value:&impl serde::Serialize)->anyhow::Result<()> {
    struct Count(u64);impl std::io::Write for Count {fn write(&mut self,b:&[u8])->std::io::Result<usize>{self.0=self.0.checked_add(b.len() as u64).filter(|n|*n<=OBJECT).ok_or_else(||std::io::Error::other("owned94 encoded cohort"))?;Ok(b.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
    serde_json::to_writer(&mut Count(0),value)?;Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn proof()->LifecycleProof { LifecycleProof {source_id:"source".into(),epoch:0,catalog_id:"catalog".into(),incarnation:"incarnation".into(),mapping_digest:"digest".into(),source_revision:1,binding_revision:1,state:playscale_core::source_lifecycle::State::Active} }
    #[test]
    fn request_is_bounded_lexical_description_not_native_authority() {
        assert!(SourceRelocation94Request::relocate("op".into(),proof(),PathBuf::from("/not-required-to-exist")).is_ok());
        assert!(SourceRelocation94Request::relocate("op".into(),proof(),PathBuf::from("relative")).is_err());
        assert!(SourceRelocation94Request::relocate("op".into(),proof(),PathBuf::from("/a/../b")).is_err());
        assert!(SourceRelocation94Request::relocate("x".repeat(1025),proof(),PathBuf::from("/a")).is_err());
    }
    #[test]
    fn original_deadline_and_actual_cancel_are_independent_refusals() {
        let cancel=AtomicBool::new(false);
        assert!(alive(Instant::now(),&cancel).is_err());
        let deadline=Instant::now()+std::time::Duration::from_secs(1);
        assert!(alive(deadline,&cancel).is_ok());
        cancel.store(true,Ordering::Release);
        assert!(alive(deadline,&cancel).is_err());
    }
}
