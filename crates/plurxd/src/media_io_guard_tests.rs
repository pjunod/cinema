//! Media descriptors read their identity through `plurx_core::media_io`.
//!
//! `fs_secure::std_file_identity` is an `fstat`. On a hard NFS mount whose
//! server is gone, an `fstat` of a media descriptor parks the calling thread
//! once the attribute cache expires, and on a runtime worker it takes the
//! worker with it (docs/server/MEDIA-IO-ISOLATION-PLAN.md). So every
//! identity read names the kind of descriptor it holds:
//!
//! - `media_io::identity` (bounded, awaited) or `media_io::identity_in_place`
//!   (callers that cannot await) for anything that can be on media storage;
//! - `fs_secure::local_file_identity` for a descriptor on the node's own
//!   disk: engines, models, private scratch, the playback cache.
//!
//! `std_file_identity` itself is left to `fs_secure` and `media_io`. The
//! one other use is pinned below with its reason.

/// (path under `crates/`, why it may call `std_file_identity` directly)
const SANCTIONED: &[(&str, &str)] = &[
    ("plurx-core/src/fs_secure.rs", "the definition"),
    ("plurx-core/src/fs_secure_windows.rs", "the definition"),
    (
        "plurx-core/src/media_io.rs",
        "the media_io identity helpers",
    ),
    (
        "plurxd/src/fragment_index_cluster.rs",
        "windows_object_version runs only inside current_object_version, which \
         SourceFence::drift calls under a media slot and drift_in_place calls \
         through media_io::in_place",
    ),
    ("plurxd/src/media_io_guard_tests.rs", "this guard"),
];

#[test]
fn media_identity_reads_go_through_media_io() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .to_owned();
    let mut direct = Vec::new();
    let mut reached_fs_secure = false;
    let mut directories = vec![crates.join("plurxd/src"), crates.join("plurx-core/src")];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).expect("a source directory") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&crates)
                .expect("under crates")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path).expect("a readable source");
            if !source.contains("std_file_identity") {
                continue;
            }
            reached_fs_secure |= relative == "plurx-core/src/fs_secure.rs";
            // The fence file is pinned to its one call, so a new media call
            // there cannot hide behind the sanctioned one.
            let pinned_fence = relative == "plurxd/src/fragment_index_cluster.rs"
                && source.matches("std_file_identity").count() == 1;
            if !pinned_fence
                && (relative == "plurxd/src/fragment_index_cluster.rs"
                    || !SANCTIONED
                        .iter()
                        .any(|(sanctioned, _)| *sanctioned == relative))
            {
                direct.push(relative);
            }
        }
    }
    assert!(reached_fs_secure, "the walk did not reach fs_secure");
    assert!(
        direct.is_empty(),
        "direct fs_secure::std_file_identity in {direct:?}. Use \
         plurx_core::media_io::identity (or identity_in_place where the caller \
         cannot await) for a descriptor that can be on media storage, or \
         fs_secure::local_file_identity for one on the node's own disk"
    );
}
