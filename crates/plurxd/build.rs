//! Build-time version stamping.
//!
//! `CARGO_PKG_VERSION` on its own can't tell a tagged release apart from the
//! forty commits that came after it — every one of them reports the same
//! number to every client and every bug report. So capture the git description
//! at build time and hand it to the crate as `PLURX_BUILD`.
//!
//! Nothing here may fail the build. A source tarball, a Docker context without
//! `.git`, or a machine with no `git` on PATH all fail to find a commit, and
//! CI or a package build can inject `PLURX_BUILD_REF` instead of relying on a
//! checkout being present.
//!
//! **When there is no commit to name, say when instead of saying nothing.**
//! `"unknown"` was the old fallback and it is useless in the one situation it
//! occurs in: somebody has just deployed and wants to know whether their change
//! is running. It cannot answer that. A date can -- it does not name the
//! commit, but it distinguishes this deploy from the last one, which is the
//! actual question. So [`BUILT_AT`] is stamped unconditionally and the UI falls
//! back to it whenever the commit is unknown.
//!
//! The stamp is the build's *source date* (`build_support/source_date.rs`):
//! `SOURCE_DATE_EPOCH` when set, else the `HEAD` commit time, else the clock.
//! It used to be the clock always, which made every build of one commit a
//! different binary and an image impossible to reproduce. A malformed
//! `SOURCE_DATE_EPOCH` is the one thing here that fails the build, because a
//! fallback would silently undo what setting it asked for.

// A build script, never a daemon child: the launcher rule in clippy.toml is for
// production code.
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "build_support/source_date.rs"]
mod source_date;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(plurx_dv_segment_probe)");
    println!("cargo:rerun-if-env-changed=PLURX_BUILD_REF");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=build_support/source_date.rs");
    watch_git_head();

    let build = std::env::var("PLURX_BUILD_REF")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=PLURX_BUILD={build}");
    println!(
        "cargo:rustc-env=PLURX_BUILT_AT={}",
        built_at(build.ends_with("-dirty"))
    );
    embed_windows_manifest();
}

fn embed_windows_manifest() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("Cargo always sets CARGO_MANIFEST_DIR"),
    )
    .join("plurxd.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bin=plurxd=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=plurxd=/MANIFESTINPUT:{}",
        manifest.display()
    );
}

/// The source date as `YYYY-MM-DDTHH:MM:SSZ`; see `build_support/source_date.rs`.
fn built_at(dirty: bool) -> String {
    let epoch = std::env::var("SOURCE_DATE_EPOCH").ok();
    let secs = source_date::resolve_source_date(epoch.as_deref(), dirty, git_commit_time, || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
    .unwrap_or_else(|err| panic!("{err}"));
    source_date::format_utc(secs)
}

/// Committer time of `HEAD`, in Unix seconds, when built from a checkout.
fn git_commit_time() -> Option<i64> {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%ct", "HEAD"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").ok()?)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Re-run when HEAD moves (a commit, a checkout, a new tag) so the stamp does
/// not go stale behind a cached build. Emitted only for paths that exist:
/// naming a missing file would make Cargo re-run this script on every single
/// build, which is exactly the case (no `.git`) where there is nothing to
/// re-read anyway.
fn watch_git_head() {
    let Some(root) = repo_root() else { return };
    for rel in ["HEAD", "refs", "packed-refs"] {
        let p = root.join(".git").join(rel);
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
}

/// Walk up from the crate directory looking for the workspace's `.git`.
fn repo_root() -> Option<PathBuf> {
    let mut dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `v0.1.0`, `v0.1.0-14-gc0ffee`, `v0.1.0-14-gc0ffee-dirty`, or — before the
/// first tag exists — the bare short hash.
fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty=-dirty"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").ok()?)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!s.is_empty()).then_some(s)
}
