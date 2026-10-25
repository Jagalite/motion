//! Result custody for the finite retained94 producer slots. These views never
//! clone or extract a Native output, including after the Native session closes.
use catabolic_store::migration::offline_bootstrap::{
    OfflineRetainedResult, OfflineWorkspaceAdmissionRefused,
};

pub(crate) enum RelocationProducer<T, E> {
    Ordinary(Result<T, E>),
    Workspace(Result<OfflineRetainedResult<T, E>, OfflineWorkspaceAdmissionRefused>),
    // Failure before a Native producer begins still occupies its original
    // one-shot result slot. It is not a successful or uncertain Native effect.
    Admission(anyhow::Error),
}
impl<T, E: std::error::Error + Send + Sync + 'static> RelocationProducer<T, E> {
    pub(crate) fn ordinary(result: Result<T, E>) -> Self { Self::Ordinary(result) }
    pub(crate) fn workspace(result: Result<OfflineRetainedResult<T, E>, OfflineWorkspaceAdmissionRefused>) -> Self {
        Self::Workspace(result)
    }
    pub(crate) fn admission(error: anyhow::Error) -> Self { Self::Admission(error) }
    pub(crate) fn result(&self) -> Result<&T, &(dyn std::error::Error + Send + Sync)> {
        match self {
            Self::Ordinary(result) => result.as_ref().map_err(|error| error as _),
            Self::Workspace(Ok(result)) => result.result().as_ref().map_err(|error| error as _),
            Self::Workspace(Err(error)) => Err(error),
            Self::Admission(error) => Err(error.as_ref()),
        }
    }
    pub(crate) fn is_ok(&self) -> bool { self.result().is_ok() }
    pub(crate) fn value(&self) -> anyhow::Result<&T> {
        // Existing owned94 post-producer error reporting remains outside the
        // scratch loan. The original error and its guard stay in this slot.
        self.result().map_err(|error| anyhow::anyhow!("retained94 operation failed: {error:#}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_views_borrow_the_same_output_and_error() {
        let output: RelocationProducer<String, catabolic_store::Error> =
            RelocationProducer::ordinary(Ok("retained output".to_owned()));
        let first = output.result().unwrap();
        assert!(std::ptr::eq(first, output.value().unwrap()));
        assert!(output.is_ok());
        let failure: RelocationProducer<(), catabolic_store::Error> =
            RelocationProducer::ordinary(Err(catabolic_store::Error("retained error".into())));
        assert!(std::ptr::eq(failure.result().unwrap_err(), failure.result().unwrap_err()));
        assert!(!failure.is_ok());
        assert!(failure.value().unwrap_err().to_string().contains("retained error"));
    }
    #[test]
    fn admission_and_workspace_refusal_are_not_successful_outputs() {
        let admission: RelocationProducer<(), catabolic_store::Error> =
            RelocationProducer::admission(anyhow::anyhow!("preflight refused"));
        assert!(!admission.is_ok());
        assert!(admission.value().is_err());
        let workspace: RelocationProducer<(), catabolic_store::Error> =
            RelocationProducer::workspace(Err(OfflineWorkspaceAdmissionRefused));
        assert!(!workspace.is_ok());
        assert!(workspace.value().is_err());
    }
}
