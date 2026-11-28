//! The M7 source identity: SHA-256 over the source files whose code decides
//! what an M7 evaluation of a title concludes.
//!
//! A settled title's M7 outcome is recorded under the identity that produced
//! it, and the idle reopen sweep re-evaluates an `unsupported` title when the
//! identity changes, so a fix to M7 reaches the titles an older M7 refused
//! without anyone bumping a version by hand. Hashing source is conservative
//! by design: a comment edit also changes it, and costs one re-evaluation of
//! the refused titles. What it misses (a behaviour change outside these files)
//! is what the administrator's "Re-evaluate M7" action is for.
//!
//! The file set, relative to the workspace root (test files excluded: names
//! ending `_tests.rs`, `tests.rs`, and anything under a `tests/` directory):
//!
//! - `crates/plurxd/src/source_probe/m7_*.rs` and every `m7_*/` subdirectory
//! - `crates/plurxd/src/decode_facts/mastering.rs`
//! - `crates/plurx-core/src/transcode/dv_m7_*.rs` and every `dv_m7_*/`
//!   subdirectory (including `dv_m7_scoring/`)
//! - `crates/plurx-core/src/store/dv_m7_*.rs`
//!
//! The digest is SHA-256 over the domain `plurx-m7-source-v1`, then each file
//! in byte order of its relative path as (path, contents), every part
//! prefixed with its length as a big-endian u64.

use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};

pub const DOMAIN: &[u8] = b"plurx-m7-source-v1";

/// Directories whose `prefix*` entries (files `prefix*.rs`, and directories
/// `prefix*`, walked whole) belong to M7.
const PREFIXED: &[(&str, &str)] = &[
    ("crates/plurxd/src/source_probe", "m7_"),
    ("crates/plurx-core/src/transcode", "dv_m7_"),
    ("crates/plurx-core/src/store", "dv_m7_"),
];

/// Single files that belong to M7.
const FILES: &[&str] = &["crates/plurxd/src/decode_facts/mastering.rs"];

/// What [`m7_source_digest`] read: the digest, and every file and directory
/// it walked, for `cargo:rerun-if-changed`.
pub struct M7Source {
    pub sha256: String,
    pub watched: Vec<PathBuf>,
}

fn is_test_file(name: &str) -> bool {
    name == "tests.rs" || name.ends_with("_tests.rs")
}

fn relative(root: &Path, path: &Path) -> io::Result<String> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| io::Error::other("M7 source outside the workspace"))?;
    let parts = rel
        .components()
        .map(|part| {
            part.as_os_str()
                .to_str()
                .ok_or_else(|| io::Error::other("M7 source path is not UTF-8"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(parts.join("/"))
}

fn walk(
    root: &Path,
    dir: &Path,
    files: &mut Vec<(String, PathBuf)>,
    watched: &mut Vec<PathBuf>,
) -> io::Result<()> {
    watched.push(dir.to_path_buf());
    let mut entries = std::fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other("M7 source name is not UTF-8"))?;
        if entry.file_type()?.is_dir() {
            if name != "tests" {
                walk(root, &path, files, watched)?;
            }
        } else if name.ends_with(".rs") && !is_test_file(name) {
            files.push((relative(root, &path)?, path));
        }
    }
    Ok(())
}

/// The M7 source digest of the workspace at `root`.
pub fn m7_source_digest(root: &Path) -> io::Result<M7Source> {
    let mut files = Vec::new();
    let mut watched = Vec::new();
    for (parent, prefix) in PREFIXED {
        let dir = root.join(parent);
        watched.push(dir.clone());
        let mut entries = std::fs::read_dir(&dir)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| name.starts_with(prefix)) else {
                continue;
            };
            if entry.file_type()?.is_dir() {
                walk(root, &path, &mut files, &mut watched)?;
            } else if name.ends_with(".rs") && !is_test_file(name) {
                files.push((relative(root, &path)?, path));
            }
        }
    }
    for file in FILES {
        files.push(((*file).to_owned(), root.join(file)));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files.dedup_by(|left, right| left.0 == right.0);
    if files.is_empty() {
        return Err(io::Error::other("no M7 sources found"));
    }
    let mut hash = Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    part(DOMAIN);
    for (rel, path) in &files {
        part(rel.as_bytes());
        part(&std::fs::read(path)?);
        watched.push(path.clone());
    }
    Ok(M7Source {
        sha256: hex(&hash.finalize()),
        watched,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, body).expect("write");
    }

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temporary workspace");
        for rel in [
            "crates/plurxd/src/source_probe/m7_title.rs",
            "crates/plurxd/src/source_probe/m7_title/recovery.rs",
            "crates/plurxd/src/source_probe/title_work.rs",
            "crates/plurxd/src/decode_facts/mastering.rs",
            "crates/plurx-core/src/transcode/dv_m7_scoring.rs",
            "crates/plurx-core/src/transcode/dv_m7_scoring/capture.rs",
            "crates/plurx-core/src/store/dv_m7_outcomes.rs",
            "crates/plurx-core/src/store/dv_title_jobs.rs",
            // Tests never move the identity.
            "crates/plurxd/src/source_probe/m7_title_tests.rs",
            "crates/plurx-core/src/transcode/dv_m7_scoring/tests.rs",
            "crates/plurx-core/src/transcode/dv_m7_scoring/tests/case.rs",
        ] {
            write(root.path(), rel, rel);
        }
        root
    }

    #[test]
    fn the_m7_source_digest_moves_with_exactly_its_listed_files() {
        let root = fixture();
        let base = m7_source_digest(root.path()).expect("digest");
        assert_eq!(base.sha256.len(), 64);
        assert!(base.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            m7_source_digest(root.path()).expect("again").sha256,
            base.sha256,
            "deterministic"
        );
        assert!(base
            .watched
            .contains(&root.path().join("crates/plurxd/src/source_probe/m7_title")));
        for (rel, moves) in [
            ("crates/plurxd/src/source_probe/m7_title.rs", true),
            ("crates/plurxd/src/source_probe/m7_title/recovery.rs", true),
            ("crates/plurxd/src/decode_facts/mastering.rs", true),
            (
                "crates/plurx-core/src/transcode/dv_m7_scoring/capture.rs",
                true,
            ),
            ("crates/plurx-core/src/store/dv_m7_outcomes.rs", true),
            ("crates/plurxd/src/source_probe/title_work.rs", false),
            ("crates/plurx-core/src/store/dv_title_jobs.rs", false),
            ("crates/plurxd/src/source_probe/m7_title_tests.rs", false),
            (
                "crates/plurx-core/src/transcode/dv_m7_scoring/tests.rs",
                false,
            ),
            (
                "crates/plurx-core/src/transcode/dv_m7_scoring/tests/case.rs",
                false,
            ),
        ] {
            let root = fixture();
            write(root.path(), rel, "changed");
            let changed = m7_source_digest(root.path()).expect("digest").sha256;
            assert_eq!(changed != base.sha256, moves, "{rel}");
        }
        // A new M7 file moves it too.
        let root = fixture();
        write(
            root.path(),
            "crates/plurxd/src/source_probe/m7_new.rs",
            "new",
        );
        assert_ne!(
            m7_source_digest(root.path()).expect("digest").sha256,
            base.sha256
        );
    }
}
