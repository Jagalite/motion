//! One retained SQLx owner, with a finite set of fixed transaction statements.
//!
//! No `Transaction` is used: SQLx's explicit Execute path does not maintain its
//! transaction-manager depth. Resource settlement instead leases this exact
//! connection's serialized handle and observes SQLite autocommit/main state.
//! Completed frames and typed results remain here. Dropping a borrowed waiter
//! does not drop a connection, an opening/statement/close frame, or its result.
//!
//! COMMIT has a concrete, recoverable never-polled state. The application must
//! retain both real authentication snapshots and perform its final temporal,
//! deadline and cancellation fence immediately before `poll_commit_after_fence`
//! in the SAME poll closure. That synchronous method invokes the first poll,
//! not merely an async constructor. This does not authenticate SQLx's later
//! worker-thread execution, nor turn a Started marker into authorization.

use std::{
    any::Any,
    future::{Future, poll_fn},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    pin::{Pin, pin},
    task::{Context, Poll},
};

use anyhow::{anyhow, bail, ensure};
use sqlx::{Connection, SqliteConnection, sqlite::{SqliteConnectOptions, SqliteQueryResult}};

type PanicPayload = Box<dyn Any + Send + 'static>;
type OpeningFrame = Pin<Box<dyn Future<Output = Result<SqliteConnection, sqlx::Error>> + Send>>;
type CloseFrame = Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send>>;
type OwnedFrame<T> = Pin<Box<dyn Future<Output = (SqliteConnection, Outcome<T>)> + Send>>;

/// These are distinct, one-shot transaction purposes, not a retry counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Purpose {
    Intent,
    Dispatch,
    Ack,
    Reconcile,
}

