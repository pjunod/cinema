//! plurx core: the storage boundary, configuration, and shared domain types.
//!
//! The single most load-bearing decision in this crate is the [`store::Store`]
//! trait: *all* replicated durable state (users, settings, library metadata,
//! watch state) is accessed through it. Phase 0–2 run a single-node SQLite
//! backend; Phase 3–4 swap in a raft-replicated backend (hiqlite, or
//! openraft + SQLite) behind the same trait. Nothing outside this crate may
//! assume which backend is in play. See `docs/ARCHITECTURE.md` §2.

// Production children go through `process::spawn_job_owned`; see clippy.toml.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod auth;
pub mod cluster;
pub mod config;
pub mod content_analysis;
pub mod domain;
pub mod dvr;
pub mod error;
pub mod fmp4;
#[cfg(unix)]
pub mod fs_secure;
#[cfg(windows)]
pub mod fs_secure_windows;
#[cfg(windows)]
pub use fs_secure_windows as fs_secure;
pub mod channel_subjects;
pub mod library_channels;
pub mod mediafacts;
pub mod metadata;
pub mod playback;
pub mod process;
pub mod scan;
pub mod secrets;
pub mod segplan;
pub mod store;
/// Media fixtures for the test suites, shared so `plurx-core` and `plurxd`
/// cannot drift onto different GOP structures and disagree about what the
/// classifier should have said. Not part of the shipped library.
#[cfg(any(test, feature = "fixtures"))]
#[allow(clippy::disallowed_methods)] // fixtures run ffmpeg directly, never in production
pub mod testfixtures;
pub mod tracks;
pub mod trakt;
pub mod transcode;
