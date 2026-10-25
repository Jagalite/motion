pub mod administration;
pub mod api;
pub mod artifact_leases;
pub mod artwork;
pub mod catalog;
pub use motion_catalog_backend as catalog_backend;
pub mod components;
pub mod config;
pub mod curation;
pub mod db;
pub mod delivery;
pub mod encoding;
pub mod events;
pub mod execution;
pub mod fmp4;
pub mod interchange;
pub mod libraries;
pub mod mail;
pub mod maintenance;
pub mod markers;
pub mod matching;
pub mod media;
pub mod metadata;
pub mod metrics;
pub mod nfo;
pub mod operations;
pub mod organization;
pub mod playback;
pub mod playback_monitor;
pub mod processing;
pub mod renditions;
pub mod scan;
pub mod scans;
pub mod search;
pub mod signin_protocol;
pub mod storage;
pub mod upgrade;
pub mod v2;
pub mod viewing;
pub mod watch;

use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, Semaphore};

#[derive(Clone)]
pub struct App {
    pub health: Arc<operations::Health>,
    pub db: sqlx::SqlitePool,
    /// Present only after durable native activation has reconciled successfully.
    pub native_catalog: Option<Arc<native_catalog::Runtime>>,
    pub admin_token: Arc<String>,
    pub origin: Arc<String>,
    pub authority: Arc<String>,
    pub ffprobe: Arc<PathBuf>,
    /// Serializes job admission, transitions, and scan publication in this process.
    pub jobs: Arc<Mutex<()>>,
    pub streams: Arc<Semaphore>,
    pub event_streams: Arc<Semaphore>,
    pub processing: Arc<processing::Runtime>,
    pub storage: Arc<storage::Runtime>,
    pub access: Arc<v2::Runtime>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub mod desktop;

pub mod presentation;

pub mod metadata_jobs;
pub mod native_catalog;
pub mod native_discovery;
#[allow(dead_code)]
pub(crate) mod native_generated;
pub mod native_service;
// Unregistered candidate verifier; no production admission constructor.
#[allow(dead_code)] // Native generated publication wiring is not enabled yet.
pub(crate) mod native_processing;
#[allow(dead_code)]
mod native_source_candidates;

pub mod native_scans;

// Isolated companion owner candidate; no schedule routes or tick are enabled.
#[allow(dead_code)]
pub(crate) mod native_schedules;

// Internal watch authority candidate; no routes or startup consumer.
#[allow(dead_code)]
pub(crate) mod native_watch;