impl Purpose {
    fn index(self) -> usize {
        match self {
            Self::Intent => 0,
            Self::Dispatch => 1,
            Self::Ack => 2,
            Self::Reconcile => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitState {
    NotAttempted,
    ClaimedNeverPolled,
    Started,
    ReturnedOk,
    ReturnedErr,
    PanickedUncertain,
    RunnerPanickedUncertain,
    AbandonedNeverPolled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Statement {
    Begin,
    Commit,
    Rollback,
}

impl Statement {
    fn sql(self) -> &'static str {
        match self {
            Self::Begin => "BEGIN IMMEDIATE",
            Self::Commit => "COMMIT",
            Self::Rollback => "ROLLBACK",
        }
    }

    fn offset(self) -> usize {
        match self {
            Self::Begin => 0,
            Self::Commit => 1,
            Self::Rollback => 2,
        }
    }
}

const PURPOSE_SLOTS: usize = 4 * 3;
const TERMINAL_ROLLBACK: usize = PURPOSE_SLOTS;
const STATEMENT_SLOTS: usize = PURPOSE_SLOTS + 1;

/// A returned SQL error and a panic payload are retained without stringifying
/// or substituting them. Summary errors returned to waiters do not own them.
enum Outcome<T> {
    Returned(Result<T, sqlx::Error>),
    Panicked(PanicPayload),
}

struct OwnedAttempt<T> {
    frame: OwnedFrame<T>,
    outcome: Option<Outcome<T>>,
    runner_panic: Option<PanicPayload>,
    started: bool,
}

impl<T> OwnedAttempt<T> {
    fn new(frame: OwnedFrame<T>) -> Self {
        Self { frame, outcome: None, runner_panic: None, started: false }
    }

    fn state(&self) -> CommitState {
        if self.runner_panic.is_some() {
            CommitState::RunnerPanickedUncertain
        } else {
            match &self.outcome {
                Some(Outcome::Returned(Ok(_))) => CommitState::ReturnedOk,
                Some(Outcome::Returned(Err(_))) => CommitState::ReturnedErr,
                Some(Outcome::Panicked(_)) => CommitState::PanickedUncertain,
                None if self.started => CommitState::Started,
                None => CommitState::ClaimedNeverPolled,
            }
        }
    }

    fn result(&self) -> anyhow::Result<()> {
        match self.state() {
            CommitState::ReturnedOk => Ok(()),
            CommitState::ReturnedErr => bail!("retained SQLx operation returned an error"),
            CommitState::PanickedUncertain => bail!("retained borrowed SQLx operation panicked; outcome is uncertain"),
            CommitState::RunnerPanickedUncertain => bail!("retained SQLx runner panicked; custody and outcome are uncertain"),
            _ => bail!("retained SQLx operation has not returned"),
        }
    }

    fn poll(&mut self, connection: &mut Option<SqliteConnection>, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.outcome.is_some() || self.runner_panic.is_some() {
            return Poll::Ready(self.result());
        }
        // The marker is written inside the actual polling entry, immediately
        // before the effect-capable frame poll, never at frame construction.
        self.started = true;
        match catch_unwind(AssertUnwindSafe(|| self.frame.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready((returned, outcome))) => {
                // Reinstall the actual owner BEFORE inspecting its outcome,
                // formatting an error, or making another fallible call.
                *connection = Some(returned);
                self.outcome = Some(outcome);
                Poll::Ready(self.result())
            }
            Err(payload) => {
                // The outer frame might have unwound its owned connection.
                // Retain the frame and payload, never repoll, never claim that
                // the raw connection survived this distinct failure boundary.
                self.runner_panic = Some(payload);
                Poll::Ready(self.result())
            }
        }
    }
}

struct StatementSlot {
    statement: Statement,
    state: CommitState,
    attempt: Option<OwnedAttempt<SqliteQueryResult>>,
}

impl StatementSlot {
    fn new(index: usize) -> Self {
        let statement = if index == TERMINAL_ROLLBACK {
            Statement::Rollback
        } else {
            match index % 3 {
                0 => Statement::Begin,
                1 => Statement::Commit,
                _ => Statement::Rollback,
            }
        };
        Self { statement, state: CommitState::NotAttempted, attempt: None }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransactionObservation {
    autocommit: bool,
    main_state: i32,
}

impl TransactionObservation {
    fn ended(self) -> bool {
        self.autocommit && self.main_state == 0
    }
}

/// Private to the finite owned relocation operation. In particular no caller
/// receives this connection or an arbitrary SQL/callback runner.
pub(super) struct RetainedConnection {
    connection: Option<SqliteConnection>,
    opening: OpeningFrame,
    open_outcome: Option<Outcome<()>>,
    open_runner_panic: Option<PanicPayload>,
    configuration: Option<OwnedAttempt<()>>,
    statements: [StatementSlot; STATEMENT_SLOTS],
    active: Option<usize>,
    rollback_inspection: [Option<OwnedAttempt<TransactionObservation>>; 4],
    pending_rollback_inspection: Option<usize>,
    inspection: [Option<OwnedAttempt<TransactionObservation>>; 2],
    settlement_requested: bool,
    transaction_settled: bool,
    close_frame: Option<CloseFrame>,
    close_attempted: bool,
    close_outcome: Option<Outcome<()>>,
    path_capacity: usize,
}

impl RetainedConnection {
    /// Call only after the enclosing whole owner has admitted the actual path,
    /// opening/configuration/statement/close frames and SQLx allocation overlap.
    /// Construction installs, but does not poll, the single opening frame.
    pub(super) fn opening(path: PathBuf) -> Self {
        let path_capacity = path.capacity();
        let opening = Box::pin(opening_future(path));
        Self {
            connection: None,
            opening,
            open_outcome: None,
            open_runner_panic: None,
            configuration: None,
            statements: std::array::from_fn(StatementSlot::new),
            active: None,
            rollback_inspection: std::array::from_fn(|_| None),
            pending_rollback_inspection: None,
            inspection: std::array::from_fn(|_| None),
            settlement_requested: false,
            transaction_settled: false,
            close_frame: None,
            close_attempted: false,
            close_outcome: None,
            path_capacity,
        }
    }

    fn poll_open_raw(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.open_runner_panic.is_some() {
            return Poll::Ready(Err(anyhow!("retained SQLx opening panicked; acquisition is uncertain")));
        }
        if let Some(outcome) = &self.open_outcome {
            return Poll::Ready(match outcome {
                Outcome::Returned(Ok(())) => Ok(()),
                _ => Err(anyhow!("retained SQLx opening failed")),
            });
        }
        match catch_unwind(AssertUnwindSafe(|| self.opening.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(Ok(connection))) => {
                self.connection = Some(connection);
                self.open_outcome = Some(Outcome::Returned(Ok(())));
                Poll::Ready(Ok(()))
            }
            Ok(Poll::Ready(Err(error))) => {
                // SQLx connect owns its connection through initial PRAGMAs;
                // an error does not return that connection. Do not invent one.
                self.open_outcome = Some(Outcome::Returned(Err(error)));
                Poll::Ready(Err(anyhow!("retained SQLx opening returned an error")))
            }
            Err(payload) => {
                self.open_runner_panic = Some(payload);
                Poll::Ready(Err(anyhow!("retained SQLx opening panicked; acquisition is uncertain")))
            }
        }
    }

    pub(super) async fn wait_open(&mut self) -> anyhow::Result<()> {
        ensure!(!self.settlement_requested, "target connection is settling");
        poll_fn(|cx| self.poll_open_raw(cx)).await?;
        if self.configuration.is_none() {
            let connection = self.connection.take().ok_or_else(|| anyhow!("opened connection is not retained"))?;
            self.configuration = Some(OwnedAttempt::new(configure_frame(connection)));
        }
        poll_fn(|cx| {
            self.configuration.as_mut().expect("configuration installed")
                .poll(&mut self.connection, cx)
        }).await
    }

    pub(super) fn connection_mut(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        ensure!(!self.settlement_requested && self.close_frame.is_none(), "target connection is terminal");
        ensure!(self.active.is_none(), "target statement has retained custody");
        ensure!(self.pending_rollback_inspection.is_none(), "target rollback observation has retained custody");
        ensure!(self.configuration.as_ref().is_some_and(|attempt| attempt.state() == CommitState::ReturnedOk), "target connection configuration has not succeeded");
        self.connection.as_mut().ok_or_else(|| anyhow!("same target connection is not available"))
    }

    fn claim(&mut self, slot: usize, terminal: bool) -> anyhow::Result<()> {
        ensure!(terminal || !self.settlement_requested, "target connection is terminal");
        ensure!(self.close_frame.is_none(), "target connection close was installed");
        ensure!(self.active.is_none() && self.connection.is_some(), "same target connection is not idle");
        ensure!(self.pending_rollback_inspection.is_none(), "target rollback observation has retained custody");
        ensure!(terminal || self.configuration.as_ref().is_some_and(|attempt| attempt.state() == CommitState::ReturnedOk), "target connection configuration has not succeeded");
        ensure!(self.statements[slot].state == CommitState::NotAttempted, "fixed statement slot has already been claimed");
        // Concrete pre-poll state: the connection is still recoverable here,
        // not inside an opaque async frame that cleanup must poll to recover.
        self.statements[slot].state = CommitState::ClaimedNeverPolled;
        self.active = Some(slot);
        Ok(())
    }

    fn poll_statement(&mut self, slot: usize, allow_first: bool, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.statements[slot].state == CommitState::ClaimedNeverPolled {
            if !allow_first {
                return Poll::Ready(Err(anyhow!("cleanup cannot first-poll a claimed statement")));
            }
            if self.active != Some(slot) {
                return Poll::Ready(Err(anyhow!("fixed statement is not the active claim")));
            }
            let Some(connection) = self.connection.take() else {
                return Poll::Ready(Err(anyhow!("claimed statement lost its same connection")));
            };
            let statement = self.statements[slot].statement;
            self.statements[slot].attempt = Some(OwnedAttempt::new(statement_frame(connection, statement)));
        }
        let Some(attempt) = self.statements[slot].attempt.as_mut() else {
            return Poll::Ready(Err(anyhow!("fixed statement was not started")));
        };
        let result = attempt.poll(&mut self.connection, cx);
        self.statements[slot].state = attempt.state();
        if result.is_ready() && self.active == Some(slot) {
            self.active = None;
        }
        result
    }

    /// Claims before returning its borrowed waiter. Dropping an unpolled waiter
    /// leaves the concrete claim and original connection in this owner.
    pub(super) fn begin(&mut self, purpose: Purpose) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let slot = purpose.index() * 3 + Statement::Begin.offset();
        let claim = self.claim(slot, false);
        async move {
            claim?;
            poll_fn(|cx| self.poll_statement(slot, true, cx)).await
        }
    }

    pub(super) fn claim_commit(&mut self, purpose: Purpose) -> anyhow::Result<()> {
        self.claim(purpose.index() * 3 + Statement::Commit.offset(), false)
    }

    /// AUTHORIZATION BOUNDARY: must be called immediately after the host's final
    /// fence, with the actual verified authentication owners still retained.
    /// The very first SQLx statement poll occurs inline before this returns.
    /// No cleanup API can call this method or first-poll a never-started COMMIT.
    pub(super) fn poll_commit_after_fence(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.settlement_requested {
            return Poll::Ready(Err(anyhow!("cannot authorize COMMIT after terminal settlement began")));
        }
        let Some(slot) = self.active else {
            return Poll::Ready(Err(anyhow!("no retained COMMIT is active")));
        };
        if self.statements[slot].statement != Statement::Commit {
            return Poll::Ready(Err(anyhow!("active retained statement is not COMMIT")));
        }
        self.poll_statement(slot, true, cx)
    }

    pub(super) fn rollback(&mut self, purpose: Purpose) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let slot = purpose.index() * 3 + Statement::Rollback.offset();
        // Re-entering this finite method resumes its exact existing frame and
        // observation; it never reconstructs or reissues a ROLLBACK.
        let claim = if self.statements[slot].state == CommitState::NotAttempted {
            self.claim(slot, false)
        } else if self.settlement_requested
            || self.active.is_some_and(|active| active != slot)
            || self.pending_rollback_inspection.is_some_and(|index| index != purpose.index())
            || self.statements[slot].state == CommitState::AbandonedNeverPolled
            || self.rollback_inspection[purpose.index()].as_ref()
                .is_some_and(|attempt| attempt.outcome.is_some() || attempt.runner_panic.is_some())
        {
            Err(anyhow!("fixed rollback cannot resume in this state"))
        } else {
            Ok(())
        };
        async move {
            claim?;
            poll_fn(|cx| self.poll_statement(slot, true, cx)).await?;
            let observation = self.inspect_rollback(purpose.index()).await?;
            ensure!(observation.ended(), "actual target transaction did not end after ROLLBACK");
            Ok(())
        }
    }

    pub(super) fn commit_state(&self, purpose: Purpose) -> CommitState {
        self.statements[purpose.index() * 3 + Statement::Commit.offset()].state
    }

    /// Exact successful normal rollback plus its positive same-handle ended
    /// observation. A missing frame or terminal autocommit is not a saved-path
    /// completion, and this never reclassifies an uncertain COMMIT.
    pub(super) fn rollback_completed(&self, purpose: Purpose) -> bool {
        self.statements[purpose.index() * 3 + Statement::Rollback.offset()].state == CommitState::ReturnedOk
            && self.rollback_inspection[purpose.index()].as_ref().is_some_and(|attempt| {
                attempt.runner_panic.is_none()
                    && matches!(&attempt.outcome, Some(Outcome::Returned(Ok(observation))) if observation.ended())
            })
    }

    async fn inspect_rollback(&mut self, index: usize) -> anyhow::Result<TransactionObservation> {
        if self.rollback_inspection[index].is_none() {
            let connection = self.connection.take().ok_or_else(|| anyhow!("cannot inspect absent rollback connection"))?;
            self.pending_rollback_inspection = Some(index);
            self.rollback_inspection[index] = Some(OwnedAttempt::new(inspect_frame(connection)));
        }
        let result = poll_fn(|cx| self.rollback_inspection[index].as_mut().expect("rollback inspection installed")
            .poll(&mut self.connection, cx)).await;
        // The actual observation/error and returned connection are installed
        // before the awaiting method can report, format or inspect its result.
        if self.pending_rollback_inspection == Some(index) {
            self.pending_rollback_inspection = None;
        }
        result?;
        match &self.rollback_inspection[index].as_ref().expect("rollback inspection retained").outcome {
            Some(Outcome::Returned(Ok(observation))) => Ok(*observation),
            _ => bail!("actual rollback transaction observation is unavailable"),
        }
    }

    async fn inspect(&mut self, slot: usize) -> anyhow::Result<TransactionObservation> {
        if self.inspection[slot].is_none() {
            let connection = self.connection.take().ok_or_else(|| anyhow!("cannot inspect absent target connection"))?;
            self.inspection[slot] = Some(OwnedAttempt::new(inspect_frame(connection)));
        }
        poll_fn(|cx| self.inspection[slot].as_mut().expect("inspection installed")
            .poll(&mut self.connection, cx)).await?;
        match &self.inspection[slot].as_ref().expect("inspection retained").outcome {
            Some(Outcome::Returned(Ok(observation))) => Ok(*observation),
            _ => bail!("actual target transaction observation is unavailable"),
        }
    }

    /// Terminal resource settlement, not authorization recovery or a durability
    /// verdict. ReturnedErr/Panicked COMMIT remains uncertain even if SQLite is
    /// now in autocommit. No purpose slot, deadline or authorization is reset.
    pub(super) fn settle(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.settlement_requested = true;
        async move {
            if self.transaction_settled {
                return Ok(());
            }
            poll_fn(|cx| self.poll_open_raw(cx)).await?;
            if let Some(configuration) = &mut self.configuration {
                // Finish only the already-installed configuration frame. Its
                // failure is retained; an available connection can still be
                // inspected/rolled back, but never made usable again.
                let _ = poll_fn(|cx| configuration.poll(&mut self.connection, cx)).await;
            }
            if let Some(slot) = self.active {
                if self.statements[slot].state == CommitState::ClaimedNeverPolled {
                    self.statements[slot].state = CommitState::AbandonedNeverPolled;
                    self.active = None;
                } else {
                    // Only the exact original started runner is resumed.
                    let _ = poll_fn(|cx| self.poll_statement(slot, false, cx)).await;
                }
            }
            if let Some(index) = self.pending_rollback_inspection {
                // A dropped normal rollback waiter can leave its exact owner
                // in this nonterminal observation. Finish that installed frame
                // before creating the independent terminal observation.
                let _ = self.inspect_rollback(index).await;
            }
            // The connection may already be inside this exact inspection
            // frame after an earlier waiter was cancelled. inspect resumes
            // that frame instead of requiring a duplicate idle owner.
            let before = self.inspect(0).await?;
            if before.ended() {
                self.transaction_settled = true;
                return Ok(());
            }
            ensure!(!before.autocommit && (0..=2).contains(&before.main_state), "actual target transaction state is unsupported or uncertain");
            if self.statements[TERMINAL_ROLLBACK].state == CommitState::NotAttempted {
                self.claim(TERMINAL_ROLLBACK, true)?;
            }
            // Unlike COMMIT this is the sole finite terminal rollback. If a
            // previous waiter disappeared, resume, never reissue, its frame.
            let _ = poll_fn(|cx| self.poll_statement(TERMINAL_ROLLBACK, true, cx)).await;
            let after = self.inspect(1).await?;
            ensure!(after.ended(), "actual target transaction did not end");
            self.transaction_settled = true;
            Ok(())
        }
    }

    pub(super) fn settled(&self) -> bool {
        self.transaction_settled
    }

    /// Installs the consuming SQLx close before returning a waiter. SQLx drops
    /// its raw connection before shutdown ACK; an error or panic is terminal
    /// uncertainty, never evidence that a reusable connection survived.
    pub(super) fn close(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let prepared = if !self.transaction_settled {
            Err(anyhow!("target connection transaction has not been positively settled"))
        } else if self.close_frame.is_some() {
            Ok(())
        } else if let Some(connection) = self.connection.take() {
            self.close_frame = Some(Box::pin(close_future(connection)));
            Ok(())
        } else {
            Err(anyhow!("same target connection is unavailable for close"))
        };
        async move {
            prepared?;
            poll_fn(|cx| self.poll_close(cx)).await
        }
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if let Some(outcome) = &self.close_outcome {
            return Poll::Ready(match outcome {
                Outcome::Returned(Ok(())) => Ok(()),
                _ => Err(anyhow!("target close failed or panicked; release is uncertain")),
            });
        }
        let Some(frame) = &mut self.close_frame else {
            return Poll::Ready(Err(anyhow!("target close was not installed")));
        };
        self.close_attempted = true;
        match catch_unwind(AssertUnwindSafe(|| frame.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(result)) => {
                self.close_outcome = Some(Outcome::Returned(result));
                Poll::Ready(if self.closed() { Ok(()) } else { Err(anyhow!("target close returned an error; release is uncertain")) })
            }
            Err(payload) => {
                self.close_outcome = Some(Outcome::Panicked(payload));
                Poll::Ready(Err(anyhow!("target close panicked; release is uncertain")))
            }
        }
    }

    pub(super) fn closed(&self) -> bool {
        matches!(self.close_outcome, Some(Outcome::Returned(Ok(()))))
    }

    /// Exact direct owner/frame storage for every finite slot, before any frame
    /// is allocated. No constructor or SQL operation is invoked to measure it.
    /// This deliberately excludes opaque nested SQLx allocations and all
    /// dynamic payload capacities; it is not a standalone H/K admission proof.
    /// The supported capture-ingress cohort has no opaque retained error or
    /// panic graph. Those owners remain terminally retained, not undercounted.
    pub(super) fn capture_ingress_history_clean(&self) -> bool {
        let clean = |attempt: &OwnedAttempt<()>| {
            attempt.runner_panic.is_none()
                && matches!(attempt.outcome, Some(Outcome::Returned(Ok(()))))
        };
        self.settled() && self.closed()
            && self.open_runner_panic.is_none()
            && matches!(self.open_outcome, Some(Outcome::Returned(Ok(()))))
            && self.configuration.as_ref().is_some_and(clean)
            && self.statements.iter().all(|slot| slot.attempt.as_ref().is_none_or(|attempt|
                attempt.runner_panic.is_none()
                    && matches!(attempt.outcome, Some(Outcome::Returned(Ok(_))))))
            && self.inspection.iter().chain(self.rollback_inspection.iter())
                .flatten().all(|attempt| attempt.runner_panic.is_none()
                    && matches!(attempt.outcome, Some(Outcome::Returned(Ok(_)))))
    }

    pub(super) fn fixed_frame_bytes() -> Option<usize> {
        std::mem::size_of::<Self>()
            .checked_add(one_argument_future_size(opening_future))?
            .checked_add(one_argument_future_size(configure_future))?
            .checked_add(two_argument_future_size(statement_future).checked_mul(STATEMENT_SLOTS)?)?
            .checked_add(one_argument_future_size(inspect_future).checked_mul(6)?)?
            .checked_add(one_argument_future_size(close_future))
    }

    /// Exact direct frame allocation sizes plus the original path capacity.
    /// NOT an aggregate admission amount: SQLx's opaque options/maps/channels,
    /// DBSTATUS, error payloads and nested futures need their own admitted
    /// accounting. The enclosing owner must never treat this as budget fit.
    pub(super) fn direct_frame_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>() + self.path_capacity
            + std::mem::size_of_val(self.opening.as_ref().get_ref());
        if let Some(attempt) = &self.configuration {
            bytes += std::mem::size_of_val(attempt.frame.as_ref().get_ref());
        }
        for slot in &self.statements {
            if let Some(attempt) = &slot.attempt {
                bytes += std::mem::size_of_val(attempt.frame.as_ref().get_ref());
            }
        }
        for attempt in self.inspection.iter().flatten() {
            bytes += std::mem::size_of_val(attempt.frame.as_ref().get_ref());
        }
        for attempt in self.rollback_inspection.iter().flatten() {
            bytes += std::mem::size_of_val(attempt.frame.as_ref().get_ref());
        }
        if let Some(frame) = &self.close_frame {
            bytes += std::mem::size_of_val(frame.as_ref().get_ref());
        }
        bytes
    }
}

// These helpers inspect return TYPES only. They do not invoke the supplied
// private function item, allocate its frame, or accept a caller callback.
fn one_argument_future_size<A, F, R>(_: F) -> usize
where F: FnOnce(A) -> R, R: Future {
    std::mem::size_of::<R>()
}

fn two_argument_future_size<A, B, F, R>(_: F) -> usize
where F: FnOnce(A, B) -> R, R: Future {
    std::mem::size_of::<R>()
}

async fn opening_future(path: PathBuf) -> Result<SqliteConnection, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        // Finite sequential protocol: no cached statement graph or large
        // queued result/command cohort is needed by this dedicated owner.
        .statement_cache_capacity(0)
        .command_buffer_size(1)
        .row_buffer_size(1)
        .foreign_keys(true);
    SqliteConnection::connect_with(&options).await
}

async fn close_future(connection: SqliteConnection) -> Result<(), sqlx::Error> {
    connection.close().await
}

fn statement_frame(connection: SqliteConnection, statement: Statement) -> OwnedFrame<SqliteQueryResult> {
    Box::pin(statement_future(connection, statement))
}

async fn statement_future(mut connection: SqliteConnection, statement: Statement) -> (SqliteConnection, Outcome<SqliteQueryResult>) {
        let outcome = {
            // This nested future only BORROWS the outer connection. An unwind
            // from its poll is caught without moving the connection into it.
            let mut borrowed = pin!(sqlx::query(statement.sql()).execute(&mut connection));
            poll_fn(|cx| match catch_unwind(AssertUnwindSafe(|| borrowed.as_mut().poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(result)) => Poll::Ready(Outcome::Returned(result)),
                Err(payload) => Poll::Ready(Outcome::Panicked(payload)),
            }).await
        };
        (connection, outcome)
}

fn configure_frame(connection: SqliteConnection) -> OwnedFrame<()> {
    Box::pin(configure_future(connection))
}

async fn configure_future(mut connection: SqliteConnection) -> (SqliteConnection, Outcome<()>) {
        let outcome = {
            let mut borrowed = pin!(crate::projection::configure_connection(&mut connection));
            poll_fn(|cx| match catch_unwind(AssertUnwindSafe(|| borrowed.as_mut().poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(result)) => Poll::Ready(Outcome::Returned(result)),
                Err(payload) => Poll::Ready(Outcome::Panicked(payload)),
            }).await
        };
        (connection, outcome)
}

fn inspect_frame(connection: SqliteConnection) -> OwnedFrame<TransactionObservation> {
    Box::pin(inspect_future(connection))
}

async fn inspect_future(mut connection: SqliteConnection) -> (SqliteConnection, Outcome<TransactionObservation>) {
        let outcome = {
            let borrowed = async {
                let mut lease = connection.lock_handle().await?;
                let raw = lease.as_raw_handle().as_ptr();
                // SAFETY: the serialized SQLx lease owns the worker lock for
                // this exact live connection, through both read-only calls.
                let observation = TransactionObservation {
                    autocommit: unsafe { rusqlite::ffi::sqlite3_get_autocommit(raw) } != 0,
                    main_state: unsafe { rusqlite::ffi::sqlite3_txn_state(raw, c"main".as_ptr()) },
                };
                Ok(observation)
            };
            let mut borrowed = pin!(borrowed);
            poll_fn(|cx| match catch_unwind(AssertUnwindSafe(|| borrowed.as_mut().poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(result)) => Poll::Ready(Outcome::Returned(result)),
                Err(payload) => Poll::Ready(Outcome::Panicked(payload)),
            }).await
        };
        (connection, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn owner() -> (tempfile::NamedTempFile, RetainedConnection) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut owner = RetainedConnection::opening(file.path().to_owned());
        owner.wait_open().await.unwrap();
        (file, owner)
    }

    async fn identity(owner: &mut RetainedConnection) -> usize {
        let mut lease = owner.connection_mut().unwrap().lock_handle().await.unwrap();
        lease.as_raw_handle().as_ptr() as usize
    }

    #[tokio::test]
    async fn returned_statement_error_reinstalls_the_same_connection() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        owner.begin(Purpose::Intent).await.unwrap();
        // Nested BEGIN is deliberately a fixed-SQL error, not a new connection.
        assert!(owner.begin(Purpose::Dispatch).await.is_err());
        assert_eq!(identity(&mut owner).await, original);
        let slot = Purpose::Dispatch.index() * 3;
        assert!(matches!(owner.statements[slot].attempt.as_ref().unwrap().outcome,
            Some(Outcome::Returned(Err(_)))));
        owner.rollback(Purpose::Intent).await.unwrap();
        assert_eq!(identity(&mut owner).await, original);
        owner.settle().await.unwrap();
        owner.close().await.unwrap();
        assert!(owner.closed());
    }

