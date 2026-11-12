//! Removed paths (pipeline contract amendment `removed_paths`): files, or
//! emptied folders, that a caller moved out of a library folder and that
//! plurx should forget without walking anything to find out.
//!
//! A removed path that does not exist is the expected case, not
//! `path_unreadable`. What must never happen is an unreadable share reading
//! as "everything is gone", so the probe tells absence (`ENOENT`, `ENOTDIR`)
//! apart from every other error, refuses a library root it cannot list or
//! that lists nothing on the parent's own filesystem (an empty mount point),
//! refuses a chain whose deepest existing folder is on another filesystem
//! than the root (a nested or auto mount that may be away), and never follows
//! a symlink. Removal goes through the same fenced, root-identity-checked
//! [`PublicationStore::reconcile_library`] a full scan uses, bounded to the
//! named files, so a node whose mount is not the library's recorded identity
//! removes nothing.
//!
//! A library root that is mounted but empty (its last file just moved out) is
//! told from an unmounted mount point by its filesystem: a live mount's device
//! differs from its parent folder's, a bare mount point's does not. An empty
//! root on its parent's own filesystem stays `path_unreadable` until it holds
//! a file again, because nothing on this node can tell it from an unmounted
//! share; the next placement or full scan clears it.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::containment::lexical;
use super::library_root_fingerprint;
use crate::domain::Library;
use crate::error::StoreError;
use crate::store::scan_request_v2::{PlacedReport, PresentReason, RetainedCode};
use crate::store::{PublicationStore, ReconcileOutcome, MAX_RECONCILE_FILE_IDS};

/// What the disk says about one removed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovedProbe {
    /// Not there. `spellings` are the forms a catalogued path for it can
    /// take: as sent, and with its deepest existing ancestor canonicalised.
    Absent { spellings: Vec<PathBuf> },
    /// Something is still there, a symlink stands in the chain (never
    /// followed), or the name exists only under case folding; nothing is
    /// removed.
    Present { reason: PresentReason },
}

/// The chain from the library root to a removed path could not be read:
/// `EIO`, `ENOTCONN`, `ESTALE`, a permission error, a root that cannot be
/// listed or is empty on its parent's filesystem, or a nested mount.
/// Transient, and nothing is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedUnreadable {
    pub root: PathBuf,
    pub error: String,
}

fn absent(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// The filesystem a path is on, where the platform says.
#[cfg(unix)]
fn device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|meta| meta.dev())
}

#[cfg(not(unix))]
fn device(_path: &Path) -> Option<u64> {
    None
}

/// Whether `path` is a mount point: its device differs from its parent's.
fn is_mount_point(path: &Path) -> bool {
    match (device(path), path.parent().and_then(device)) {
        (Some(here), Some(parent)) => here != parent,
        _ => false,
    }
}

/// Whether `root` is listable and either lists something or is a live mount
/// that is simply empty. An empty plain folder is how an unmounted share
/// looks, and a removed path under it would read as absent. Blocking.
pub fn root_listable(root: &Path) -> Result<(), RemovedUnreadable> {
    let unreadable = |error: String| RemovedUnreadable {
        root: root.to_path_buf(),
        error,
    };
    let mut entries = std::fs::read_dir(root).map_err(|error| unreadable(error.to_string()))?;
    match entries.next() {
        Some(Ok(_)) => Ok(()),
        Some(Err(error)) => Err(unreadable(error.to_string())),
        None if is_mount_point(root) => Ok(()),
        None => Err(unreadable(
            "the library root is empty and is not a mount point of its own, which is how an \
             unmounted share looks; nothing is removed while it is"
                .to_owned(),
        )),
    }
}

