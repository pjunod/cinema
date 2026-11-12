//! Removed paths (pipeline contract amendment `removed_paths`): files, or
//! emptied folders, that a caller moved out of a library folder and that
//! plurx should forget without walking anything to find out.
//!
//! A removed path that does not exist is the expected case, not
//! `path_unreadable`. What must never happen is an unreadable share reading
//! as "everything is gone", so the probe tells absence (`ENOENT`, `ENOTDIR`)
//! apart from every other error, refuses a library root it cannot list or
//! that lists nothing (an empty mount point), and never follows a symlink.
//! Removal goes through the same fenced, root-identity-checked
//! [`PublicationStore::reconcile_library`] a full scan uses, bounded to the
//! named files, so a node whose mount is not the library's recorded identity
//! removes nothing.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::containment::lexical;
use super::library_root_fingerprint;
use crate::domain::Library;
use crate::error::StoreError;
use crate::store::scan_request_v2::PlacedReport;
use crate::store::{PublicationStore, ReconcileOutcome, MAX_RECONCILE_FILE_IDS};

/// What the disk says about one removed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovedProbe {
    /// Not there. `spellings` are the forms a catalogued path for it can
    /// take: as sent, and with its deepest existing ancestor canonicalised.
    Absent { spellings: Vec<PathBuf> },
    /// Something is still there (or a symlink stands in the chain, which is
    /// never followed); nothing is removed.
    Present,
}

/// The chain from the library root to a removed path could not be read:
/// `EIO`, `ENOTCONN`, `ESTALE`, a permission error, or a root that cannot be
/// listed or is empty. Transient, and nothing is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedUnreadable {
    pub root: PathBuf,
    pub error: String,
}

fn absent(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Whether `root` is listable and lists something. An empty mount point is
/// how an unmounted share looks, and a removed path under it would read as
/// absent. Blocking.
pub fn root_listable(root: &Path) -> Result<(), RemovedUnreadable> {
    let unreadable = |error: String| RemovedUnreadable {
        root: root.to_path_buf(),
        error,
    };
    let mut entries = std::fs::read_dir(root).map_err(|error| unreadable(error.to_string()))?;
    match entries.next() {
        Some(Ok(_)) => Ok(()),
        Some(Err(error)) => Err(unreadable(error.to_string())),
        None => Err(unreadable(
            "the library root is empty, which is how an unmounted share looks; \
             nothing is removed while it is"
                .to_owned(),
        )),
    }
}

/// Probe `path`, which lies lexically under the configured `root`, with
/// `lstat` from the root down: absent at the first missing component,
/// present if the whole chain exists. Blocking.
pub fn probe_removed(root: &Path, path: &Path) -> Result<RemovedProbe, RemovedUnreadable> {
    let root = lexical(root);
    let path = lexical(path);
    let unreadable = |error: std::io::Error| RemovedUnreadable {
        root: root.clone(),
        error: error.to_string(),
    };
    let below = path.strip_prefix(&root).map_err(|_| RemovedUnreadable {
        root: root.clone(),
        error: format!("`{}` is not under this root", path.display()),
    })?;
    let mut existing = root.clone();
    let components: Vec<_> = below.components().collect();
    for (index, component) in components.iter().enumerate() {
        let at = existing.join(component);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Ok(RemovedProbe::Present),
            Ok(_) => existing = at,
            Err(error) if absent(&error) => {
                let mut canonical = existing.canonicalize().map_err(unreadable)?;
                for rest in &components[index..] {
                    canonical.push(rest);
                }
                let mut spellings = vec![path.clone()];
                if canonical != path {
                    spellings.push(canonical);
                }
                return Ok(RemovedProbe::Absent { spellings });
            }
            Err(error) => return Err(unreadable(error)),
        }
    }
    Ok(RemovedProbe::Present)
}

/// One absent removed path to forget.
#[derive(Debug, Clone)]
pub struct AbsentPath {
    /// As the caller sent it; the report names it so.
    pub path: String,
    pub spellings: Vec<PathBuf>,
}

/// What forgetting the absent paths did.
#[derive(Debug, Clone, Default)]
pub struct Removal {
    /// One per absent path: `gone`, `unknown` or `retained`.
    pub reports: Vec<PlacedReport>,
    pub deleted_files: u64,
    pub pruned_items: u64,
}

/// One absent path that had catalogue records.
struct Target {
    /// As the caller sent it.
    path: String,
    /// Every file recorded at it (or below it, for a folder).
    files: Vec<(i64, PathBuf)>,
    /// The item owning the first of them.
    item_id: i64,
}