    #[tokio::test]
    async fn cleanup_never_first_polls_claimed_commit() {
        let (file, mut owner) = owner().await;
        sqlx::query("CREATE TABLE retained_commit_probe(value INTEGER)")
            .execute(owner.connection_mut().unwrap()).await.unwrap();
        owner.begin(Purpose::Intent).await.unwrap();
        sqlx::query("INSERT INTO retained_commit_probe VALUES (1)")
            .execute(owner.connection_mut().unwrap()).await.unwrap();
        owner.claim_commit(Purpose::Intent).unwrap();
        assert_eq!(owner.commit_state(Purpose::Intent), CommitState::ClaimedNeverPolled);
        owner.settle().await.unwrap();
        assert_eq!(owner.commit_state(Purpose::Intent), CommitState::AbandonedNeverPolled);
        let commit = Purpose::Intent.index() * 3 + Statement::Commit.offset();
        assert!(owner.statements[commit].attempt.is_none());
        assert!(owner.settled());
        owner.close().await.unwrap();
        let options = SqliteConnectOptions::new().filename(file.path()).create_if_missing(false);
        let mut verifier = SqliteConnection::connect_with(&options).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM retained_commit_probe")
            .fetch_one(&mut verifier).await.unwrap();
        assert_eq!(count, 0);
        verifier.close().await.unwrap();
    }

