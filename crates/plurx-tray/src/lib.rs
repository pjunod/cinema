//! Linux desktop companion for a natively installed plurx server
//! (docs/features/DESKTOP-TRAY-IMPLEMENTATION.md, task M4).
//!
//! Portable modules hold every decision and are tested on any host:
//! the versioned controller protocol, the bounded `plurx-control` runner,
//! the menu/window model, the effect-based core, logind session eligibility,
//! the XDG autostart preference and the smoke receipt. `linux` is the GTK4
//! control window, the ksni StatusNotifierItem and the D-Bus probes.
pub mod autostart;
pub mod cli;
pub mod command;
pub mod core;
pub mod model;
pub mod protocol;
pub mod receipt;
pub mod runner;
pub mod session;

#[cfg(target_os = "linux")]
pub mod linux;

pub const APP_ID: &str = "tv.plurx.Server";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit the release build stamps; never invented.
pub fn app_build() -> &'static str {
    option_env!("PLURX_BUILD_SHA").unwrap_or("unknown")
}

/// The public release repository whose GitHub release pages hold the notes.
pub fn release_repository() -> &'static str {
    option_env!("PLURX_RELEASE_REPOSITORY").unwrap_or("pjunod/cinema")
}

#[cfg(test)]
mod tests;