/// Remove the catalogue files recorded at (or, for a folder, under) each
/// absent path, and the items that leaves with no files.
pub async fn remove_absent(
    store: &PublicationStore<'_>,
    library: &Library,
    absent: &[AbsentPath],
) -> Result<Removal, StoreError> {
    let mut removal = Removal::default();
    let mut library_files: Option<Vec<(i64, PathBuf)>> = None;
    let mut targets: Vec<Target> = Vec::new();
    for entry in absent {
        let mut files: Vec<(i64, PathBuf)> = Vec::new();
        let mut item_id = None;
        for spelling in &entry.spellings {
            if let Some(file) = store.get_file_by_path(&spelling.to_string_lossy()).await? {
                item_id = Some(file.item_id);
                files.push((file.id, file.path));
                break;
            }
        }
        if files.is_empty() {
            // A folder: every catalogued file below it, compared by component
            // so `/m/Dune` does not take `/m/Dune 2`.
            if library_files.is_none() {
                library_files = Some(store.library_file_paths(library.id).await?);
            }
            let known = library_files.as_deref().unwrap_or_default();
            for spelling in &entry.spellings {
                files.extend(
                    known
                        .iter()
                        .filter(|(_, path)| lexical(path).starts_with(spelling))
                        .cloned(),
                );
                if !files.is_empty() {
                    break;
                }
            }
            files.sort();
            if let Some((_, first)) = files.first() {
                item_id = store
                    .get_file_by_path(&first.to_string_lossy())
                    .await?
                    .map(|file| file.item_id);
            }
        }
        match item_id {
            Some(item_id) if !files.is_empty() => targets.push(Target {
                path: entry.path.clone(),
                files,
                item_id,
            }),
            _ => removal.reports.push(PlacedReport::Unknown {
                path: entry.path.clone(),
            }),
        }
    }
    if targets.is_empty() {
        // Every path is `unknown`, already in the caller's order.
        return Ok(removal);
    }

    let paths = library.paths.clone();
    let fingerprint = tokio::task::spawn_blocking(move || library_root_fingerprint(&paths))
        .await
        .map_err(|error| {
            StoreError::Task(format!("library root fingerprint task failed: {error}"))
        })?;
    let mut refused: Option<String> = None;
    match fingerprint {
        Err(error) => refused = Some(error.to_string()),
        Ok(fingerprint) => {
            let mut ids: Vec<i64> = targets
                .iter()
                .flat_map(|target| target.files.iter().map(|(id, _)| *id))
                .collect();
            ids.sort_unstable();
            ids.dedup();
            for batch in ids.chunks(MAX_RECONCILE_FILE_IDS) {
                match store
                    .reconcile_library(library.id, &fingerprint, batch, batch.len() as u64)
                    .await?
                {
                    ReconcileOutcome::Applied {
                        deleted_files,
                        pruned_items,
                    } => {
                        removal.deleted_files += deleted_files;
                        removal.pruned_items += pruned_items;
                    }
                    ReconcileOutcome::RefusedRoot { expected } => {
                        refused = Some(format!(
                            "this node's library root identity does not match the one recorded \
                             for the library ({expected}); a full scan establishes it"
                        ));
                        break;
                    }
                    ReconcileOutcome::RefusedPrune { requested, limit } => {
                        refused = Some(format!(
                            "removal of {requested} files refused by the bound of {limit}"
                        ));
                        break;
                    }
                }
            }
        }
    }

    for Target {
        path,
        files,
        item_id,
    } in targets
    {
        let mut kept = false;
        for (_, file) in &files {
            if store
                .get_file_by_path(&file.to_string_lossy())
                .await?
                .is_some()
            {
                kept = true;
                break;
            }
        }
        if kept {
            removal.reports.push(PlacedReport::Retained {
                path,
                item_id,
                reason: refused.clone().unwrap_or_else(|| {
                    "a running or verified Dolby Vision conversion holds the file".to_owned()
                }),
            });
        } else {
            removal.reports.push(PlacedReport::Gone {
                path,
                item_id,
                item_removed: store.get_item(item_id).await?.is_none(),
            });
        }
    }
    // In the order the caller named them.
    removal.reports.sort_by_key(|report| {
        absent
            .iter()
            .position(|entry| entry.path == report.path())
            .unwrap_or(usize::MAX)
    });
    Ok(removal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absence — a missing component, or a file where a folder should be —
    /// is `Absent` with the canonical spelling a catalogued path would have;
    /// an existing path or a symlink in the chain is `Present` (never
    /// followed); an empty root is refused as a possible unmounted share.
    #[test]
    fn removed_paths_probe_absence_apart_from_unreadable_roots_and_never_follow_links() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).expect("root");
        assert!(
            root_listable(&root).is_err(),
            "an empty root may be an unmounted share"
        );
        std::fs::create_dir_all(root.join("Heat (1995)")).expect("heat");
        std::fs::write(root.join("Heat (1995)").join("Heat.mkv"), b"x").expect("write");
        root_listable(&root).expect("a root that lists something");
        assert!(root_listable(&dir.path().join("absent")).is_err());

        let gone = root.join("Dune (1984)").join("Dune.mkv");
        let canonical_root = root.canonicalize().expect("canonical");
        match probe_removed(&root, &gone).expect("probe") {
            RemovedProbe::Absent { spellings } => {
                assert_eq!(spellings[0], gone);
                assert!(spellings
                    .iter()
                    .any(|path| path == &canonical_root.join("Dune (1984)").join("Dune.mkv")));
            }
            other => panic!("{other:?}"),
        }
        let through_file = root.join("Heat (1995)").join("Heat.mkv").join("inner");
        assert!(matches!(
            probe_removed(&root, &through_file).expect("ENOTDIR is absence"),
            RemovedProbe::Absent { .. }
        ));
        assert_eq!(
            probe_removed(&root, &root.join("Heat (1995)").join("Heat.mkv")).expect("probe"),
            RemovedProbe::Present
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().expect("outside");
            std::os::unix::fs::symlink(outside.path(), root.join("link")).expect("link");
            assert_eq!(
                probe_removed(&root, &root.join("link").join("gone.mkv")).expect("probe"),
                RemovedProbe::Present,
                "a symlink is never followed to decide absence"
            );
        }
        assert!(probe_removed(&root, Path::new("/elsewhere/x.mkv")).is_err());
    }
}