    #[tokio::test]
    async fn dropped_never_polled_begin_keeps_connection_recoverable() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        drop(owner.begin(Purpose::Intent));
        assert!(owner.connection.is_some());
        assert_eq!(owner.statements[0].state, CommitState::ClaimedNeverPolled);
        owner.settle().await.unwrap();
        let mut lease = owner.connection.as_mut().unwrap().lock_handle().await.unwrap();
        assert_eq!(lease.as_raw_handle().as_ptr() as usize, original);
        drop(lease);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn completed_commit_retains_frame_result_and_original_connection() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        owner.begin(Purpose::Intent).await.unwrap();
        owner.claim_commit(Purpose::Intent).unwrap();
        // Runner-only fixture. Production must do real host authentication
        // inside this polling closure immediately before the poll call.
        poll_fn(|cx| owner.poll_commit_after_fence(cx)).await.unwrap();
        assert_eq!(owner.commit_state(Purpose::Intent), CommitState::ReturnedOk);
        assert_eq!(identity(&mut owner).await, original);
        assert!(owner.claim_commit(Purpose::Intent).is_err());
        owner.settle().await.unwrap();
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn commit_error_keeps_connection_and_is_not_reclassified_by_autocommit() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        // There is deliberately no transaction, so this actual fixed COMMIT
        // returns SQLite's error. Autocommit afterward is not durable success.
        owner.claim_commit(Purpose::Intent).unwrap();
        assert!(poll_fn(|cx| owner.poll_commit_after_fence(cx)).await.is_err());
        assert_eq!(owner.commit_state(Purpose::Intent), CommitState::ReturnedErr);
        assert_eq!(identity(&mut owner).await, original);
        owner.settle().await.unwrap();
        assert!(owner.settled());
        assert_eq!(owner.commit_state(Purpose::Intent), CommitState::ReturnedErr);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn dropped_unpolled_close_waiter_retains_exact_frame_until_positive_ack() {
        let (_file, mut owner) = owner().await;
        assert!(!owner.rollback_completed(Purpose::Ack));
        owner.begin(Purpose::Ack).await.unwrap();
        owner.rollback(Purpose::Ack).await.unwrap();
        assert!(owner.rollback_completed(Purpose::Ack));
        assert!(!owner.rollback_completed(Purpose::Reconcile));
        // A saved ACK completes through rollback, with no COMMIT attempt.
        assert_eq!(owner.commit_state(Purpose::Ack), CommitState::NotAttempted);
        owner.settle().await.unwrap();
        assert!(matches!(&owner.inspection[0].as_ref().unwrap().outcome,
            Some(Outcome::Returned(Ok(observation))) if observation.ended()));
        drop(owner.close());
        assert!(owner.close_frame.is_some());
        assert!(owner.connection.is_none());
        assert!(!owner.close_attempted);
        assert!(owner.close_outcome.is_none());
        assert!(!owner.closed());
        owner.close().await.unwrap();
        assert!(owner.closed());
        assert!(owner.close_frame.is_some());
        assert!(matches!(&owner.close_outcome, Some(Outcome::Returned(Ok(())))));
        assert!(owner.configuration.is_some());
        assert!(owner.rollback_inspection[Purpose::Ack.index()].is_some());
        assert!(owner.rollback_completed(Purpose::Ack));
        assert_eq!(owner.commit_state(Purpose::Ack), CommitState::NotAttempted);
    }

