//! Deterministic decisions. No clocks, database connections, tasks, or file handles.
pub mod access;
pub mod access_admin;
pub mod artwork;
pub mod catalog;
pub mod catalog_backend;
pub mod catalog_source;
pub mod color;
pub mod components;
pub mod delivery;
pub mod delivery_admission;
pub mod demands;
pub mod downloads;
pub mod events;
pub mod execution_deadline;
pub mod identity;
pub mod interchange;
pub mod jobs;
pub mod mail;
pub mod maintenance;
pub mod markers;
pub mod matching;
pub mod metadata;
pub mod nfo;
pub mod offline;
pub mod organization;
pub mod parts;
pub mod playback;
pub mod playback_monitor;
pub mod playback_session;
pub mod previews;
pub mod processing;
pub mod ranges;
pub mod registration;
pub mod renditions;
pub mod revision;
pub mod scan;
pub mod search;
pub mod sources;
pub mod subtitles;
pub mod timeline_viewing;
pub mod viewing;
pub mod watch;
pub mod work;

pub mod signin;

pub mod catalog_cutover;

pub mod catalog_registration;

pub mod catalog_projection;

pub mod native_scan;

pub mod native_matching;

pub mod catalog_aggregate_revision;

pub mod catalog_command_attempt;

pub mod catalog_kind;

pub mod native_processing_control;
pub mod processing_publication;

pub mod source_lifecycle;

pub mod scan_schedule;

pub mod paired_backup;

pub mod source_succession;

pub mod catalog_identity_lineage;
