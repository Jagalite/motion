//! Execution capacity adapter. Correctness decisions live in
//! `playscale_core::work`; this module applies them under one lock, wakes
//! admitted owners, and reports observed process exits and stuck terminations.
use playscale_core::work::{self, Budget, Class, Effect, Hold, Input, Ledger, Rejection};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

/// Interactive delivery may use both units; background work is limited to one.
pub const BUDGET: Budget = Budget {
    units: 2,
    interactive_reserve: 1,
};
/// How long the server waits for a supervisor to confirm termination before the
/// reservation is classified as stuck. The supervisor's own SIGTERM grace is shorter.
pub const TERMINATION_DEADLINE: Duration = Duration::from_secs(10);

struct Inner {
    ledger: Ledger,
    starts: HashMap<u64, oneshot::Sender<()>>,
    terminate: HashMap<u64, CancellationToken>,
}

pub struct Coordinator {
    inner: Mutex<Inner>,
}

#[derive(Serialize, ToSchema)]
pub struct Snapshot {
    pub units: u32,
    pub interactive_reserve: u32,
    pub used: u64,
    pub running: usize,
    pub terminating: usize,
    /// Owners whose termination was not confirmed. Their capacity stays reserved.
    pub stuck: Vec<String>,
    pub waiting: usize,
}

impl Coordinator {
    pub fn new(budget: Budget) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                ledger: Ledger::new(budget),
                starts: HashMap::new(),
                terminate: HashMap::new(),
            }),
        })
    }

    fn apply(inner: &mut Inner, input: Input) -> Vec<Effect> {
        let (next, effects) = work::transition(&inner.ledger, &input);
        inner.ledger = next;
        for effect in &effects {
            match effect {
                Effect::Start { ticket } => {
                    if let Some(start) = inner.starts.remove(ticket) {
                        let _ = start.send(());
                    }
                }
                Effect::Terminate { ticket } => {
                    if let Some(token) = inner.terminate.get(ticket) {
                        token.cancel();
                    }
                }
                Effect::Released { ticket, .. } => {
                    inner.terminate.remove(ticket);
                }
                Effect::Accepted { .. } | Effect::Rejected { .. } => {}
            }
        }
        effects
    }

    fn input(&self, input: Input) -> Vec<Effect> {
        Self::apply(&mut self.inner.lock().unwrap(), input)
    }

    /// Wait for admission. Dropping the future before admission withdraws the request.
    /// `owner` must be unique among live requests (for example `job:attempt`).
    pub async fn reserve(
        self: &Arc<Self>,
        owner: String,
        class: Class,
        units: u32,
    ) -> Result<Lease, Rejection> {
        let (start, started) = oneshot::channel();
        let token = CancellationToken::new();
        let ticket = {
            let mut inner = self.inner.lock().unwrap();
            // Tickets are issued sequentially; register before the reducer may start it.
            let ticket = inner.ledger.last_ticket.wrapping_add(1);
            inner.starts.insert(ticket, start);
            inner.terminate.insert(ticket, token.clone());
            let effects = Self::apply(
                &mut inner,
                Input::Request {
                    owner: owner.clone(),
                    class,
                    units,
                },
            );
            let accepted = effects.iter().find_map(|e| match e {
                Effect::Accepted { ticket, .. } => Some(*ticket),
                _ => None,
            });
            if accepted != Some(ticket) {
                inner.starts.remove(&ticket);
                inner.terminate.remove(&ticket);
                return Err(effects
                    .iter()
                    .find_map(|e| match e {
                        Effect::Rejected { reason, .. } => Some(*reason),
                        _ => None,
                    })
                    .unwrap_or(Rejection::OwnerConflict));
            }
            ticket
        };
        let mut waiting = Withdraw {
            coordinator: self.clone(),
            ticket,
            armed: true,
        };
        // The sender is only removed when Start is emitted.
        let _ = started.await;
        waiting.armed = false;
        Ok(Lease {
            coordinator: self.clone(),
            ticket,
            terminate: token,
            stuck: AtomicBool::new(false),
            running: Mutex::new(None),
        })
    }

    /// Whether a request would be admitted immediately. Advisory: used to choose
    /// between overlapping and disruptive replacement before requesting capacity.
    pub fn fits_now(&self, class: Class, units: u32) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.ledger.waiting.is_empty() && inner.ledger.fits(class, units)
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        let l = &inner.ledger;
        let count = |hold| l.held.values().filter(|r| r.hold == hold).count();
        Snapshot {
            units: l.budget.units,
            interactive_reserve: l.budget.interactive_reserve,
            used: l.used(),
            running: count(Hold::Running),
            terminating: count(Hold::Terminating),
            stuck: l
                .held
                .values()
                .filter(|r| r.hold == Hold::Stuck)
                .map(|r| r.owner.clone())
                .collect(),
            waiting: l.waiting.len(),
        }
    }
}

