// Test-only source inventory. Rust syntax, including actual module/include!
// edges, establishes ownership; names and quoted source cannot hide a file.
// Unsupported production includes/local modules/conditional paths refuse the
// audit. This finite fixture is not a Rust macro/cfg expansion interpreter.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

pub fn production_files(root: &Path, parents: &[&Path]) -> Vec<PathBuf> {
    fn normalize(path: &Path) -> PathBuf {
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    assert!(result.pop(), "source path parent");
                }
                Component::CurDir => {}
                other => result.push(other.as_os_str()),
            }
        }
        result
    }
    fn walk(directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    // A #[path] external module has a different child-module directory from
    // a default foo.rs module. Keep that compiler resolution context explicitly.
    fn edges(
        items: &[syn::Item],
        base: &Path,
        parent: &Path,
        source_parent: &Path,
        inherited_test: bool,
        output: &mut Vec<(PathBuf, bool, bool)>,
        opaque: &mut bool,
    ) {
        for item in items {
            let syn::Item::Mod(module) = item else {
                struct Includes<'a> {
                    parent: &'a Path,
                    test: bool,
                    output: &'a mut Vec<(PathBuf, bool, bool)>,
                    opaque: &'a mut bool,
                }
                impl<'ast> syn::visit::Visit<'ast> for Includes<'_> {
                    fn visit_macro(&mut self, item: &'ast syn::Macro) {
                        if item.path.is_ident("include") {
                            // Production include! changes module contexts; this
                            // bounded inventory refuses it rather than guessing.
                            if !self.test {
                                *self.opaque = true;
                            }
                            if let Ok(path) = syn::parse2::<syn::LitStr>(item.tokens.clone()) {
                                self.output.push((
                                    normalize(&self.parent.join(path.value())),
                                    self.test,
                                    true,
                                ));
                            }
                        }
                    }
                    fn visit_item_mod(&mut self, _item: &'ast syn::ItemMod) {
                        // Function/local modules need a separate Rust name
                        // context. Unknown production ownership must refuse.
                        if !self.test {
                            *self.opaque = true;
                        }
                    }
                }
                syn::visit::Visit::visit_item(
                    &mut Includes {
                        parent: source_parent,
                        test: inherited_test,
                        output,
                        opaque,
                    },
                    item,
                );
                continue;
            };
            let test = inherited_test
                || module.attrs.iter().any(|attribute| {
                    attribute.path().is_ident("cfg")
                        && attribute
                            .parse_args::<syn::Path>()
                            .is_ok_and(|path| path.is_ident("test"))
                });
            // Conditional path attributes require cfg expansion. This finite
            // inventory refuses opaque production ownership instead of guessing.
            if !test
                && module
                    .attrs
                    .iter()
                    .any(|attribute| attribute.path().is_ident("cfg_attr"))
            {
                *opaque = true;
            }
            let explicit = module.attrs.iter().find_map(|attribute| {
                if !attribute.path().is_ident("path") {
                    return None;
                }
                let syn::Meta::NameValue(value) = &attribute.meta else {
                    return None;
                };
                let syn::Expr::Lit(value) = &value.value else {
                    return None;
                };
                let syn::Lit::Str(value) = &value.lit else {
                    return None;
                };
                Some(normalize(&parent.join(value.value())))
            });
            fn production_inline_path(items: &[syn::Item]) -> bool {
                items.iter().any(|item| {
                    let syn::Item::Mod(module) = item else {
                        return false;
                    };
                    if module.attrs.iter().any(|attribute| {
                        attribute.path().is_ident("cfg")
                            && attribute
                                .parse_args::<syn::Path>()
                                .is_ok_and(|path| path.is_ident("test"))
                    }) {
                        return false;
                    }
                    module.attrs.iter().any(|attribute| {
                        attribute.path().is_ident("path") || attribute.path().is_ident("cfg_attr")
                    }) || module
                        .content
                        .as_ref()
                        .is_some_and(|(_, children)| production_inline_path(children))
                })
            }
            if let Some((_, children)) = &module.content {
                if !test && (explicit.is_some() || production_inline_path(children)) {
                    *opaque = true;
                }
                let directory = explicit.unwrap_or_else(|| base.join(module.ident.to_string()));
                edges(
                    children,
                    &directory,
                    &directory,
                    source_parent,
                    test,
                    output,
                    opaque,
                );
            } else {
                let explicit_path = explicit.is_some();
                let path = explicit.unwrap_or_else(|| {
                    let name = module.ident.to_string();
                    let flat = base.join(format!("{name}.rs"));
                    if flat.exists() {
                        flat
                    } else {
                        base.join(name).join("mod.rs")
                    }
                });
                // Only real edges are accepted; a wrong directory calculation
                // must fail loudly instead of silently widening an exclusion.
                assert!(path.is_file(), "declared module exists: {}", path.display());
                output.push((path, test, explicit_path));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    let mut sources = BTreeMap::new();
    for file in files
        .iter()
        .map(PathBuf::as_path)
        .chain(parents.iter().copied())
    {
        let source = std::fs::read_to_string(file).expect("module source");
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
        sources.insert(normalize(file), syntax);
    }
    let mut explicit = BTreeSet::new();
    // Only top-level external #[path] attributes have a source-file-relative
    // resolution proven here. Never let an inline/test path guess retag an
    // unrelated production file's module directory.
    fn path_attributes(items: &[syn::Item], parent: &Path, explicit: &mut BTreeSet<PathBuf>) {
        for item in items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            if module.content.is_some() {
                continue;
            }
            for attribute in &module.attrs {
                if attribute.path().is_ident("path") {
                    if let syn::Meta::NameValue(value) = &attribute.meta {
                        if let syn::Expr::Lit(value) = &value.value {
                            if let syn::Lit::Str(value) = &value.lit {
                                explicit.insert(normalize(&parent.join(value.value())));
                            }
                        }
                    }
                }
            }
        }
    }
    for (file, syntax) in &sources {
        path_attributes(&syntax.items, file.parent().expect("parent"), &mut explicit);
    }
    let mut graph = BTreeMap::new();
    let mut opaque_files = BTreeSet::new();
    let mut contexts: BTreeMap<PathBuf, BTreeSet<bool>> = BTreeMap::new();
    for (file, syntax) in &sources {
        let parent = file.parent().expect("parent");
        let stem = file.file_stem().expect("stem");
        let base =
            if explicit.contains(file) || matches!(stem.to_str(), Some("mod" | "lib" | "main")) {
                parent.to_owned()
            } else {
                parent.join(stem)
            };
        let mut declarations = Vec::new();
        let mut opaque = false;
        edges(
            &syntax.items,
            &base,
            parent,
            parent,
            false,
            &mut declarations,
            &mut opaque,
        );
        if opaque {
            opaque_files.insert(file.clone());
        }
        for (path, _, context) in &declarations {
            contexts.entry(path.clone()).or_default().insert(*context);
        }
        graph.insert(file.clone(), declarations);
    }
    let mut reached = BTreeSet::new();
    let mut pending: Vec<_> = parents
        .iter()
        .map(|file| (normalize(file), false))
        .collect();
    pending.extend(
        files
            .iter()
            .filter(|file| {
                matches!(
                    file.file_stem().and_then(|stem| stem.to_str()),
                    Some("lib" | "main")
                )
            })
            .map(|file| (normalize(file), false)),
    );
    fn follow(
        graph: &BTreeMap<PathBuf, Vec<(PathBuf, bool, bool)>>,
        pending: &mut Vec<(PathBuf, bool)>,
        reached: &mut BTreeSet<(PathBuf, bool)>,
    ) {
        while let Some((file, test)) = pending.pop() {
            if !reached.insert((file.clone(), test)) {
                continue;
            }
            if let Some(children) = graph.get(&file) {
                pending.extend(
                    children
                        .iter()
                        .map(|(child, child_test, _)| (child.clone(), test || *child_test)),
                );
            }
        }
    }
    follow(&graph, &mut pending, &mut reached);
    // Unreachable source files remain auditable production inputs. In
    // particular, a newly added or undeclared file cannot silently disappear.
    pending.extend(
        files
            .iter()
            .filter(|file| !reached.iter().any(|(visited, _)| visited == *file))
            .map(|file| (normalize(file), false)),
    );
    follow(&graph, &mut pending, &mut reached);
    for (file, test) in &reached {
        if !*test {
            assert!(
                !opaque_files.contains(file),
                "unsupported production include, local module or conditional module path: {}",
                file.display()
            );
            assert!(
                !contexts
                    .get(file)
                    .is_some_and(|contexts| contexts.len() > 1),
                "mixed default/path production module context: {}",
                file.display()
            );
        }
    }
    files.retain(|file| {
        let file = normalize(file);
        !reached.contains(&(file.clone(), true)) || reached.contains(&(file, false))
    });
    files
}