    #[tokio::test]
    async fn retained_close_error_after_actual_release_never_becomes_positive() {
        let (_file, mut owner) = owner().await;
        owner.settle().await.unwrap();
        assert!(owner.settled());
        let connection = owner.connection.take().unwrap();
        // Test-only fault after the genuine close completes. The exact frame
        // owns the real connection, and its returned error is retained. This
        // never supplies a fake successful close or a reusable replacement.
        owner.close_frame = Some(Box::pin(async move {
            connection.close().await?;
            Err(sqlx::Error::Protocol("injected post-release close error".into()))
        }));
        assert!(owner.close().await.is_err());
        assert!(owner.connection.is_none());
        assert!(owner.close_frame.is_some());
        assert!(matches!(&owner.close_outcome, Some(Outcome::Returned(Err(_)))));
        assert!(!owner.closed());
        // A repoll of the completed async frame would panic; only its retained
        // error may be observed by a second terminal-cleanup waiter.
        assert!(owner.close().await.is_err());
        assert!(matches!(&owner.close_outcome, Some(Outcome::Returned(Err(_)))));
        assert!(!owner.closed());
    }

    #[tokio::test]
    async fn retained_close_panic_after_actual_release_never_repolls_or_succeeds() {
        let (_file, mut owner) = owner().await;
        owner.settle().await.unwrap();
        let connection = owner.connection.take().unwrap();
        owner.close_frame = Some(Box::pin(async move {
            connection.close().await?;
            panic!("injected post-release close panic");
            #[allow(unreachable_code)]
            Ok(())
        }));
        assert!(owner.close().await.is_err());
        assert!(owner.connection.is_none());
        assert!(owner.close_frame.is_some());
        assert!(matches!(&owner.close_outcome, Some(Outcome::Panicked(_))));
        assert!(!owner.closed());
        assert!(owner.close().await.is_err());
        assert!(matches!(&owner.close_outcome, Some(Outcome::Panicked(_))));
        assert!(!owner.closed());
    }