struct Withdraw {
    coordinator: Arc<Coordinator>,
    ticket: u64,
    armed: bool,
}
impl Drop for Withdraw {
    fn drop(&mut self) {
        if self.armed {
            let mut inner = self.coordinator.inner.lock().unwrap();
            inner.starts.remove(&self.ticket);
            if inner.ledger.is_waiting(self.ticket) {
                Coordinator::apply(
                    &mut inner,
                    Input::Cancel {
                        ticket: self.ticket,
                    },
                );
                inner.terminate.remove(&self.ticket);
            } else {
                // Admitted concurrently with cancellation: nothing ran, so it exited.
                Coordinator::apply(
                    &mut inner,
                    Input::Exited {
                        ticket: self.ticket,
                    },
                );
            }
        }
    }
}

/// Capacity held by one execution owner. Dropping the lease reports that the
/// owner's work ended, unless a process outlived its termination deadline; then
/// the reaper reports the exit once it is actually observed.
pub struct Lease {
    coordinator: Arc<Coordinator>,
    ticket: u64,
    terminate: CancellationToken,
    stuck: AtomicBool,
    /// Witness of a started execution whose end is not yet confirmed. If the lease
    /// is dropped while this is set (e.g. its future was cancelled), the capacity
    /// stays reserved until the witness is released.
    running: Mutex<Option<Witness>>,
}
impl Lease {
    pub fn ticket(&self) -> u64 {
        self.ticket
    }
    /// Record that an execution owning the witness at `path` has started.
    pub fn started(&self, path: &std::path::Path) -> std::io::Result<()> {
        *self.running.lock().unwrap() = Some(Witness::open(path)?);
        Ok(())
    }
    fn settled(&self) {
        self.running.lock().unwrap().take();
    }
    /// Whether termination was not confirmed; a reaper now owns the release.
    pub fn is_stuck(&self) -> bool {
        self.stuck.load(Ordering::SeqCst)
    }
    /// Cancelled when the coordinator requests termination of this owner.
    pub fn termination_requested(&self) -> &CancellationToken {
        &self.terminate
    }
    /// Fence the owner: termination is starting. Capacity remains reserved.
    pub fn terminating(&self) {
        self.coordinator.input(Input::Cancel {
            ticket: self.ticket,
        });
    }
    /// A child did not confirm termination in time. Capacity stays reserved until
    /// `reaper` resolves (the child exited and was reaped).
    pub fn stuck(&self, reaper: impl Future<Output = ()> + Send + 'static) {
        self.terminating();
        self.coordinator.input(Input::Stuck {
            ticket: self.ticket,
        });
        if !self.stuck.swap(true, Ordering::SeqCst) {
            let coordinator = self.coordinator.clone();
            let ticket = self.ticket;
            tokio::spawn(async move {
                reaper.await;
                coordinator.input(Input::Exited { ticket });
            });
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.stuck.load(Ordering::SeqCst) {
            return;
        }
        let running = self.running.lock().unwrap().take();
        match running {
            None => {
                self.coordinator.input(Input::Exited {
                    ticket: self.ticket,
                });
            }
            Some(witness) => {
                // Dropped mid-execution: the supervisor sees its control pipe close
                // and terminates; release only once every holder is gone.
                tracing::warn!(ticket = self.ticket, "lease dropped during execution");
                self.terminating();
                self.coordinator.input(Input::Stuck {
                    ticket: self.ticket,
                });
                let coordinator = self.coordinator.clone();
                let ticket = self.ticket;
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move {
                        witness.wait().await;
                        coordinator.input(Input::Exited { ticket });
                    });
                }
            }
        }
    }
}