#[cfg(test)]
mod tests {
    #[test]
    fn ownership_requires_real_test_declarations_and_keeps_new_product_files() {
        let directory = tempfile::tempdir().expect("fixture");
        let root = directory.path();
        std::fs::write(
            root.join("lib.rs"),
            r##"
#[cfg(test)]
#[path = "owned.rs"]
mod actual_tests;
#[cfg(test)]
mod nested;
#[cfg(test)]
mod inline { mod child; }
#[path = "renamed.rs"]
mod product;
#[cfg(test)]
#[path = "shared.rs"]
mod shared_tests;
#[path = "shared.rs"]
mod shared_product;
#[cfg(not(test))]
mod release;
// #[cfg(test)]
// #[path = "comment.rs"]
// mod imaginary;
const QUOTED: &str = r#"
#[cfg(test)]
#[path = "quoted.rs"]
mod imaginary;
"#;
"##,
        )
        .expect("owned source fixture operation");
        std::fs::create_dir(root.join("nested")).expect("owned source fixture operation");
        std::fs::write(root.join("nested.rs"), "mod child;\n")
            .expect("owned source fixture operation");
        std::fs::write(root.join("nested/child.rs"), "fn nested() {}\n")
            .expect("owned source fixture operation");
        std::fs::create_dir(root.join("inline")).expect("owned source fixture operation");
        std::fs::write(root.join("inline/child.rs"), "fn inline() {}\n")
            .expect("owned source fixture operation");
        // A #[path] product module's child resolves beside its source file.
        std::fs::write(root.join("renamed.rs"), "#[cfg(test)]\nmod adjacent;\n")
            .expect("owned source fixture operation");
        std::fs::write(root.join("adjacent.rs"), "fn adjacent() {}\n")
            .expect("owned source fixture operation");
        // The same resolution applies in a #[path] test module; includes
        // inherit that owner's test status but do not hide unrelated siblings.
        std::fs::write(
            root.join("owned.rs"),
            "mod owned_child; include!(\"fragment.rs\");\n",
        )
        .expect("owned source fixture operation");
        for name in [
            "shared.rs",
            "owned_child.rs",
            "fragment.rs",
            "release.rs",
            "comment.rs",
            "quoted.rs",
            "new_product.rs",
            "product_tests.rs",
        ] {
            std::fs::write(root.join(name), "fn example() {}\n")
                .expect("owned source fixture operation");
        }
        let observed = super::production_files(root, &[])
            .into_iter()
            .map(|path| {
                path.strip_prefix(root)
                    .expect("owned source fixture operation")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            observed,
            [
                "lib.rs",
                "shared.rs",
                "renamed.rs",
                "release.rs",
                "comment.rs",
                "quoted.rs",
                "new_product.rs",
                "product_tests.rs"
            ]
            .map(String::from)
            .into()
        );
    }
    #[test]
    fn opaque_or_mixed_production_ownership_refuses_to_hide_test_aliased_files() {
        for source in [
            "mod wrapper { include!(\"shared.rs\"); } #[cfg(test)] #[path=\"shared.rs\"] mod test_alias;",
            "fn f() { #[path=\"shared.rs\"] mod production; } #[cfg(test)] #[path=\"shared.rs\"] mod test_alias;",
            "fn f() { include!(concat!(\"shared\", \".rs\")); } #[cfg(test)] #[path=\"shared.rs\"] mod test_alias;",
            "#[cfg(test)] #[path=\"shared.rs\"] mod test_alias; include!(concat!(\"shared\", \".rs\"));",
            "#[cfg(test)] #[path=\"shared.rs\"] mod test_alias; #[cfg_attr(all(), path=\"shared.rs\")] mod release;",
            "mod shared; #[cfg(test)] #[path=\"shared.rs\"] mod test_alias; #[cfg(test)] #[path=\"shared/child.rs\"] mod child_alias;",
        ] {
            let directory = tempfile::tempdir().expect("opaque ownership fixture");
            let root = directory.path();
            std::fs::write(root.join("lib.rs"),source).expect("owned source fixture operation");
            std::fs::write(root.join("release.rs"),"fn release() {}\n").expect("owned source fixture operation");
            std::fs::write(root.join("shared.rs"),"mod child;\n").expect("owned source fixture operation");
            std::fs::write(root.join("child.rs"),"fn path_child() {}\n").expect("owned source fixture operation");
            std::fs::create_dir(root.join("wrapper")).expect("owned source fixture operation");
            std::fs::write(root.join("wrapper/shared.rs"), "fn wrong_include_target() {}\n").expect("owned source fixture operation");
            std::fs::create_dir(root.join("shared")).expect("owned source fixture operation");
            std::fs::write(root.join("shared/child.rs"),"fn default_child() {}\n").expect("owned source fixture operation");
            assert!(std::panic::catch_unwind(|| super::production_files(root,&[])).is_err(),
                "opaque/mixed production ownership must refuse, never hide shared.rs or shared/child.rs");
        }
        // This exact finite layout was checked with pinned rustc metadata in
        // production and cfg(test) modes: tests/decoy/inner.rs is production.
        // outer.rs's inline TEST path cannot retag tests/decoy.rs's child base.
        let directory = tempfile::tempdir().expect("inline test path context fixture");
        let root = directory.path();
        for path in ["outer/tests", "tests/decoy"] {
            std::fs::create_dir_all(root.join(path)).expect("owned source fixture operation");
        }
        for (path, source) in [
            ("lib.rs", "mod outer; mod tests; #[cfg(test)] #[path=\"tests/decoy/inner.rs\"] mod inner_alias;"),
            ("outer.rs", "#[cfg(test)] mod tests { #[path=\"decoy.rs\"] mod child; }"),
            ("outer/tests/decoy.rs", "pub fn test_only() {}"),
            ("tests.rs", "mod decoy;"),
            ("tests/decoy.rs", "pub mod inner;"),
            ("tests/inner.rs", "pub fn decoy() {}"),
            ("tests/decoy/inner.rs", "pub fn actual() {}"),
        ] { std::fs::write(root.join(path), source).expect("owned source fixture operation"); }
        assert!(std::panic::catch_unwind(|| super::production_files(root,&[])).is_err(),
            "the real production inner file has mixed proven contexts and must refuse, never disappear due to an inline TEST path guess");
    }
}
