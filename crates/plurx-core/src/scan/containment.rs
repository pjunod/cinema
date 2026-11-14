//! Whether a path a caller names belongs to a library, decided in an order
//! that keeps "not ours" apart from "ours, but unreadable right now"
//! (pipeline contract v2 §5.1).
//!
//! Containment is decided **lexically first**: the request path, with `.` and
//! `..` resolved and no symlink followed, against the configured roots. Only
//! then does the filesystem get a say, and what it can say is limited to
//! "readable" or "not readable now": an unmounted share, `ENOTCONN`, `EIO`,
//! `ESTALE` or a path that is missing under a root we know is transient. The
//! one filesystem answer that is permanent is a symlink that resolves outside
//! its root — the same escape the canonical check always refused.
use std::path::{Component, Path, PathBuf};

use crate::domain::Library;

/// The verdict for one path.
#[derive(Debug)]
pub enum PathClass<'a> {
    /// Under no configured root, or a symlink out of the one it is under.
    /// Permanent: retrying cannot change it.
    Outside { roots: Vec<String> },
    /// Under `root` lexically, but the root or the path cannot be resolved
    /// right now. Transient.
    Unreadable { root: PathBuf, error: String },
    /// Under `root`, and it resolves there.
    Inside {
        library: &'a Library,
        root: PathBuf,
        canonical: PathBuf,
    },
}

/// `path` made absolute against nothing — it must already be absolute — with
/// `.` dropped and `..` applied, never following a symlink.
pub fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Every configured root, for the self-explaining 422.
pub fn configured_roots(libraries: &[Library]) -> Vec<String> {
    libraries
        .iter()
        .flat_map(|library| library.paths.iter().map(|root| root.display().to_string()))
        .collect()
}

/// `path`, or — when it does not exist — its nearest ancestor that does,
/// never above the configured root it lies under. Only absence (`ENOENT`,
/// `ENOTDIR`) climbs; any other error stops here, so classification reports
/// it. Blocking.
pub fn nearest_existing(libraries: &[Library], path: &Path) -> PathBuf {
    let mut at = lexical(path);
    let Some((_, root)) = lexical_root(libraries, &at) else {
        return at;
    };
    while at != root {
        match std::fs::symlink_metadata(&at) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                at.pop();
            }
            _ => break,
        }
    }
    at
}

/// The library and configured root that lexically contain `path`; the
/// longest root wins, so `/media/kids` beats `/media`.
pub fn lexical_root<'a>(libraries: &'a [Library], path: &Path) -> Option<(&'a Library, PathBuf)> {
    let path = lexical(path);
    libraries
        .iter()
        .flat_map(|library| {
            library
                .paths
                .iter()
                .map(move |root| (library, lexical(root)))
        })
        // Component-wise: `/data` must not contain `/database`.
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
}

