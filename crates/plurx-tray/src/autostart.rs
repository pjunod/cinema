//! The tray's XDG autostart preference. The package installs
//! `/etc/xdg/autostart/tv.plurx.Server-autostart.desktop`; the user's choice
//! is a same-named override in `$XDG_CONFIG_HOME/autostart` that the
//! controller writes (`startup set tray on|off`). The session manager already
//! honours `Hidden=true`; the tray re-checks the same resolution at
//! `--autostart` so a stale or partial override can never launch it, and it
//! never writes autostart files itself. This preference is independent of the
//! server's boot policy.
use std::path::{Path, PathBuf};

pub const ENTRY_NAME: &str = "tv.plurx.Server-autostart.desktop";
const MAX_ENTRY_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preference {
    Enabled(PathBuf),
    /// `Hidden=true` or `X-GNOME-Autostart-enabled=false` in the winning file.
    Disabled(PathBuf),
    /// No entry anywhere: the desktop package's entry was removed.
    Missing,
    /// The winning file could not be read or parsed; never launch on doubt.
    Unreadable(PathBuf, String),
}

impl Preference {
    pub fn launches(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }
}

/// XDG Base Directory search order: the user's config home first, then each
/// system config directory. The first existing file wins, even if it says
/// Hidden — that is how a user override masks the system entry.
pub fn search_paths(
    home: Option<&str>,
    config_home: Option<&str>,
    config_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    match config_home.filter(|v| v.starts_with('/')) {
        Some(dir) => paths.push(Path::new(dir).join("autostart").join(ENTRY_NAME)),
        None => {
            if let Some(home) = home.filter(|v| v.starts_with('/')) {
                paths.push(Path::new(home).join(".config/autostart").join(ENTRY_NAME));
            }
        }
    }
    let dirs = config_dirs.filter(|v| !v.is_empty()).unwrap_or("/etc/xdg");
    for dir in dirs.split(':').filter(|d| d.starts_with('/')) {
        paths.push(Path::new(dir).join("autostart").join(ENTRY_NAME));
    }
    paths
}

/// Parses the `[Desktop Entry]` group of a desktop file for the keys that
/// disable autostart. Keys in other groups (actions) do not count.
pub fn entry_enabled(text: &str) -> bool {
    let mut in_entry = false;
    let mut enabled = true;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Hidden", "true") => enabled = false,
            ("X-GNOME-Autostart-enabled", "false") => enabled = false,
            _ => {}
        }
    }
    enabled
}

pub fn resolve(paths: &[PathBuf]) -> Preference {
    for path in paths {
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Preference::Unreadable(path.clone(), e.to_string()),
        };
        if !metadata.is_file() || metadata.len() > MAX_ENTRY_BYTES {
            return Preference::Unreadable(path.clone(), "not a small regular file".into());
        }
        return match std::fs::read_to_string(path) {
            Ok(text) if entry_enabled(&text) => Preference::Enabled(path.clone()),
            Ok(_) => Preference::Disabled(path.clone()),
            Err(e) => Preference::Unreadable(path.clone(), e.to_string()),
        };
    }
    Preference::Missing
}

pub fn current() -> Preference {
    let var = |k: &str| std::env::var(k).ok();
    resolve(&search_paths(
        var("HOME").as_deref(),
        var("XDG_CONFIG_HOME").as_deref(),
        var("XDG_CONFIG_DIRS").as_deref(),
    ))
}