/// Whether `dir` lists `name` only with other capitals: the lookup that found
/// it was folding case.
fn case_variant(dir: &Path, name: &std::ffi::OsStr) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let wanted = name.to_string_lossy().to_lowercase();
    let mut folded = false;
    for entry in entries.flatten() {
        let listed = entry.file_name();
        if listed == name {
            return false;
        }
        folded |= listed.to_string_lossy().to_lowercase() == wanted;
    }
    folded
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
            Ok(meta) if meta.file_type().is_symlink() => {
                return Ok(RemovedProbe::Present {
                    reason: PresentReason::SymlinkInChain,
                })
            }
            Ok(_) => {
                if index + 1 == components.len() && case_variant(&existing, component.as_os_str()) {
                    return Ok(RemovedProbe::Present {
                        reason: PresentReason::CaseVariant,
                    });
                }
                existing = at;
            }
            Err(error) if absent(&error) => {
                // The deepest folder that exists must be on the root's own
                // filesystem: a nested or auto mount that is away leaves its
                // mount point behind, and everything under it would read as
                // gone.
                if let (Some(here), Some(top)) = (device(&existing), device(&root)) {
                    if here != top {
                        return Err(RemovedUnreadable {
                            root: root.clone(),
                            error: format!(
                                "`{}` is on a different filesystem than the library root; \
                                 a nested mount may be away, so nothing under it is removed",
                                existing.display()
                            ),
                        });
                    }
                }
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
    Ok(RemovedProbe::Present {
        reason: PresentReason::Exists,
    })
}

/// One absent removed path to forget.
#[derive(Debug, Clone)]
pub struct AbsentPath {
    /// As the caller sent it; the report names it so.
    pub path: String,
    pub spellings: Vec<PathBuf>,
    /// The configured library root it lies under, for the second look just
    /// before removal.
    pub root: PathBuf,
}

/// What forgetting the absent paths did.
#[derive(Debug, Clone, Default)]
pub struct Removal {
    /// One per absent path: `gone`, `unknown`, `present` or `retained`.
    pub reports: Vec<PlacedReport>,
    pub deleted_files: u64,
    pub pruned_items: u64,
}

/// One absent path that had catalogue records.
struct Target {
    /// As the caller sent it.
    path: String,
    root: PathBuf,
    /// Every file recorded at it (or below it, for a folder).
    files: Vec<(i64, PathBuf)>,
    /// The item owning the first of them.
    item_id: i64,
    /// Every distinct item owning one of them, in file order.
    item_ids: Vec<i64>,
}

fn retained(target: &Target, code: RetainedCode, reason: String) -> PlacedReport {
    PlacedReport::Retained {
        path: target.path.clone(),
        item_id: target.item_id,
        code,
        retryable: code.retryable(),
        reason,
    }
}

