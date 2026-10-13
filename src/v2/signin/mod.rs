//! Sign-in adapters. Every credential issue, link, proof consumption and
//! revocation commits with the production core decision in a reserved writer.
mod administration;
mod flows;
mod store;
use super::{
    Body, Problem, access_management,
    auth::{self, Caller},
    if_match,
};
use crate::{App, db::begin_write, new_id, now};
pub use administration::*;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
pub use flows::*;
use playscale_core::{
    access::{AccessError, Credential, CredentialKind, Device, Grant, Permission},
    access_admin::{self, User},
    signin as core,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
pub(crate) use store::audit;
pub(crate) use store::require_fresh;
use store::*;

fn jsonstr<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("sign-in state serialization")
}
fn unavailable() -> Problem {
    problem(core::Error::Unavailable)
}
fn problem(error: core::Error) -> Problem {
    Problem::new(
        match error {
            core::Error::RecentProofRequired => StatusCode::FORBIDDEN,
            core::Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            core::Error::IdentityConflict
            | core::Error::LastMethod
            | core::Error::AlreadyUsed
            | core::Error::Stale => StatusCode::CONFLICT,
            _ => StatusCode::BAD_REQUEST,
        },
        error.code(),
        match error {
            core::Error::RecentProofRequired => {
                "Sign in again or approve this specific action from another authorized device"
            }
            core::Error::LastMethod => {
                "Keep an independent usable sign-in method before removing this one"
            }
            _ => "The sign-in attempt cannot continue; start a new attempt",
        },
    )
}
impl From<core::Error> for Problem {
    fn from(e: core::Error) -> Self {
        problem(e)
    }
}
pub(super) async fn device_lineage_valid(
    c: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<bool, Problem> {
    store::family_valid(c, id).await
}
