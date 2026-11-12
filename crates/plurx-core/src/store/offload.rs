//! Offloaded items: Curator deleted a title's files to free disk and cinema
//! keeps the item, its watch history and its metadata, greyed out, until
//! Get it back brings files again (`docs/INTEGRATION.md`, "Offload").
//!
//! One column does it: `items.offloaded_at` (unix seconds). It is set only
//! when an offload leaves an item with no files, and it is cleared by a
//! trigger on `files` whenever a file attaches to the item, so no attach path
//! (scan, import, repair, move) can forget to.
//!
//! The one thing every item-deleting prune must honour is that a file-less
//! leaf with `offloaded_at` set is *kept*: [`fileless_prunable_leaf!`] is the
//! single SQL predicate for "a file-less leaf that may be pruned", and
//! `every_leaf_prune_uses_the_shared_predicate` fails on any
//! `DELETE FROM items` under `store/` that neither uses it nor is listed as
//! intentionally different.

/// `kind IN (...) AND offloaded_at IS NULL AND id NOT IN (SELECT item_id ...)`:
/// the leaves a prune may delete for having no files. Usable in `concat!`,
/// `format!` arguments and `const` SQL; it names no table alias, so it applies
/// to a bare `FROM items`.
macro_rules! fileless_prunable_leaf {
    () => {
        "kind IN ('movie','episode','video','photo','book','audiobook') \
         AND offloaded_at IS NULL AND id NOT IN (SELECT item_id FROM files)"
    };
}
pub(crate) use fileless_prunable_leaf;

/// The SQLite migration introducing the column and the attach triggers.
pub(crate) const SQLITE_INTRODUCED_SCHEMA: i64 = 130;
pub(crate) const COLUMN_SQL: &str = "ALTER TABLE items ADD COLUMN offloaded_at INTEGER";
/// A file attaching (insert, or moving to another item) ends the offload.
pub(crate) const TRIGGER_SQL: [&str; 2] = [
    "CREATE TRIGGER IF NOT EXISTS items_offloaded_clear_on_file_insert \
     AFTER INSERT ON files BEGIN \
       UPDATE items SET offloaded_at = NULL \
        WHERE id = NEW.item_id AND offloaded_at IS NOT NULL; \
     END",
    "CREATE TRIGGER IF NOT EXISTS items_offloaded_clear_on_file_move \
     AFTER UPDATE OF item_id ON files BEGIN \
       UPDATE items SET offloaded_at = NULL \
        WHERE id = NEW.item_id AND offloaded_at IS NOT NULL; \
     END",
];
/// Whether the column exists (replicated install is idempotent).
pub(crate) const COLUMN_PRESENT_SQL: &str =
    "SELECT COUNT(*) AS count FROM pragma_table_info('items') WHERE name = 'offloaded_at'";

/// The SQLite migration step: the column, then the triggers.
pub(crate) const SQLITE_MIGRATION: &str = concat!(
    "ALTER TABLE items ADD COLUMN offloaded_at INTEGER;\n",
    "CREATE TRIGGER IF NOT EXISTS items_offloaded_clear_on_file_insert \
     AFTER INSERT ON files BEGIN \
       UPDATE items SET offloaded_at = NULL \
        WHERE id = NEW.item_id AND offloaded_at IS NOT NULL; \
     END;\n",
    "CREATE TRIGGER IF NOT EXISTS items_offloaded_clear_on_file_move \
     AFTER UPDATE OF item_id ON files BEGIN \
       UPDATE items SET offloaded_at = NULL \
        WHERE id = NEW.item_id AND offloaded_at IS NOT NULL; \
     END;\n",
);

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// `text[at..at + len]`, shortened to a character boundary.
    fn window(text: &str, at: usize, len: usize) -> &str {
        let mut end = (at + len).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[at..end]
    }

    /// Statements that delete items on purpose without the predicate, each
    /// with why. A new entry here is a decision, not a convenience.
    const INTENTIONAL: &[(&str, &str)] = &[
        (
            "hiqlite_publication.rs",
            "identity repair retires a duplicate item after moving its files to the survivor",
        ),
        (
            "sqlite/publication.rs",
            "identity repair retires a duplicate item after moving its files to the survivor",
        ),
        ("sharing_catalogue_source.rs", "removes one named shared-catalogue item"),
        ("hiqlite.rs", "wipes the catalogue for a restore"),
        ("hiqlite_catalog.rs", "full-text-index triggers"),
        ("tests.rs", "test fixtures"),
    ];

    /// The SQLite migration carries exactly the statements the replicated
    /// install runs, so the two catalogues cannot drift.
    #[test]
    fn the_sqlite_migration_and_the_replicated_install_agree() {
        assert!(super::SQLITE_MIGRATION.contains(super::COLUMN_SQL));
        for trigger in super::TRIGGER_SQL {
            assert!(super::SQLITE_MIGRATION.contains(trigger), "{trigger}");
        }
    }

    /// Every `DELETE FROM items` that decides by "has no files" goes through
    /// the shared predicate, so an offloaded item can never be pruned.
    #[test]
    fn every_leaf_prune_uses_the_shared_predicate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        let mut offenders = Vec::new();
        for file in files {
            let rel = file.strip_prefix(&root).expect("under the source root").to_string_lossy().replace('\\', "/");
            if INTENTIONAL.iter().any(|(name, _)| rel.ends_with(name)) || rel == "store/offload.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("readable source");
            for (at, _) in text.match_indices("DELETE FROM items WHERE") {
                let window = window(&text, at, 700);
                let end = window[1..].find("DELETE FROM").map_or(window.len(), |i| i + 1);
                let statement = &window[..end];
                // Only the statements that decide by "no files"; seasons,
                // shows and folders are kept alive by their offloaded child.
                let leaf = statement.contains("FROM files");
                if leaf
                    && !(statement.contains("fileless_prunable_leaf")
                        || statement.contains("offloaded_at"))
                {
                    offenders.push(format!("{rel}: {}", statement.lines().next().unwrap_or("")));
                }
            }
            for (at, _) in text.match_indices("DELETE FROM items\n") {
                let statement = window(&text, at, 400);
                if statement.contains("FROM files")
                    && !(statement.contains("fileless_prunable_leaf")
                        || statement.contains("offloaded_at"))
                {
                    offenders.push(format!("{rel}: multi-line DELETE FROM items"));
                }
            }
        }
        assert!(offenders.is_empty(), "prunes that can delete offloaded items: {offenders:#?}");
    }
}