    struct BorrowedPanic<'a>(&'a mut SqliteConnection);

    impl Future for BorrowedPanic<'_> {
        type Output = Result<(), sqlx::Error>;

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            let _same_borrow = &mut *self.get_mut().0;
            panic!("injected panic in the borrowed statement future");
        }
    }

    #[tokio::test]
    async fn contained_borrowed_panic_reinstalls_actual_connection_and_payload() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        let mut connection = owner.connection.take().unwrap();
        let frame = Box::pin(async move {
            let outcome = {
                let mut borrowed = pin!(BorrowedPanic(&mut connection));
                poll_fn(|cx| match catch_unwind(AssertUnwindSafe(|| borrowed.as_mut().poll(cx))) {
                    Ok(Poll::Pending) => Poll::Pending,
                    Ok(Poll::Ready(result)) => Poll::Ready(Outcome::Returned(result)),
                    Err(payload) => Poll::Ready(Outcome::Panicked(payload)),
                }).await
            };
            (connection, outcome)
        });
        let mut attempt = OwnedAttempt::new(frame);
        assert!(poll_fn(|cx| attempt.poll(&mut owner.connection, cx)).await.is_err());
        assert_eq!(attempt.state(), CommitState::PanickedUncertain);
        assert!(matches!(&attempt.outcome, Some(Outcome::Panicked(payload)) if payload.is::<&'static str>()));
        assert_eq!(identity(&mut owner).await, original);
        // A second observation returns the retained result, not another poll
        // of the completed/panicked nested future.
        assert!(poll_fn(|cx| attempt.poll(&mut owner.connection, cx)).await.is_err());
        assert_eq!(identity(&mut owner).await, original);
        owner.settle().await.unwrap();
        owner.close().await.unwrap();
    }

    // Delay delivery AFTER the genuine serialized-handle observation, while
    // the same connection is still held by the installed owned frame. This
    // deterministically exercises cancellation at that real owned boundary.
    fn held_inspection(
        connection: SqliteConnection,
    ) -> (
        OwnedAttempt<TransactionObservation>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let frame = Box::pin(async move {
            let observed = inspect_frame(connection).await;
            let _ = observed_tx.send(());
            let _ = release_rx.await;
            observed
        });
        (OwnedAttempt::new(frame), observed_rx, release_tx)
    }

    #[tokio::test]
    async fn dropped_pre_rollback_inspection_waiter_resumes_same_frame() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        owner.begin(Purpose::Intent).await.unwrap();
        let connection = owner.connection.take().unwrap();
        let (attempt, observed, release) = held_inspection(connection);
        owner.inspection[0] = Some(attempt);
        let mut waiter = Box::pin(owner.settle());
        tokio::select! {
            result = &mut waiter => panic!("inspection unexpectedly completed: {result:?}"),
            result = observed => result.unwrap(),
        }
        drop(waiter);
        assert!(owner.connection.is_none());
        release.send(()).unwrap();
        owner.settle().await.unwrap();
        assert_eq!(owner.statements[TERMINAL_ROLLBACK].state, CommitState::ReturnedOk);
        let mut lease = owner.connection.as_mut().unwrap().lock_handle().await.unwrap();
        assert_eq!(lease.as_raw_handle().as_ptr() as usize, original);
        drop(lease);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn dropped_post_rollback_inspection_waiter_does_not_reissue_rollback() {
        let (_file, mut owner) = owner().await;
        let original = identity(&mut owner).await;
        owner.begin(Purpose::Intent).await.unwrap();
        owner.settlement_requested = true;
        assert!(!owner.inspect(0).await.unwrap().ended());
        owner.claim(TERMINAL_ROLLBACK, true).unwrap();
        poll_fn(|cx| owner.poll_statement(TERMINAL_ROLLBACK, true, cx)).await.unwrap();
        let connection = owner.connection.take().unwrap();
        let (attempt, observed, release) = held_inspection(connection);
        owner.inspection[1] = Some(attempt);
        let mut waiter = Box::pin(owner.settle());
        tokio::select! {
            result = &mut waiter => panic!("inspection unexpectedly completed: {result:?}"),
            result = observed => result.unwrap(),
        }
        drop(waiter);
        assert!(owner.connection.is_none());
        release.send(()).unwrap();
        owner.settle().await.unwrap();
        assert_eq!(owner.statements[TERMINAL_ROLLBACK].state, CommitState::ReturnedOk);
        assert!(owner.claim(TERMINAL_ROLLBACK, true).is_err());
        let mut lease = owner.connection.as_mut().unwrap().lock_handle().await.unwrap();
        assert_eq!(lease.as_raw_handle().as_ptr() as usize, original);
        drop(lease);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn normal_rollback_observation_survives_waiter_drop_and_terminal_transfer() {
        for finish_terminally in [false, true] {
            let (_file, mut owner) = owner().await;
            let original = identity(&mut owner).await;
            owner.begin(Purpose::Intent).await.unwrap();
            let slot = Purpose::Intent.index() * 3 + Statement::Rollback.offset();
            owner.claim(slot, false).unwrap();
            poll_fn(|cx| owner.poll_statement(slot, true, cx)).await.unwrap();
            let connection = owner.connection.take().unwrap();
            let (attempt, observed, release) = held_inspection(connection);
            owner.rollback_inspection[Purpose::Intent.index()] = Some(attempt);
            owner.pending_rollback_inspection = Some(Purpose::Intent.index());
            let mut waiter = Box::pin(owner.rollback(Purpose::Intent));
            tokio::select! {
                result = &mut waiter => panic!("rollback observation unexpectedly completed: {result:?}"),
                result = observed => result.unwrap(),
            }
            drop(waiter);
            assert!(owner.connection_mut().is_err());
            release.send(()).unwrap();
            if finish_terminally {
                owner.settle().await.unwrap();
                assert!(owner.settled());
            } else {
                owner.rollback(Purpose::Intent).await.unwrap();
                assert_eq!(identity(&mut owner).await, original);
                assert!(owner.rollback(Purpose::Intent).await.is_err());
                owner.begin(Purpose::Dispatch).await.unwrap();
                owner.rollback(Purpose::Dispatch).await.unwrap();
                owner.settle().await.unwrap();
            }
            assert_eq!(owner.statements[slot].state, CommitState::ReturnedOk);
            assert!(owner.pending_rollback_inspection.is_none());
            owner.close().await.unwrap();
        }
    }
}
