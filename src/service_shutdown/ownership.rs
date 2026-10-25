//! Task-owned join evidence. This is not yet stopped-service/export authority:
//! terminal reservation/native admission closure and I/O custody are separate.
use super::{Completion, completed};
use std::{
    collections::BTreeSet, fmt::Display, future::Future, pin::Pin, sync::Arc, time::Duration,
};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Worker,
    Http,
    Metrics,
    Ingress,
    /// Must be registered only when the real native watch owner is launched.
    Watch,
}
struct Identity {
    runtime: Option<Arc<playscale::native_catalog::Runtime>>,
}
pub(crate) struct Registry {
    identity: Arc<Identity>,
    roles: BTreeSet<Role>,
    ingress_recorded: bool,
}
pub(crate) struct RequiredOwners {
    identity: Arc<Identity>,
    roles: BTreeSet<Role>,
}
pub(crate) struct OwnedTask {
    identity: Arc<Identity>,
    role: Role,
    task: Pin<Box<dyn Future<Output = Completion> + Send>>,
    completion: Option<Completion>,
    proof_issued: bool,
}
pub(crate) struct JoinedOwner {
    identity: Arc<Identity>,
    role: Role,
}
pub(crate) struct JoinedServices {
    identity: Arc<Identity>,
}
impl Registry {
    pub(crate) fn new(runtime: Option<Arc<playscale::native_catalog::Runtime>>) -> Self {
        Self {
            identity: Arc::new(Identity { runtime }),
            roles: BTreeSet::new(),
            ingress_recorded: false,
        }
    }
    pub(crate) fn register<E: Display + Send + 'static>(
        &mut self,
        role: Role,
        task: JoinHandle<Result<(), E>>,
    ) -> anyhow::Result<OwnedTask> {
        anyhow::ensure!(
            role != Role::Ingress,
            "ingress must be registered through startup optional handle"
        );
        self.insert(role, task)
    }
    fn insert<E: Display + Send + 'static>(
        &mut self,
        role: Role,
        task: JoinHandle<Result<(), E>>,
    ) -> anyhow::Result<OwnedTask> {
        anyhow::ensure!(self.roles.insert(role), "duplicate service owner role");
        Ok(OwnedTask {
            identity: self.identity.clone(),
            role,
            task: Box::pin(async move { completed(task.await) }),
            completion: None,
            proof_issued: false,
        })
    }
    pub(crate) fn ingress<E: Display + Send + 'static>(
        &mut self,
        task: Option<JoinHandle<Result<(), E>>>,
    ) -> anyhow::Result<Option<OwnedTask>> {
        anyhow::ensure!(
            !self.ingress_recorded,
            "ingress startup registration already recorded"
        );
        self.ingress_recorded = true;
        task.map(|task| self.insert(Role::Ingress, task))
            .transpose()
    }
    pub(crate) fn seal(self) -> anyhow::Result<RequiredOwners> {
        anyhow::ensure!(
            self.ingress_recorded
                && [Role::Worker, Role::Http, Role::Metrics]
                    .iter()
                    .all(|role| self.roles.contains(role)),
            "incomplete actual service owner registration"
        );
        Ok(RequiredOwners {
            identity: self.identity,
            roles: self.roles,
        })
    }
}
impl OwnedTask {
    /// Select may stop polling this method; the original handle remains owned
    /// inside self, and a consumed terminal result is never polled twice.
    pub(crate) async fn wait(&mut self) -> Completion {
        if self.completion.is_none() {
            self.completion = Some((&mut self.task).await);
        }
        self.completion.as_ref().expect("joined result").clone()
    }
    pub(crate) async fn join(&mut self, grace: Duration) -> anyhow::Result<JoinedOwner> {
        anyhow::ensure!(!self.proof_issued, "service join proof already consumed");
        tokio::time::timeout(grace, self.wait())
            .await
            .map_err(|_| anyhow::anyhow!("service owner {:?} did not finish", self.role))?
            .map_err(anyhow::Error::msg)?;
        self.proof_issued = true;
        Ok(JoinedOwner {
            identity: self.identity.clone(),
            role: self.role,
        })
    }
}
impl RequiredOwners {
    pub(crate) fn finish(self, owners: Vec<JoinedOwner>) -> anyhow::Result<JoinedServices> {
        let mut roles = BTreeSet::new();
        for owner in owners {
            anyhow::ensure!(
                Arc::ptr_eq(&owner.identity, &self.identity) && roles.insert(owner.role),
                "foreign or duplicate service join proof"
            );
        }
        anyhow::ensure!(roles == self.roles, "missing actual service join proof");
        Ok(JoinedServices {
            identity: self.identity,
        })
    }
}
impl JoinedServices {
    pub(crate) fn belongs_to(&self, runtime: &Arc<playscale::native_catalog::Runtime>) -> bool {
        self.identity
            .runtime
            .as_ref()
            .is_some_and(|actual| Arc::ptr_eq(actual, runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn registered() -> (RequiredOwners, Vec<JoinedOwner>) {
        let mut registry = Registry::new(None);
        let mut tasks = Vec::new();
        for role in [Role::Worker, Role::Http, Role::Metrics] {
            tasks.push(
                registry
                    .register(role, tokio::spawn(async { Ok::<(), anyhow::Error>(()) }))
                    .unwrap(),
            );
        }
        registry.ingress::<anyhow::Error>(None).unwrap();
        let required = registry.seal().unwrap();
        let mut proofs = Vec::new();
        for task in &mut tasks {
            proofs.push(task.join(Duration::from_secs(1)).await.unwrap());
        }
        (required, proofs)
    }
    #[tokio::test]
    async fn exact_startup_set_rejects_foreign_and_missing_owner_proofs() {
        let (required, mut proofs) = registered().await;
        let (_, mut foreign) = registered().await;
        proofs[0] = foreign.remove(0);
        assert!(required.finish(proofs).is_err());
        let (required, mut proofs) = registered().await;
        proofs.pop();
        assert!(required.finish(proofs).is_err());
        let (required, proofs) = registered().await;
        assert!(required.finish(proofs).is_ok());
    }
    #[tokio::test]
    async fn optional_presence_and_every_registered_extra_owner_require_join() {
        let mut registry = Registry::new(None);
        assert!(Registry::new(None).seal().is_err());
        let mut tasks = Vec::new();
        for role in [Role::Worker, Role::Http, Role::Metrics, Role::Watch] {
            tasks.push(
                registry
                    .register(role, tokio::spawn(async { Ok::<(), anyhow::Error>(()) }))
                    .unwrap(),
            );
        }
        let ingress = registry
            .ingress(Some(tokio::spawn(async { Ok::<(), anyhow::Error>(()) })))
            .unwrap()
            .unwrap();
        tasks.push(ingress);
        let required = registry.seal().unwrap();
        let mut proofs = Vec::new();
        for task in &mut tasks {
            proofs.push(task.join(Duration::from_secs(1)).await.unwrap());
        }
        proofs.pop();
        assert!(required.finish(proofs).is_err());
    }
    #[tokio::test]
    async fn consumed_select_result_yields_exactly_one_join_proof() {
        let mut registry = Registry::new(None);
        let mut task = registry
            .register(
                Role::Worker,
                tokio::spawn(async { Ok::<(), anyhow::Error>(()) }),
            )
            .unwrap();
        task.wait().await.unwrap();
        task.join(Duration::from_secs(1)).await.unwrap();
        assert!(task.join(Duration::from_secs(1)).await.is_err());
    }
}
