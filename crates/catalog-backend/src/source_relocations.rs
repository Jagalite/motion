//! Private Motion94 relocation control owner. The migration remains unregistered.
//! Native completed-offline receipt authority and current application requester
//! admission are separate from interpretation of saved descriptions.
pub(crate) mod history;
pub(crate) mod image_closure;
// Exact insertion is consumed by the finite intent/ACK owner being integrated;
// it is not a generic SQL capability or a public admission constructor.
#[allow(dead_code)]
mod insertion;

mod coordinator;
mod facade;
pub(crate) mod journal;
pub use crate::offline_control_upgrade::SourceSessionClosePhase;
pub use facade::{
    DispatchedSourceRelocation, SourceRelocationRequester, SourceRelocationSession,
    SourceSessionCloseOperation,
};

#[cfg(test)]
mod tests;

mod facade_image;
pub use facade_image::{
    RestoreDescriptorFailure, RestoreSealSelection, SourceCurrentImage, SourceCurrentImageFailure,
    RetainedCaptureInputCustody, RetainedNativeCaptureCustody, RetainedPairedCaptureCustody, SourceImageCaptureOperation, SourceImageCapturePreparationFailure, SourceRestoreOperation,
};

pub(crate) mod owned_operation;
pub use owned_operation::{OwnedSourceRelocation94, RelocationTargetWriter, SourceRelocation94Request, SourceRelocation94Purpose, SourceRelocation94Phase, SourceRelocation94CommitState};
