//! Test-only observation of both genuine startup recoveries. Not a production
//! readiness, admission or drain permit. Register before NativeService::start.
use crate::App;
use anyhow::{Context, ensure};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::oneshot;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Scan,
    Processing,
}
type Key = (usize, Role);
type Registration = (Weak<()>, oneshot::Sender<()>);
static WAITERS: OnceLock<Mutex<BTreeMap<Key, Registration>>> = OnceLock::new();
fn waiters() -> &'static Mutex<BTreeMap<Key, Registration>> {
    WAITERS.get_or_init(Default::default)
}
fn owner(app: &App) -> anyhow::Result<usize> {
    Ok(Arc::as_ptr(
        app.native_catalog
            .as_ref()
            .context("native startup owner missing")?,
    ) as usize)
}
pub(crate) struct Observation {
    owner: usize,
    identity: Arc<()>,
    receivers: Option<(oneshot::Receiver<()>, oneshot::Receiver<()>)>,
    // Prevent exact Runtime address recycling while an observation is retained.
    _runtime: Arc<crate::native_catalog::Runtime>,
}
impl Observation {
    pub(crate) async fn wait(mut self) -> anyhow::Result<()> {
        let (scan, processing) = self.receivers.take().expect("single startup observation");
        tokio::try_join!(
            async {
                scan.await
                    .context("native scan recovery success was not observed")
            },
            async {
                processing
                    .await
                    .context("native processing recovery success was not observed")
            },
        )?;
        Ok(())
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if let Ok(mut map) = waiters().lock() {
            for role in [Role::Scan, Role::Processing] {
                let key = (self.owner, role);
                if map
                    .get(&key)
                    .is_some_and(|(id, _)| id.ptr_eq(&Arc::downgrade(&self.identity)))
                {
                    map.remove(&key);
                }
            }
        }
    }
}
pub(crate) fn observe(app: &App) -> anyhow::Result<Observation> {
    let owner = owner(app)?;
    let identity = Arc::new(());
    let (scan_send, scan) = oneshot::channel();
    let (processing_send, processing) = oneshot::channel();
    let mut map = waiters()
        .lock()
        .map_err(|_| anyhow::anyhow!("startup observation registry poisoned"))?;
    ensure!(
        [Role::Scan, Role::Processing]
            .iter()
            .all(|role| !map.contains_key(&(owner, *role))),
        "startup recovery already observed for this runtime"
    );
    map.insert((owner, Role::Scan), (Arc::downgrade(&identity), scan_send));
    map.insert(
        (owner, Role::Processing),
        (Arc::downgrade(&identity), processing_send),
    );
    Ok(Observation {
        owner,
        identity,
        receivers: Some((scan, processing)),
        _runtime: app
            .native_catalog
            .as_ref()
            .expect("validated native owner")
            .clone(),
    })
}
/// Called only immediately after the corresponding real recovery returns Ok.
/// Failed/cancelled recovery never reaches this observer and cannot signal ready.
pub(crate) fn recovered(app: &App, role: Role) {
    let owner = owner(app).expect("successful native recovery has runtime");
    let registration = waiters()
        .lock()
        .expect("startup observation registry poisoned")
        .remove(&(owner, role));
    if let Some((identity, sender)) = registration {
        if identity.upgrade().is_some() {
            let _ = sender.send(());
        }
    }
}