/// Remove the catalogue files recorded at (or, for a folder, under) each
/// absent path, and the items that leaves with no files. With
/// `expected_not_placed` nothing is removed: an expected file of the same
/// request is not indexed, and the old copy stays until the new one is.
pub async fn remove_absent(
    store: &PublicationStore<'_>,
    library: &Library,
    absent: &[AbsentPath],
    expected_not_placed: bool,
) -> Result<Removal, StoreError> {
    let mut removal = Removal::default();
    let mut targets: Vec<Target> = Vec::new();
    for entry in absent {
        let mut files: Vec<(i64, PathBuf)> = Vec::new();
        let mut item_ids: Vec<i64> = Vec::new();
        for spelling in &entry.spellings {
            if let Some(file) = store.get_file_by_path(&spelling.to_string_lossy()).await? {
                item_ids.push(file.item_id);
                files.push((file.id, file.path));
                break;
            }
        }
        if files.is_empty() {
            // A folder: every catalogued file below it, matched by component
            // so `/m/Dune` does not take `/m/Dune (2021)`.
            for spelling in &entry.spellings {
                files.extend(store.library_file_paths_under(library.id, spelling).await?);
                if !files.is_empty() {
                    break;
                }
            }
            files.sort();
            for (_, file) in &files {
                if let Some(found) = store.get_file_by_path(&file.to_string_lossy()).await? {
                    if !item_ids.contains(&found.item_id) {
                        item_ids.push(found.item_id);
                    }
                }
            }
        }
        match item_ids.first().copied() {
            Some(item_id) if !files.is_empty() => targets.push(Target {
                path: entry.path.clone(),
                root: entry.root.clone(),
                files,
                item_id,
                item_ids,
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

    // Held, not refused: the removal waits for a request whose new file is
    // indexed.
    if expected_not_placed {
        for target in &targets {
            removal.reports.push(retained(
                target,
                RetainedCode::ExpectedNotPlaced,
                "an expected file of this request was not placed, so the old record stays \
                 until the new file is indexed"
                    .to_owned(),
            ));
        }
        sort_in_request_order(&mut removal.reports, absent);
        return Ok(removal);
    }

    // The second look, as late as it can be: a file put back since the first
    // probe, or a chain that stopped answering, removes nothing.
    let looking: Vec<(PathBuf, PathBuf)> = targets
        .iter()
        .map(|target| (target.root.clone(), PathBuf::from(&target.path)))
        .collect();
    let looked = tokio::task::spawn_blocking(move || {
        looking
            .iter()
            .map(|(root, path)| probe_removed(root, path))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|error| StoreError::Task(format!("removed path re-probe task failed: {error}")))?;
    let mut removable: Vec<Target> = Vec::new();
    for (target, looked) in targets.into_iter().zip(looked) {
        match looked {
            Ok(RemovedProbe::Absent { .. }) => removable.push(target),
            Ok(RemovedProbe::Present { .. }) => removal.reports.push(PlacedReport::Present {
                path: target.path,
                reason: PresentReason::ReappearedBeforeRemoval,
            }),
            Err(error) => removal.reports.push(retained(
                &target,
                RetainedCode::ChainUnreadable,
                error.error,
            )),
        }
    }
    let targets = removable;
    if targets.is_empty() {
        sort_in_request_order(&mut removal.reports, absent);
        return Ok(removal);
    }

    let paths = library.paths.clone();
    let fingerprint = tokio::task::spawn_blocking(move || library_root_fingerprint(&paths))
        .await
        .map_err(|error| {
            StoreError::Task(format!("library root fingerprint task failed: {error}"))
        })?;
    let mut refused: Option<(RetainedCode, String)> = None;
    match fingerprint {
        Err(error) => {
            refused = Some((RetainedCode::RootIdentityUnavailable, error.to_string()));
        }
        Ok(fingerprint) => {
            let mut ids: Vec<i64> = targets
                .iter()
                .flat_map(|target| target.files.iter().map(|(id, _)| *id))
                .collect();
            ids.sort_unstable();
            ids.dedup();
            for batch in ids.chunks(MAX_RECONCILE_FILE_IDS) {
                // The fourth argument is the most files this call may delete,
                // not a ratio of the library: the caller named these files, so
                // the bound is exactly them. The deletion is still limited to
                // `batch`, the root identity is still checked, and a request
                // names at most 64 paths.
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
                        refused = Some(if expected == "<unregistered>" {
                            (
                                RetainedCode::RootIdentityUnset,
                                "no root identity is recorded for this library yet; a full scan \
                                 establishes it"
                                    .to_owned(),
                            )
                        } else {
                            (
                                RetainedCode::RootIdentityMismatch,
                                format!(
                                    "this node's library root identity does not match the one \
                                     recorded for the library ({expected})"
                                ),
                            )
                        });
                        break;
                    }
                    ReconcileOutcome::RefusedPrune { requested, limit } => {
                        refused = Some((
                            RetainedCode::PruneBound,
                            format!("removal of {requested} files refused by the bound of {limit}"),
                        ));
                        break;
                    }
                }
            }
        }
    }

    for target in targets {
        let mut kept = false;
        for (_, file) in &target.files {
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
            let (code, reason) = refused.clone().unwrap_or_else(|| {
                (
                    RetainedCode::RecordKept,
                    "the record is still there after the removal; the one exclusion the removal \
                     makes is a running or verified Dolby Vision conversion holding the file"
                        .to_owned(),
                )
            });
            removal.reports.push(retained(&target, code, reason));
        } else {
            let mut item_removed = true;
            for id in &target.item_ids {
                item_removed &= store.get_item(*id).await?.is_none();
            }
            removal.reports.push(PlacedReport::Gone {
                item_ids: target.item_ids,
                item_id: target.item_id,
                path: target.path,
                item_removed,
            });
        }
    }
    sort_in_request_order(&mut removal.reports, absent);
    Ok(removal)
}

/// In the order the caller named them.
fn sort_in_request_order(reports: &mut [PlacedReport], absent: &[AbsentPath]) {
    reports.sort_by_key(|report| {
        absent
            .iter()
            .position(|entry| entry.path == report.path())
            .unwrap_or(usize::MAX)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absence — a missing component, or a file where a folder should be —
    /// is `Absent` with the canonical spelling a catalogued path would have;
    /// an existing path is `Present` (`exists`), a symlink in the chain is
    /// `Present` (`symlink_in_chain`, never followed); an empty root on its
    /// parent's filesystem is refused as a possible unmounted share.
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
            RemovedProbe::Present {
                reason: PresentReason::Exists
            }
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().expect("outside");
            std::os::unix::fs::symlink(outside.path(), root.join("link")).expect("link");
            assert_eq!(
                probe_removed(&root, &root.join("link").join("gone.mkv")).expect("probe"),
                RemovedProbe::Present {
                    reason: PresentReason::SymlinkInChain
                },
                "a symlink is never followed to decide absence"
            );
        }
        assert!(probe_removed(&root, Path::new("/elsewhere/x.mkv")).is_err());
    }

    /// A name that exists only because the filesystem folds case is told
    /// apart from one that exists as sent. (Linux does not fold, so the
    /// lookup helper is exercised directly with the wrong capitals.)
    #[test]
    fn a_case_only_difference_is_a_case_variant_not_a_plain_presence() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("Dune.mkv"), b"x").expect("write");
        assert!(case_variant(dir.path(), "dune.mkv".as_ref()));
        assert!(!case_variant(dir.path(), "Dune.mkv".as_ref()), "exact name");
        assert!(!case_variant(dir.path(), "Other.mkv".as_ref()));
    }

    /// An empty root that is a live mount (its device differs from its
    /// parent's) is not an unmounted share; a nested mount whose mount point
    /// is all that is left refuses the removal. Uses `/proc`, a mount on every
    /// Linux host, and is skipped elsewhere.
    #[test]
    #[cfg(target_os = "linux")]
    fn mounts_are_told_from_plain_folders_by_their_device() {
        if !Path::new("/proc/self").exists() {
            return;
        }
        assert!(is_mount_point(Path::new("/proc")));
        let dir = tempfile::tempdir().expect("tmp");
        assert!(!is_mount_point(dir.path()));
        // Root `/` with a removed path under `/proc`: the deepest existing
        // folder (`/proc`) is another filesystem than the root's.
        let nested = probe_removed(Path::new("/"), Path::new("/proc/plurx-no-such-entry/x.mkv"));
        assert!(
            matches!(&nested, Err(error) if error.error.contains("different filesystem")),
            "{nested:?}"
        );
        // A path absent directly under the root itself is plain absence.
        assert!(matches!(
            probe_removed(Path::new("/"), Path::new("/plurx-no-such-entry/x.mkv")),
            Ok(RemovedProbe::Absent { .. })
        ));
    }

    /// A permission error on the chain is never read as absence.
    #[test]
    #[cfg(unix)]
    fn a_permission_error_on_the_chain_is_unreadable_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        // A filesystem that refuses chmod (some shared mounts) cannot stage
        // the case; `/dev/shm` usually can.
        let base = Path::new("/dev/shm");
        let dir = if base.is_dir() {
            tempfile::tempdir_in(base).expect("tmp")
        } else {
            tempfile::tempdir().expect("tmp")
        };
        let root = dir.path().join("tv");
        let locked = root.join("Locked");
        std::fs::create_dir_all(&locked).expect("locked");
        std::fs::write(root.join("keep.mkv"), b"x").expect("write");
        if std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).is_err() {
            return;
        }
        let readable_anyway = std::fs::read_dir(&locked).is_ok();
        let probed = probe_removed(&root, &locked.join("Show.mkv"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        if readable_anyway {
            return; // running as a user the mode does not bind
        }
        assert!(
            matches!(&probed, Err(error) if error.error.to_lowercase().contains("permission")),
            "{probed:?}"
        );
    }
}