/// Whether any component of `path` strictly below the configured `root` is a
/// symlink, found with `lstat` alone so no link is ever followed. The root
/// itself may be a link; it is configuration. An error means a component
/// cannot be read right now (or is missing). Blocking.
pub fn symlink_below_root(root: &Path, path: &Path) -> std::io::Result<bool> {
    let root = lexical(root);
    let path = lexical(path);
    let Ok(below) = path.strip_prefix(&root) else {
        return Ok(false);
    };
    let mut at = root;
    for component in below.components() {
        at.push(component);
        if std::fs::symlink_metadata(&at)?.file_type().is_symlink() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Classify `path` against `libraries`. Blocking: it may `stat` the root and
/// the path, so async callers run it on a blocking thread.
pub fn classify_path<'a>(libraries: &'a [Library], path: &Path) -> PathClass<'a> {
    let Some((library, root)) = lexical_root(libraries, path) else {
        return PathClass::Outside {
            roots: configured_roots(libraries),
        };
    };
    let canonical_root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return PathClass::Unreadable {
                root,
                error: error.to_string(),
            }
        }
    };
    // A mount point that exists but holds nothing (the share is not
    // mounted over it) canonicalizes; reading it is what tells.
    if let Err(error) = std::fs::read_dir(&canonical_root) {
        return PathClass::Unreadable {
            root,
            error: error.to_string(),
        };
    }
    let canonical = match lexical(path).canonicalize() {
        Ok(canonical) => canonical,
        Err(error) => {
            return PathClass::Unreadable {
                root,
                error: error.to_string(),
            }
        }
    };
    if !canonical.starts_with(&canonical_root) {
        return PathClass::Outside {
            roots: configured_roots(libraries),
        };
    }
    PathClass::Inside {
        library,
        root,
        canonical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LibraryKind;

    fn library(id: i64, roots: &[&Path]) -> Library {
        Library {
            id,
            name: format!("library {id}"),
            kind: LibraryKind::Movies,
            paths: roots.iter().map(|root| root.to_path_buf()).collect(),
            anime: false,
            created_at: 0,
            scan_interval_mins: 0,
            refresh_interval_mins: 0,
            last_scan_at: None,
            last_refresh_at: None,
        }
    }

    #[test]
    fn lexical_containment_comes_before_the_filesystem() {
        let dir = tempfile::tempdir().expect("dir");
        let movies = dir.path().join("movies");
        let kids = movies.join("kids");
        std::fs::create_dir_all(kids.join("Film (2001)")).expect("tree");
        let libraries = vec![library(1, &[&movies]), library(2, &[&kids])];

        assert!(matches!(
            classify_path(&libraries, &dir.path().join("elsewhere/x.mkv")),
            PathClass::Outside { .. }
        ));
        assert!(
            matches!(
                classify_path(&libraries, &movies.join("../elsewhere")),
                PathClass::Outside { .. }
            ),
            "`..` is resolved before containment"
        );
        let sibling = dir.path().join("moviesx");
        std::fs::create_dir_all(&sibling).expect("sibling");
        assert!(
            matches!(
                classify_path(&libraries, &sibling),
                PathClass::Outside { .. }
            ),
            "component-wise, not a string prefix"
        );
        match classify_path(&libraries, &kids.join("Film (2001)")) {
            PathClass::Inside { library, .. } => assert_eq!(library.id, 2, "longest root wins"),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(
                classify_path(&libraries, &movies.join("Not There (1999)")),
                PathClass::Unreadable { .. }
            ),
            "missing under a known root is transient, not outside"
        );

        // An unmounted share: the root itself is gone.
        let share = dir.path().join("share");
        let unmounted = vec![library(3, &[&share])];
        assert!(matches!(
            classify_path(&unmounted, &share.join("Show/Season 1")),
            PathClass::Unreadable { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_below_a_root_is_found_without_following_it() {
        let dir = tempfile::tempdir().expect("dir");
        let movies = dir.path().join("movies");
        std::fs::create_dir_all(movies.join("Film (2001)")).expect("movies");
        std::os::unix::fs::symlink("/nonexistent/target", movies.join("link")).expect("symlink");
        assert!(!symlink_below_root(&movies, &movies.join("Film (2001)")).expect("plain"));
        assert!(
            symlink_below_root(&movies, &movies.join("link/deeper")).expect("dangling link"),
            "found by lstat, never resolved"
        );
        assert!(symlink_below_root(&movies, &movies.join("gone")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_its_root_is_outside() {
        let dir = tempfile::tempdir().expect("dir");
        let movies = dir.path().join("movies");
        let secret = dir.path().join("secret");
        std::fs::create_dir_all(&movies).expect("movies");
        std::fs::create_dir_all(&secret).expect("secret");
        std::os::unix::fs::symlink(&secret, movies.join("escape")).expect("symlink");
        let libraries = vec![library(1, &[&movies])];
        assert!(matches!(
            classify_path(&libraries, &movies.join("escape")),
            PathClass::Outside { .. }
        ));
    }
}