/// Evidence that every process which inherited an execution's ownership
/// descriptor is gone. The server creates a lock file and holds an exclusive
/// `flock` on one open description that children inherit. A separate probe
/// description can take the lock only after every holder closed it or died,
/// including descendants that left the supervisor's process group. The probe is
/// unaffected by later deletion of the path. Without `flock` (non-Unix) the
/// witness is vacuous and only the supervisor's exit is evidence.
pub struct Witness {
    #[cfg(unix)]
    probe: std::fs::File,
}
/// The locked description to hand to the execution (see [`inherit`]).
pub struct Held(pub std::fs::File);

#[cfg(unix)]
fn flock(file: &std::fs::File, operation: libc::c_int) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), operation) == 0 }
}

impl Witness {
    /// Create the witness for a new execution. Fails if another owner holds it.
    pub fn create(path: &std::path::Path) -> std::io::Result<(Self, Held)> {
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        #[cfg(unix)]
        if !flock(&held, libc::LOCK_EX | libc::LOCK_NB) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "execution witness is held by another owner",
            ));
        }
        Ok((Self::open(path)?, Held(held)))
    }
    /// Observe an existing witness, e.g. one left by a previous server process.
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        return Ok(Self {
            probe: std::fs::File::open(path)?,
        });
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(Self {})
        }
    }
    pub fn released(&self) -> bool {
        #[cfg(unix)]
        {
            flock(&self.probe, libc::LOCK_EX | libc::LOCK_NB) && flock(&self.probe, libc::LOCK_UN)
        }
        #[cfg(not(unix))]
        true
    }
    /// Resolve once every holder is gone. A blocking-pool thread waits on the lock.
    pub async fn wait(self) {
        #[cfg(unix)]
        {
            let probe = self.probe;
            let _ = tokio::task::spawn_blocking(move || {
                while !flock(&probe, libc::LOCK_EX) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .await;
        }
    }
}

/// Make `files` descriptors 3, 4, ... in the spawned child, without close-on-exec,
/// so the supervisor and its encoder inherit them.
#[cfg(unix)]
pub fn inherit(command: &mut tokio::process::Command, files: &[&std::fs::File]) {
    use std::os::fd::AsRawFd;
    let fds: Vec<libc::c_int> = files.iter().map(|f| f.as_raw_fd()).collect();
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            let mut temporary = [0; 8];
            for (i, fd) in fds.iter().enumerate() {
                temporary[i] = libc::fcntl(*fd, libc::F_DUPFD, 16);
                if temporary[i] < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for (i, fd) in temporary.iter().take(fds.len()).enumerate() {
                if libc::dup2(*fd, 3 + i as libc::c_int) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(*fd);
            }
            Ok(())
        });
    }
}

