pub mod administration;
pub mod api;
pub mod artwork;
pub mod catalog;
pub mod config;
pub mod curation;
pub mod db;
pub mod delivery;
pub mod encoding;
pub mod events;
pub mod execution;
pub mod libraries;
pub mod maintenance;
pub mod matching;
pub mod media;
pub mod metadata;
pub mod operations;
pub mod organization;
pub mod playback;
pub mod processing;
pub mod renditions;
pub mod scan;
pub mod storage;
pub mod upgrade;
pub mod v2;
pub mod viewing;

use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, Semaphore};

#[derive(Clone)]
pub struct App {
    pub health: Arc<operations::Health>,
    pub db: sqlx::SqlitePool,
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