/// Confirm that an execution ended: its supervisor (`child`, if it may still be
/// running; `None` after a natural exit) exits and its witness is released. `exited` runs once both are observed: immediately
/// when confirmed within `deadline`, otherwise from a reaper while the lease stays
/// stuck. A failed wait is not confirmation. Returns whether it was confirmed.
pub async fn confirm_exit(
    lease: &Lease,
    child: Option<tokio::process::Child>,
    witness: Option<Witness>,
    deadline: Duration,
    exited: impl FnOnce() + Send + 'static,
) -> bool {
    if child.is_some() {
        // A running supervisor is being stopped; a natural exit is not fenced.
        lease.terminating();
    }
    let started = tokio::time::Instant::now();
    let (reaped, remaining) = match child {
        None => (true, None),
        Some(mut child) => match tokio::time::timeout(deadline, child.wait()).await {
            Ok(Ok(_)) => (true, None),
            Ok(Err(error)) => {
                // Not evidence of exit; keep the handle for the reaper to retry.
                tracing::error!(%error, "could not wait for worker");
                (false, Some(child))
            }
            Err(_) => (false, Some(child)),
        },
    };
    let mut released = witness.as_ref().is_none_or(Witness::released);
    while reaped && !released && started.elapsed() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        released = witness.as_ref().is_none_or(Witness::released);
    }
    if reaped && released {
        lease.settled();
        exited();
        return true;
    }
    lease.settled();
    tracing::error!(
        ticket = lease.ticket(),
        "worker termination not confirmed; capacity remains reserved"
    );
    lease.stuck(async move {
        if let Some(mut child) = remaining {
            // Retry failed waits; only a successful reap is evidence.
            while child.wait().await.is_err() {
                if witness.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        if let Some(witness) = witness {
            witness.wait().await;
        }
        exited();
    });
    false
}

impl Coordinator {
    /// Account for an execution left by a previous server process whose witness is
    /// still held. It is adopted as stuck capacity even beyond the budget, so no new
    /// work runs beside it; `released` runs after the witness is free.
    pub fn recovered(
        self: &Arc<Self>,
        owner: String,
        class: Class,
        units: u32,
        witness: Witness,
        released: impl FnOnce() + Send + 'static,
    ) {
        let effects = self.input(Input::Adopt {
            owner: owner.clone(),
            class,
            units,
        });
        let Some(ticket) = effects.iter().find_map(|e| match e {
            Effect::Accepted { ticket, .. } => Some(*ticket),
            _ => None,
        }) else {
            // Owner names embed unique paths; a conflict means it is already adopted.
            tracing::error!(%owner, "recovered execution could not be adopted");
            return;
        };
        tracing::warn!(%owner, "execution from a previous process is still alive; capacity reserved");
        let coordinator = self.clone();
        tokio::spawn(async move {
            witness.wait().await;
            coordinator.input(Input::Exited { ticket });
            released();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stuck_child_keeps_capacity_until_reaped() {
        let coordinator = Coordinator::new(Budget {
            units: 1,
            interactive_reserve: 0,
        });
        let lease = coordinator
            .reserve("a".into(), Class::Preparation, 1)
            .await
            .unwrap();
        let child = tokio::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .unwrap();
        let (seen, observed) = tokio::sync::oneshot::channel();
        assert!(
            !confirm_exit(
                &lease,
                Some(child),
                None,
                Duration::from_millis(50),
                move || {
                    let _ = seen.send(());
                }
            )
            .await
        );
        drop(lease);
        assert_eq!(coordinator.snapshot().stuck, ["a"]);
        let next = coordinator.clone();
        let waiting = tokio::spawn(async move {
            next.reserve("b".into(), Class::Preparation, 1)
                .await
                .map(|_| ())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiting.is_finished());
        assert_eq!(coordinator.snapshot().waiting, 1);
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        observed.await.unwrap();
        let s = coordinator.snapshot();
        assert!(s.stuck.is_empty() && s.used == 0 && s.waiting == 0);
    }

    #[tokio::test]
    async fn confirmed_exit_and_abandoned_waiters_release() {
        let coordinator = Coordinator::new(Budget {
            units: 1,
            interactive_reserve: 0,
        });
        let lease = coordinator
            .reserve("a".into(), Class::Preparation, 1)
            .await
            .unwrap();
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            coordinator.reserve("b".into(), Class::Preparation, 1),
        )
        .await;
        assert!(blocked.is_err());
        assert_eq!(coordinator.snapshot().waiting, 0);
        let child = tokio::process::Command::new("true").spawn().unwrap();
        assert!(confirm_exit(&lease, Some(child), None, Duration::from_secs(5), || {}).await);
        assert_eq!(coordinator.snapshot().terminating, 1);
        drop(lease);
        assert_eq!(coordinator.snapshot().used, 0);
        assert!(matches!(
            coordinator
                .reserve("c".into(), Class::Preparation, 2)
                .await
                .err(),
            Some(Rejection::ExceedsBudget)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn witness_outlives_supervisor_until_every_holder_exits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner");
        let (witness, held) = Witness::create(&path).unwrap();
        assert!(Witness::create(&path).is_err(), "second owner rejected");
        // A descendant that keeps the inherited descriptor after its parent exits.
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 1 3>&3 & exit 0"]);
        inherit(&mut command, &[&held.0]);
        let child = command.spawn().unwrap();
        drop(held);
        let coordinator = Coordinator::new(Budget {
            units: 1,
            interactive_reserve: 0,
        });
        let lease = coordinator
            .reserve("a".into(), Class::Preparation, 1)
            .await
            .unwrap();
        // The direct child exits at once, but the witness is still held.
        assert!(
            !confirm_exit(
                &lease,
                Some(child),
                Some(witness),
                Duration::from_millis(200),
                || {}
            )
            .await
        );
        drop(lease);
        assert_eq!(coordinator.snapshot().stuck, ["a"]);
        tokio::time::timeout(Duration::from_secs(5), async {
            while coordinator.snapshot().used != 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        let (witness, held) = Witness::create(&path).unwrap();
        assert!(!witness.released());
        drop(held);
        assert!(witness.released());
    }
}
