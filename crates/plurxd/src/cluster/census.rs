//! A source census of the reach-arounds: production code that touches the
//! cluster's raw parts instead of asking [`crate::cluster`].
//!
//! It parses every production file under `src/` with `syn` (found through
//! `tests/support/rust_source_inventory.rs`, so a new file cannot hide), skips
//! `#[cfg(test)]` items, and counts per file and per [`Class`]:
//!
//! - a field access named `membership`, `replication` or `serving`, on any
//!   receiver — `state.membership` and a subsystem's cloned `self.membership`
//!   alike;
//! - a path naming one of [`TYPES`], or any other path starting `hiqlite::`;
//! - a call to `PeerTransport::new`;
//! - the same three names taken out by a pattern
//!   (`let AppState { membership, .. } = state;`) or moved by a shorthand
//!   field (`Foo { serving }`);
//! - a path naming one of [`TYPES`], or any other path starting `hiqlite::`;
//! - a call to `PeerTransport::new`;
//! - a call, by name, to a membership manager method: its role ([`ROLE`]) and
//!   roster ([`ROSTER`]) answers, and every other public method of
//!   `MembershipManager` (`manager`), read from plurx-core's source less the
//!   names other types share ([`NOT_CLUSTER`]). A handle under another field
//!   name is still counted by what is called on it.
//!
//! Macro bodies are not Rust syntax to `syn`, so their tokens are scanned for
//! the same spellings (`tokio::join!(self.membership.activity_peers(), ..)`
//! counts). An item, statement or block under `#[cfg(test)]` or
//! `#[cfg(all(test, ..))]` is skipped; `any(test, ..)` is counted.
//!
//! What it cannot see: a type renamed on import (`use … as`), a call through
//! a generic or trait-object receiver whose method name is not the
//! manager's, and code outside this crate's `src/` — plurx-core,
//! plurx-cluster-check, `tests/` and `benches/` are not read.
//!
//! [`REACH_AROUNDS`] pins every file's count exactly: a file that gains a site
//! fails, a file that loses one fails until its entry is lowered, and a file
//! with no entry must have none. [`CEILING`] is the table's sum and holds no
//! headroom; `tests/operations/test_cluster_facade_ratchet.py` fails a change
//! that raises it.
//!
//! It is a source census: it counts spellings, not calls or cost.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::visit::Visit;

mod source_inventory {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/rust_source_inventory.rs"
    ));
}

/// What a reach-around touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// `.membership`
    Membership,
    /// `.replication`
    Replication,
    /// `.serving`
    Serving,
    /// A path naming one of [`TYPES`].
    Type,
    /// Any other `hiqlite::` path.
    Hiqlite,
    /// `PeerTransport::new`
    Transport,
    /// A role method of [`ROLE`].
    Role,
    /// A roster method of [`ROSTER`].
    Roster,
    /// Any other public `MembershipManager` method, by name (derived from
    /// plurx-core's source, less [`NOT_CLUSTER`]).
    Manager,
}

impl Class {
    const ALL: [Self; 9] = [
        Self::Membership,
        Self::Replication,
        Self::Serving,
        Self::Type,
        Self::Hiqlite,
        Self::Transport,
        Self::Role,
        Self::Roster,
        Self::Manager,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Membership => "membership",
            Self::Replication => "replication",
            Self::Serving => "serving",
            Self::Type => "type",
            Self::Hiqlite => "hiqlite",
            Self::Transport => "transport",
            Self::Role => "role",
            Self::Roster => "roster",
            Self::Manager => "manager",
        }
    }

    fn field(name: &str) -> Option<Self> {
        match name {
            "membership" => Some(Self::Membership),
            "replication" => Some(Self::Replication),
            "serving" => Some(Self::Serving),
            _ => None,
        }
    }

    fn method(name: &str) -> Option<Self> {
        Self::method_given(name, &manager_pub_fns_cached())
    }

    /// A role or roster name counts while the manager exposes its inherent
    /// method to this crate. A module-private one cannot be called from
    /// here (the compiler rejects it), so a call by that name reaches a trait
    /// impl, and the raw manager behind such a trait is counted where it is
    /// named or held. Visibility is read from plurx-core on every run: making
    /// a raw method `pub` again counts its callers again at once.
    fn method_given(name: &str, public: &[String]) -> Option<Self> {
        let exposed = public.iter().any(|method| method == name);
        if ROLE.contains(&name) {
            exposed.then_some(Self::Role)
        } else if ROSTER.contains(&name) {
            exposed.then_some(Self::Roster)
        } else if manager_methods().iter().any(|method| method == name) {
            Some(Self::Manager)
        } else {
            None
        }
    }
}

/// Raw handles and metrics types of the cluster layer.
const TYPES: &[&str] = &[
    "MembershipManager",
    "ClusterAccess",
    "ReplicationMonitor",
    "ServingFence",
    "PassiveRaftMetrics",
    "PassiveMembershipMetrics",
    "LocalDbRaftMetrics",
];

/// The membership manager's local-role answers (build plan §2).
const ROLE: &[&str] = &[
    "is_replicated",
    "local_serving_role",
    "local_maintenance_active",
    "local_node_is_committed_voter",
    "local_node_is_committed_member",
    "local_node_is_active_voter",
    "may_run_cluster_jobs",
];

/// The membership manager's peer rosters (build plan §2).
const ROSTER: &[&str] = &[
    "activity_peers",
    "activity_voter_count",
    "media_peers",
    "media_peers_for_route",
    "operations_peers",
    "reachable_peer_http_urls",
    "node_hostnames",
    "clock_peers",
    "clock_peers_hint",
    "clock_peers_after_authenticated_request",
    "cache_admin_revocation_peers",
];

/// Public `MembershipManager` methods the census does not count, each with
/// why: the name is every other type's too, so a by-name count would count
/// those. Everything else public on the manager is a cluster question.
const NOT_CLUSTER: &[(&str, &str)] = &[
    ("status", "every HTTP response and process has a `status()`"),
    ("finalize", "every digest and hasher has a `finalize()`"),
    (
        "metrics_handle",
        "every subsystem hands out a `metrics_handle()`",
    ),
    (
        "unavailable",
        "a constructor; the type path is counted as `type`",
    ),
    (
        "replicated",
        "a constructor; the type path is counted as `type`",
    ),
];

/// The public methods of `MembershipManager`, from plurx-core's source:
/// every `pub fn` of an inherent `impl MembershipManager` in
/// `cluster/membership.rs` and its production children, less
/// [`NOT_CLUSTER`].
fn manager_methods() -> &'static [String] {
    static METHODS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    METHODS.get_or_init(|| {
        let mut methods = manager_pub_fns()
            .into_iter()
            .filter(|name| !NOT_CLUSTER.iter().any(|(skip, _)| skip == name))
            .collect::<Vec<_>>();
        methods.sort();
        methods.dedup();
        methods
    })
}

fn manager_pub_fns() -> Vec<String> {
    manager_declared_fns(false)
}

fn manager_pub_fns_cached() -> &'static [String] {
    static PUBLIC: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    PUBLIC.get_or_init(manager_pub_fns)
}

fn manager_declared_fns(include_scoped: bool) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plurx-core/src/cluster");
    let mut files = vec![root.join("membership.rs")];
    for entry in std::fs::read_dir(root.join("membership")).expect("membership children") {
        let path = entry.expect("a membership child").path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        if path.extension().is_some_and(|ext| ext == "rs")
            && stem != "tests"
            && !stem.ends_with("_tests")
        {
            files.push(path);
        }
    }
    let mut names = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).expect("read a membership source");
        let syntax = syn::parse_file(&source).expect("a membership source parses");
        for item in syntax.items {
            let syn::Item::Impl(block) = item else {
                continue;
            };
            let syn::Type::Path(ty) = block.self_ty.as_ref() else {
                continue;
            };
            if block.trait_.is_some()
                || is_cfg_test(&block.attrs)
                || !ty
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "MembershipManager")
            {
                continue;
            }
            for item in block.items {
                if let syn::ImplItem::Fn(function) = item {
                    let visible = matches!(function.vis, syn::Visibility::Public(_))
                        || (include_scoped
                            && matches!(&function.vis,
                            syn::Visibility::Restricted(scope) if scope.path.is_ident("super")));
                    if visible && !is_cfg_test(&function.attrs) {
                        names.push(function.sig.ident.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Files that own the raw parts, relative to `src/`.
const EXEMPT: &[(&str, &str)] = &[
    ("cluster.rs", "the facade itself"),
    (
        "serving_fence.rs",
        "the serving fence is a wrapped mechanism the facade exposes",
    ),
    (
        "http/peer_transport.rs",
        "the signed peer transport `call` (A5/B5) will wrap",
    ),
];

/// Reach-arounds per production file and class, pinned exactly. How the
/// numbers moved, slice by slice, is recorded in
/// `docs/cluster/CLUSTER-FACADE-BUILD.md` §10.
#[rustfmt::skip]
const REACH_AROUNDS: &[(&str, &str, usize)] = &[
    ("clock_offset.rs", "membership", 13),
    ("clock_offset.rs", "type", 2),
    ("desktop_control.rs", "membership", 2),
    ("desktop_control.rs", "replication", 4),
    ("dv_runtime.rs", "membership", 1),
    ("http/cluster_operations.rs", "membership", 42),
    ("http/cluster_operations.rs", "serving", 4),
    ("http/hls/relay.rs", "membership", 1),
    ("http/hls/relay.rs", "manager", 1),
    ("http/live_tv.rs", "membership", 1),
    ("http/live_tv.rs", "manager", 1),
    ("http/stream.rs", "serving", 1),
    ("http/system.rs", "replication", 1),
    ("live_tv.rs", "serving", 13),
    ("live_tv/dvr.rs", "serving", 12),
    ("live_tv/resource.rs", "serving", 3),
    ("main.rs", "membership", 14),
    ("main.rs", "replication", 4),
    ("main.rs", "serving", 3),
    ("main.rs", "type", 5),
    ("main.rs", "manager", 8),
    ("pgs_overlay.rs", "manager", 1),
    ("sharing.rs", "serving", 3),
    ("state.rs", "membership", 2),
    ("state.rs", "replication", 2),
    ("state.rs", "serving", 1),
    ("state.rs", "type", 6),
    ("state.rs", "manager", 1),
    ("subtitles.rs", "manager", 2),
    ("waitpool.rs", "serving", 4),
];

/// The sum of [`REACH_AROUNDS`]; it may only fall.
const CEILING: usize = 158;

fn exempt(rel: &str) -> bool {
    EXEMPT.iter().any(|(path, _)| rel == *path)
}

/// Whether a `cfg` predicate holds only under test: `test`, or an `all(..)`
/// with `test` among its terms. `any(test, ..)` and `not(..)` also compile
/// outside tests, so they are counted.
fn cfg_is_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|terms| terms.iter().any(cfg_is_test)),
        _ => false,
    }
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| cfg_is_test(&meta))
    })
}

/// The outer attributes of an expression, for `#[cfg(test)]` on a statement
/// or block.
fn expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
    match expr {
        syn::Expr::Array(e) => &e.attrs,
        syn::Expr::Assign(e) => &e.attrs,
        syn::Expr::Async(e) => &e.attrs,
        syn::Expr::Await(e) => &e.attrs,
        syn::Expr::Binary(e) => &e.attrs,
        syn::Expr::Block(e) => &e.attrs,
        syn::Expr::Break(e) => &e.attrs,
        syn::Expr::Call(e) => &e.attrs,
        syn::Expr::Cast(e) => &e.attrs,
        syn::Expr::Closure(e) => &e.attrs,
        syn::Expr::Const(e) => &e.attrs,
        syn::Expr::Continue(e) => &e.attrs,
        syn::Expr::Field(e) => &e.attrs,
        syn::Expr::ForLoop(e) => &e.attrs,
        syn::Expr::Group(e) => &e.attrs,
        syn::Expr::If(e) => &e.attrs,
        syn::Expr::Index(e) => &e.attrs,
        syn::Expr::Infer(e) => &e.attrs,
        syn::Expr::Let(e) => &e.attrs,
        syn::Expr::Lit(e) => &e.attrs,
        syn::Expr::Loop(e) => &e.attrs,
        syn::Expr::Macro(e) => &e.attrs,
        syn::Expr::Match(e) => &e.attrs,
        syn::Expr::MethodCall(e) => &e.attrs,
        syn::Expr::Paren(e) => &e.attrs,
        syn::Expr::Path(e) => &e.attrs,
        syn::Expr::Range(e) => &e.attrs,
        syn::Expr::RawAddr(e) => &e.attrs,
        syn::Expr::Reference(e) => &e.attrs,
        syn::Expr::Repeat(e) => &e.attrs,
        syn::Expr::Return(e) => &e.attrs,
        syn::Expr::Struct(e) => &e.attrs,
        syn::Expr::Try(e) => &e.attrs,
        syn::Expr::TryBlock(e) => &e.attrs,
        syn::Expr::Tuple(e) => &e.attrs,
        syn::Expr::Unary(e) => &e.attrs,
        syn::Expr::Unsafe(e) => &e.attrs,
        syn::Expr::While(e) => &e.attrs,
        syn::Expr::Yield(e) => &e.attrs,
        _ => &[],
    }
}

/// Where a source sits, which decides which of its declarations can reach
/// a counted site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// A counted production file of this crate: every declaration.
    Daemon,
    /// An exempt facade file: only fields visible outside it. A private
    /// facade field cannot be named from a counted file; the compiler
    /// rejects it.
    Facade,
    /// Another workspace or vendored crate: only its `pub` fields and
    /// aliases, which are all this crate can reach.
    Dependency,
}

/// What the sources declare, read before any site is counted, so a field
/// spelling counts only where a raw handle can actually sit behind it.
///
/// A name counts when no read source declares it (an unknown receiver), or
/// when any declaration of it has a raw or unresolved type: one of
/// [`TYPES`] or an alias of one, a generic parameter, `impl Trait`, `dyn`
/// or `_`. Only a name every declaration gives a proven type is not
/// counted. One raw field anywhere keeps the name counted everywhere.
#[derive(Default)]
struct Declarations {
    declared_fields: BTreeSet<String>,
    raw_fields: BTreeSet<String>,
    /// Local names that stand for a raw type: `use … as`, `type X = …`.
    raw_aliases: BTreeSet<String>,
    /// `(enum, variant)` of every enum the counted crate declares, so
    /// `Rejection::ServingFence` is not the fence.
    variants: BTreeSet<(String, String)>,
    pending_fields: Vec<(String, syn::Type, BTreeSet<String>)>,
    type_items: Vec<(String, syn::Type)>,
}

impl Declarations {
    fn of(source: &str) -> Self {
        let mut declarations = Self::default();
        declarations.read(source, Scope::Daemon);
        declarations.finish();
        declarations
    }

    fn read(&mut self, source: &str, scope: Scope) {
        let Ok(file) = syn::parse_file(source) else {
            assert!(scope == Scope::Dependency, "a production file parses");
            return;
        };
        let mut reader = DeclarationReader {
            declarations: self,
            scope,
            generics: Vec::new(),
        };
        reader.visit_file(&file);
    }

    /// Resolve aliases of aliases, then classify every pending field.
    fn finish(&mut self) {
        loop {
            let before = self.raw_aliases.len();
            for (name, ty) in &self.type_items {
                if type_names_raw(ty, &BTreeSet::new(), &self.raw_aliases, false) {
                    self.raw_aliases.insert(name.clone());
                }
            }
            if self.raw_aliases.len() == before {
                break;
            }
        }
        for (name, ty, generics) in std::mem::take(&mut self.pending_fields) {
            if type_names_raw(&ty, &generics, &self.raw_aliases, true) {
                self.raw_fields.insert(name);
            }
        }
    }

    fn counts_field(&self, name: &str) -> bool {
        !self.declared_fields.contains(name) || self.raw_fields.contains(name)
    }
}

/// Whether a type names a raw cluster type or an alias of one; with
/// `unresolved`, also whether the census cannot tell what it is.
fn type_names_raw(
    ty: &syn::Type,
    generics: &BTreeSet<String>,
    aliases: &BTreeSet<String>,
    unresolved: bool,
) -> bool {
    struct Probe<'a> {
        generics: &'a BTreeSet<String>,
        aliases: &'a BTreeSet<String>,
        unresolved: bool,
        hit: bool,
    }
    impl<'ast> Visit<'ast> for Probe<'_> {
        fn visit_type_impl_trait(&mut self, ty: &'ast syn::TypeImplTrait) {
            self.hit |= self.unresolved;
            syn::visit::visit_type_impl_trait(self, ty);
        }
        fn visit_type_trait_object(&mut self, ty: &'ast syn::TypeTraitObject) {
            self.hit |= self.unresolved;
            syn::visit::visit_type_trait_object(self, ty);
        }
        fn visit_type_infer(&mut self, ty: &'ast syn::TypeInfer) {
            self.hit |= self.unresolved;
            syn::visit::visit_type_infer(self, ty);
        }
        fn visit_path(&mut self, path: &'ast syn::Path) {
            for segment in &path.segments {
                let name = segment.ident.to_string();
                if TYPES.contains(&name.as_str()) || self.aliases.contains(&name) {
                    self.hit = true;
                }
            }
            if self.unresolved
                && path.segments.len() == 1
                && self.generics.contains(&path.segments[0].ident.to_string())
            {
                self.hit = true;
            }
            syn::visit::visit_path(self, path);
        }
    }
    let mut probe = Probe {
        generics,
        aliases,
        unresolved,
        hit: false,
    };
    probe.visit_type(ty);
    probe.hit
}

struct DeclarationReader<'d> {
    declarations: &'d mut Declarations,
    scope: Scope,
    generics: Vec<BTreeSet<String>>,
}

impl DeclarationReader<'_> {
    fn with_generics(&mut self, generics: &syn::Generics, visit: impl FnOnce(&mut Self)) {
        let mut names = self.generics.last().cloned().unwrap_or_default();
        for param in generics.type_params() {
            names.insert(param.ident.to_string());
        }
        self.generics.push(names);
        visit(self);
        self.generics.pop();
    }

    fn public(vis: &syn::Visibility) -> bool {
        matches!(vis, syn::Visibility::Public(_))
    }
}

impl<'ast> Visit<'ast> for DeclarationReader<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let skip = match item {
            syn::Item::Mod(item) => is_cfg_test(&item.attrs),
            syn::Item::Struct(item) => is_cfg_test(&item.attrs),
            syn::Item::Enum(item) => is_cfg_test(&item.attrs),
            syn::Item::Union(item) => is_cfg_test(&item.attrs),
            syn::Item::Impl(item) => is_cfg_test(&item.attrs),
            syn::Item::Fn(item) => is_cfg_test(&item.attrs),
            syn::Item::Type(item) => is_cfg_test(&item.attrs),
            syn::Item::Use(item) => is_cfg_test(&item.attrs),
            _ => false,
        };
        if !skip {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.with_generics(&item.generics, |reader| {
            syn::visit::visit_item_struct(reader, item);
        });
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.with_generics(&item.generics, |reader| {
            syn::visit::visit_item_union(reader, item);
        });
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if self.scope != Scope::Dependency {
            for variant in &item.variants {
                self.declarations
                    .variants
                    .insert((item.ident.to_string(), variant.ident.to_string()));
            }
        }
        self.with_generics(&item.generics, |reader| {
            syn::visit::visit_item_enum(reader, item);
        });
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if is_cfg_test(&field.attrs) {
            return;
        }
        let Some(name) = field.ident.as_ref().map(ToString::to_string) else {
            return;
        };
        let visible = match self.scope {
            Scope::Daemon => true,
            Scope::Facade => !matches!(field.vis, syn::Visibility::Inherited),
            Scope::Dependency => Self::public(&field.vis),
        };
        if visible && Class::field(&name).is_some() {
            self.declarations.declared_fields.insert(name.clone());
            let generics = self.generics.last().cloned().unwrap_or_default();
            self.declarations
                .pending_fields
                .push((name, field.ty.clone(), generics));
        }
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if self.scope != Scope::Dependency || Self::public(&item.vis) {
            self.declarations
                .type_items
                .push((item.ident.to_string(), (*item.ty).clone()));
        }
    }

    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if TYPES.contains(&rename.ident.to_string().as_str()) {
            self.declarations
                .raw_aliases
                .insert(rename.rename.to_string());
        }
    }
}

struct Counter<'d> {
    counts: BTreeMap<Class, usize>,
    declarations: &'d Declarations,
}

impl Counter<'_> {
    /// A field spelling counts only where a raw handle can sit behind it.
    fn field(&self, name: &str) -> Option<Class> {
        Class::field(name).filter(|_| self.declarations.counts_field(name))
    }

    fn add(&mut self, class: Class) {
        *self.counts.entry(class).or_default() += 1;
    }

    /// The class of a path given as its segment names, if it counts. A raw
    /// type's name, or an alias of one, is the raw type unless it follows
    /// `Self` or an enum this crate declares with a variant of that name.
    fn path_class<'a>(&self, segments: impl Iterator<Item = &'a str>) -> Option<Class> {
        let segments = segments.collect::<Vec<_>>();
        let first = *segments.first()?;
        let mut class = (first == "hiqlite").then_some(Class::Hiqlite);
        for (at, segment) in segments.iter().enumerate() {
            let raw = TYPES.contains(segment) || self.declarations.raw_aliases.contains(*segment);
            let variant = at > 0
                && (segments[at - 1] == "Self"
                    || self
                        .declarations
                        .variants
                        .contains(&(segments[at - 1].to_owned(), (*segment).to_owned())));
            if raw && !variant {
                class = Some(Class::Type);
            }
        }
        class
    }

    fn scan_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.scan_use(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => self.use_leaf(prefix, Some(name.ident.to_string())),
            syn::UseTree::Rename(rename) => {
                self.use_leaf(prefix, Some(rename.ident.to_string()));
            }
            syn::UseTree::Glob(_) => self.use_leaf(prefix, None),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.scan_use(tree, prefix);
                }
            }
        }
    }

    fn use_leaf(&mut self, prefix: &[String], leaf: Option<String>) {
        let segments = prefix.iter().map(String::as_str).chain(leaf.as_deref());
        if let Some(class) = self.path_class(segments) {
            self.add(class);
        }
    }

    /// Macro bodies are tokens, not syntax: scan them for the same spellings.
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let tokens = tokens.into_iter().collect::<Vec<_>>();
        let mut at = 0;
        while at < tokens.len() {
            match &tokens[at] {
                TokenTree::Group(group) => self.scan_tokens(group.stream()),
                TokenTree::Punct(punct) if punct.as_char() == '.' => {
                    // `..serving` is a range or a struct update, not a field.
                    let rest = at > 0
                        && matches!(&tokens[at - 1], TokenTree::Punct(prior) if prior.as_char() == '.');
                    if let (false, Some(TokenTree::Ident(name))) = (rest, tokens.get(at + 1)) {
                        let name = name.to_string();
                        let called = matches!(
                            tokens.get(at + 2),
                            Some(TokenTree::Group(group))
                                if group.delimiter() == Delimiter::Parenthesis
                        ) || matches!(
                            tokens.get(at + 2),
                            Some(TokenTree::Punct(punct)) if punct.as_char() == ':'
                        );
                        if called {
                            if let Some(class) = Class::method(&name) {
                                self.add(class);
                            }
                        } else if let Some(class) = self.field(&name) {
                            self.add(class);
                        }
                        at += 2;
                        continue;
                    }
                }
                TokenTree::Ident(first) => {
                    // A path: `a::b::c`. Collect its segments and classify it
                    // once, as `syn` would for the same path outside a macro.
                    let mut segments = vec![first.to_string()];
                    let mut next = at + 1;
                    while let (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b))) =
                        (tokens.get(next), tokens.get(next + 1))
                    {
                        if a.as_char() != ':' || b.as_char() != ':' {
                            break;
                        }
                        let Some(TokenTree::Ident(segment)) = tokens.get(next + 2) else {
                            break;
                        };
                        segments.push(segment.to_string());
                        next += 3;
                    }
                    if segments.len() >= 2
                        && segments[segments.len() - 2] == "PeerTransport"
                        && segments[segments.len() - 1] == "new"
                    {
                        self.add(Class::Transport);
                    }
                    if let Some(class) = self.path_class(segments.iter().map(String::as_str)) {
                        self.add(class);
                    }
                    at = next;
                    continue;
                }
                _ => {}
            }
            at += 1;
        }
    }
}

impl<'ast> Visit<'ast> for Counter<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs = match item {
            syn::Item::Const(item) => &item.attrs,
            syn::Item::Enum(item) => &item.attrs,
            syn::Item::ExternCrate(item) => &item.attrs,
            syn::Item::Fn(item) => &item.attrs,
            syn::Item::ForeignMod(item) => &item.attrs,
            syn::Item::Impl(item) => &item.attrs,
            syn::Item::Macro(item) => &item.attrs,
            syn::Item::Mod(item) => &item.attrs,
            syn::Item::Static(item) => &item.attrs,
            syn::Item::Struct(item) => &item.attrs,
            syn::Item::Trait(item) => &item.attrs,
            syn::Item::TraitAlias(item) => &item.attrs,
            syn::Item::Type(item) => &item.attrs,
            syn::Item::Union(item) => &item.attrs,
            syn::Item::Use(item) => &item.attrs,
            _ => return syn::visit::visit_item(self, item),
        };
        if !is_cfg_test(attrs) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs = match item {
            syn::ImplItem::Const(item) => &item.attrs,
            syn::ImplItem::Fn(item) => &item.attrs,
            syn::ImplItem::Type(item) => &item.attrs,
            syn::ImplItem::Macro(item) => &item.attrs,
            _ => return syn::visit::visit_impl_item(self, item),
        };
        if !is_cfg_test(attrs) {
            syn::visit::visit_impl_item(self, item);
        }
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs = match item {
            syn::TraitItem::Const(item) => &item.attrs,
            syn::TraitItem::Fn(item) => &item.attrs,
            syn::TraitItem::Type(item) => &item.attrs,
            syn::TraitItem::Macro(item) => &item.attrs,
            _ => return syn::visit::visit_trait_item(self, item),
        };
        if !is_cfg_test(attrs) {
            syn::visit::visit_trait_item(self, item);
        }
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if !is_cfg_test(&field.attrs) {
            syn::visit::visit_field(self, field);
        }
    }

    fn visit_field_value(&mut self, field: &'ast syn::FieldValue) {
        if is_cfg_test(&field.attrs) {
            return;
        }
        // `Foo { membership }`: the shorthand moves the handle by name.
        if let (None, syn::Member::Named(name)) = (&field.colon_token, &field.member) {
            if let Some(class) = self.field(&name.to_string()) {
                self.add(class);
            }
        }
        syn::visit::visit_field_value(self, field);
    }

    /// `let AppState { membership, .. } = state;` takes the handle out by
    /// name, and whatever is then called on it would count nothing.
    fn visit_field_pat(&mut self, field: &'ast syn::FieldPat) {
        if is_cfg_test(&field.attrs) {
            return;
        }
        if let syn::Member::Named(name) = &field.member {
            if let Some(class) = self.field(&name.to_string()) {
                self.add(class);
            }
        }
        syn::visit::visit_field_pat(self, field);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        let skip = match stmt {
            syn::Stmt::Expr(expr, _) => is_cfg_test(expr_attrs(expr)),
            syn::Stmt::Macro(mac) => is_cfg_test(&mac.attrs),
            _ => false,
        };
        if !skip {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if !is_cfg_test(expr_attrs(expr)) {
            syn::visit::visit_expr(self, expr);
        }
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if !is_cfg_test(&local.attrs) {
            syn::visit::visit_local(self, local);
        }
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if !is_cfg_test(&arm.attrs) {
            syn::visit::visit_arm(self, arm);
        }
    }

    fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
        if let syn::Member::Named(name) = &field.member {
            if let Some(class) = self.field(&name.to_string()) {
                self.add(class);
            }
        }
        syn::visit::visit_expr_field(self, field);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if let Some(class) = Class::method(&call.method.to_string()) {
            self.add(class);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            let segments = &path.path.segments;
            if segments.len() >= 2
                && segments[segments.len() - 2].ident == "PeerTransport"
                && segments[segments.len() - 1].ident == "new"
            {
                self.add(Class::Transport);
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let names = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if let Some(class) = self.path_class(names.iter().map(String::as_str)) {
            self.add(class);
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.scan_use(&item.tree, &mut Vec::new());
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.visit_path(&mac.path);
        self.scan_tokens(mac.tokens.clone());
    }
}

/// Count one self-contained source: its own declarations decide its
/// field spellings.
fn census_of(source: &str) -> BTreeMap<Class, usize> {
    census_with(source, &Declarations::of(source))
}

fn census_with(source: &str, declarations: &Declarations) -> BTreeMap<Class, usize> {
    let file = syn::parse_file(source).expect("a production file parses");
    let mut counter = Counter {
        counts: BTreeMap::new(),
        declarations,
    };
    counter.visit_file(&file);
    counter.counts
}

/// Production sources of every other workspace and vendored crate, whose
/// `pub` fields this crate can reach.
fn dependency_sources() -> Vec<String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("the crate directory");
    let mut roots = Vec::new();
    for parent in [manifest.join(".."), manifest.join("../../vendor")] {
        for entry in std::fs::read_dir(&parent).expect("a workspace crate directory") {
            let path = entry.expect("a workspace crate").path().join("src");
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            if path.is_dir() && !path.starts_with(&manifest) {
                roots.push(path);
            }
        }
    }
    let mut sources = Vec::new();
    while let Some(directory) = roots.pop() {
        for entry in std::fs::read_dir(&directory).expect("a source directory") {
            let path = entry.expect("a source entry").path();
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("");
            if path.is_dir() {
                if stem != "tests" {
                    roots.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && stem != "tests"
                && !stem.ends_with("_tests")
            {
                sources.push(std::fs::read_to_string(&path).expect("read a dependency source"));
            }
        }
    }
    sources
}

type Census = BTreeMap<(String, Class), usize>;

fn src_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every production file's reach-arounds.
fn census() -> Census {
    let root = src_root();
    let files = source_inventory::production_files(&root, &[])
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(&root)
                .expect("a production file under src")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path).expect("read a production file");
            (rel, source)
        })
        .collect::<Vec<_>>();
    let mut declarations = Declarations::default();
    for (rel, source) in &files {
        let scope = if exempt(rel) {
            Scope::Facade
        } else {
            Scope::Daemon
        };
        declarations.read(source, scope);
    }
    for source in dependency_sources() {
        declarations.read(&source, Scope::Dependency);
    }
    declarations.finish();
    let mut census = Census::new();
    for (rel, source) in &files {
        if exempt(rel) {
            continue;
        }
        for (class, count) in census_with(source, &declarations) {
            census.insert((rel.clone(), class), count);
        }
    }
    census
}

fn pinned() -> Census {
    REACH_AROUNDS
        .iter()
        .map(|(file, label, count)| {
            let class = Class::ALL
                .into_iter()
                .find(|class| class.label() == *label)
                .unwrap_or_else(|| panic!("{file}: unknown class {label}"));
            (((*file).to_owned(), class), *count)
        })
        .collect()
}

/// Every (file, class) whose count differs from its pin, by name.
fn differences(observed: &Census, pinned: &Census) -> Vec<String> {
    let mut keys = observed.keys().chain(pinned.keys()).collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter_map(|key| {
            let found = observed.get(key).copied().unwrap_or(0);
            let pin = pinned.get(key).copied().unwrap_or(0);
            (found != pin)
                .then(|| format!("{} [{}]: found {found}, pinned {pin}", key.0, key.1.label()))
        })
        .collect()
}

/// The table as it would have to read, for a change that moves a count.
fn table(census: &Census) -> String {
    census
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|((file, class), count)| {
            format!("    (\"{file}\", \"{}\", {count}),\n", class.label())
        })
        .collect()
}

#[test]
fn reach_arounds_match_the_pinned_census() {
    let observed = census();
    let differences = differences(&observed, &pinned());
    assert!(
        differences.is_empty(),
        "cluster reach-arounds moved. Ask `state.cluster` instead of the raw \
         part; a file that lost a site lowers its pin and CEILING in the same \
         change.\n{}\nThe table now reads:\n{}total {}",
        differences.join("\n"),
        table(&observed),
        observed.values().sum::<usize>(),
    );
}

#[test]
fn the_ceiling_is_the_table_sum_with_no_headroom() {
    let sum = REACH_AROUNDS
        .iter()
        .map(|(_, _, count)| count)
        .sum::<usize>();
    assert_eq!(CEILING, sum, "CEILING must equal the sum of REACH_AROUNDS");
    for (file, _, count) in REACH_AROUNDS {
        assert!(*count > 0, "{file}: a zero pin is an entry to delete");
    }
}

#[test]
fn every_exemption_names_a_real_path_and_a_reason() {
    let root = src_root();
    for (path, reason) in EXEMPT {
        assert!(!reason.is_empty(), "{path}: an exemption states its reason");
        assert!(
            root.join(path).is_file(),
            "{path}: an exemption names a path that exists"
        );
    }
    for (file, _, _) in REACH_AROUNDS {
        assert!(!exempt(file), "{file}: an exempt file has no pin");
    }
}

/// Synthetic controls: each spelling a new reach-around could take is
/// counted, and added to a real file it fails the census naming that file
/// and class.
#[test]
fn a_new_reach_around_fails_naming_its_file_and_class() {
    let cases: &[(&str, &str, &[Class])] = &[
        (
            "direct",
            "async fn f(state: &AppState) { let _ = state.membership.promote_learner(id).await; }",
            &[Class::Membership, Class::Manager],
        ),
        (
            "split across lines",
            "async fn f(state: &AppState) {\n    let _ = state\n        .membership\n        \
             .promote_learner(id)\n        .await;\n}",
            &[Class::Membership, Class::Manager],
        ),
        (
            "a cloned field",
            "impl Pool { async fn f(&self) { let _ = self.membership.promote_learner(id).await; } }",
            &[Class::Membership, Class::Manager],
        ),
        (
            "a transport",
            "fn f(m: Manager) -> crate::http::peer_transport::PeerTransport { PeerTransport::new(m) }",
            &[Class::Transport],
        ),
        (
            "inside a macro",
            "async fn f(&self) { let _ = tokio::join!(self.membership.promote_learner(id), \
             self.serving.is_ready()); }",
            &[Class::Membership, Class::Manager, Class::Serving],
        ),
        (
            "a raw type",
            "fn f(m: plurx_core::cluster::membership::MembershipManager) {}",
            &[Class::Type],
        ),
        ("a hiqlite path", "fn f() -> hiqlite::Error { todo!() }", &[Class::Hiqlite]),
        (
            "a destructured handle",
            "async fn f(state: &AppState) { let AppState { membership, .. } = state; \
             let _ = membership.issue_token(d).await; }",
            &[Class::Membership, Class::Manager],
        ),
        (
            "a shorthand field",
            "fn f(serving: Fence) -> S { S { serving } }",
            &[Class::Serving],
        ),
        (
            "a manager method on a renamed field",
            "async fn f(&self) { let _ = self.cluster_handle.promote_learner(id).await; }",
            &[Class::Manager],
        ),
        (
            "a test-and-something cfg is still production when any",
            "#[cfg(any(test, unix))] fn f(s: &S) { let _ = s.replication; }",
            &[Class::Replication],
        ),
    ];
    let real = pinned();
    for (name, source, classes) in cases {
        let counted = census_of(source);
        for class in *classes {
            assert!(
                counted.get(class).copied().unwrap_or(0) > 0,
                "{name}: {} is not counted in {source}",
                class.label()
            );
        }
        let mut observed = real.clone();
        for (class, count) in counted {
            *observed
                .entry(("watched.rs".to_owned(), class))
                .or_default() += count;
        }
        let differences = differences(&observed, &real);
        for class in *classes {
            assert!(
                differences
                    .iter()
                    .any(|line| line.starts_with(&format!("watched.rs [{}]", class.label()))),
                "{name}: the census does not fail naming watched.rs [{}]: {differences:?}",
                class.label()
            );
        }
    }
}

#[test]
fn test_only_items_are_not_counted() {
    let source =
        "#[cfg(test)] mod tests { fn f(s: &S) { let _ = s.membership.activity_peers(); } }\n\
                  impl S { #[cfg(test)] fn g(&self) { let _ = self.serving.is_ready(); } }\n\
                  fn h(s: &S) { let _ = s.replication; }";
    let counted = census_of(source);
    assert_eq!(counted.get(&Class::Replication), Some(&1));
    assert_eq!(counted.get(&Class::Membership), None);
    assert_eq!(counted.get(&Class::Serving), None);
    assert_eq!(counted.get(&Class::Roster), None);
}

#[test]
fn test_only_conjunctions_statements_and_blocks_are_not_counted() {
    let source = "#[cfg(all(test, unix))] fn f(s: &S) { let _ = s.membership; }\n\
                  fn g(s: &S) {\n\
                      #[cfg(test)]\n\
                      s.serving.reset();\n\
                      #[cfg(test)]\n\
                      { let _ = s.membership.media_peers(); }\n\
                      #[cfg(test)]\n\
                      tracing::debug!(\"{:?}\", s.replication);\n\
                  }";
    assert_eq!(census_of(source), BTreeMap::new());
}

#[test]
fn a_struct_update_in_a_macro_is_not_a_field() {
    let counted = census_of("fn f() -> Vec<S> { vec![S { a: 1, ..serving }] }");
    assert_eq!(counted.get(&Class::Serving), None);
    let counted = census_of("fn f() -> Vec<S> { vec![s.serving] }");
    assert_eq!(counted.get(&Class::Serving), Some(&1));
}

/// Classification by declaration: what the census must still count, and the
/// one shape it no longer counts, a field every declaration proves local.
#[test]
fn spellings_count_by_what_the_crate_declares() {
    let counted: &[(&str, &str, Class)] = &[
        (
            "a raw handle under another field name",
            "struct S { handle: MembershipManager }\n\
             async fn f(s: &S) { let _ = s.handle.promote_learner(1).await; }",
            Class::Type,
        ),
        (
            "a use-as alias of a raw type",
            "use plurx_core::cluster::membership::MembershipManager as Mm;\n\
             fn f(m: Mm) -> Mm { m }",
            Class::Type,
        ),
        (
            "a type alias of a raw type",
            "type Mm = plurx_core::cluster::membership::MembershipManager;\n\
             fn f(m: Mm) {}",
            Class::Type,
        ),
        (
            "an accessor returning a raw type",
            "impl S { fn raw(&self) -> &ServingFence { &self.x } }",
            Class::Type,
        ),
        (
            "a generic-typed field",
            "struct S<T> { membership: T }\n\
             fn f<T>(s: &S<T>) { let _ = &s.membership; }",
            Class::Membership,
        ),
        (
            "a trait-object field",
            "struct S { serving: Box<dyn Gate> }\n\
             fn f(s: &S) { let _ = &s.serving; }",
            Class::Serving,
        ),
        (
            "a raw clone through a local binding",
            "struct S { membership: MembershipManager }\n\
             fn f(s: &S) { let m = s.membership.clone(); drop(m); }",
            Class::Membership,
        ),
        (
            "a proven field sharing its name with a raw one",
            "struct Row { membership: Status }\n\
             struct State { membership: MembershipManager }\n\
             fn f(r: &Row) -> &str { &r.membership.node_id }",
            Class::Membership,
        ),
        (
            "an undeclared receiver",
            "fn f(s: &Other) -> bool { s.serving.is_ready() }",
            Class::Serving,
        ),
        (
            "the raw type beside a same-named variant",
            "enum E { ServingFence }\n\
             fn f(fence: ServingFence) -> E { E::ServingFence }",
            Class::Type,
        ),
    ];
    for (name, source, class) in counted {
        let found = census_of(source).get(class).copied().unwrap_or(0);
        assert!(
            found > 0,
            "{name}: {} is not counted in {source}",
            class.label()
        );
    }
    let alias = census_of(
        "use plurx_core::cluster::membership::MembershipManager as Mm;\n\
         fn f(m: Mm) -> Mm { m }",
    );
    assert_eq!(
        alias.get(&Class::Type),
        Some(&3),
        "the import and both uses"
    );

    let local = census_of(
        "struct Row { membership: Status, serving: Readiness }\n\
         enum Rejection { ServingFence }\n\
         impl Rejection { fn code(&self) -> u16 { match self { Self::ServingFence => 503 } } }\n\
         fn f(r: &Row) -> bool { r.serving.ready && !r.membership.node_id.is_empty() }\n\
         fn g() -> Rejection { Rejection::ServingFence }",
    );
    assert_eq!(
        local,
        BTreeMap::new(),
        "proven local fields and variants are not raw"
    );
}

/// A file the census has never seen fails naming that file.
#[test]
fn a_new_file_fails_naming_itself() {
    let real = pinned();
    let mut observed = real.clone();
    observed.insert(("brand_new.rs".to_owned(), Class::Membership), 1);
    assert!(differences(&observed, &real)
        .iter()
        .any(|line| line.starts_with("brand_new.rs [membership]")));
}

/// Role and roster detection follows the manager's real visibility: a name
/// the manager exposes counts, and the same name counts no longer once
/// plurx-core makes its inherent method module-private.
#[test]
fn role_and_roster_names_count_while_the_manager_exposes_them() {
    let exposed = vec![
        "may_run_cluster_jobs".to_owned(),
        "operations_peers".to_owned(),
    ];
    assert_eq!(
        Class::method_given("may_run_cluster_jobs", &exposed),
        Some(Class::Role)
    );
    assert_eq!(
        Class::method_given("operations_peers", &exposed),
        Some(Class::Roster)
    );
    assert_eq!(Class::method_given("may_run_cluster_jobs", &[]), None);
    assert_eq!(Class::method_given("operations_peers", &[]), None);
    let scoped = manager_declared_fns(true);
    for name in ROLE.iter().chain(ROSTER) {
        let public = manager_pub_fns_cached().iter().any(|method| method == name);
        let declared = scoped.iter().any(|method| method == name);
        assert!(
            public || !declared || Class::method(name).is_none(),
            "{name}: a module-private raw method is not counted by name"
        );
    }
}

/// The manager's public surface is read from plurx-core, so a new method is
/// counted the day it lands; the hand lists name only real methods.
#[test]
fn every_named_method_is_a_real_manager_method() {
    let all = manager_declared_fns(true);
    let public = manager_pub_fns();
    for name in ROLE.iter().chain(ROSTER) {
        assert!(
            !public.iter().any(|method| method == name),
            "raw method {name} must remain module-private"
        );
    }
    assert!(
        all.len() > 50,
        "the manager's methods were not found: {all:?}"
    );
    let retired = [
        "activity_peers",
        "activity_voter_count",
        "media_peers",
        "media_peers_for_route",
    ];
    for name in &retired {
        assert!(ROSTER.contains(name), "retired raw method remains reserved");
        assert!(
            !all.iter().any(|method| method == name),
            "retired raw method {name} must not return"
        );
    }
    for name in ROLE
        .iter()
        .chain(ROSTER)
        .filter(|name| !retired.contains(name))
    {
        assert!(
            all.iter().any(|method| method == name),
            "{name} is not a restricted manager method"
        );
    }
    for (name, reason) in NOT_CLUSTER {
        assert!(!reason.is_empty(), "{name}: a skip states its reason");
        assert!(
            all.iter().any(|method| method == name),
            "{name} is not a manager method"
        );
    }
    assert!(manager_methods()
        .iter()
        .any(|method| method == "promote_learner"));
    assert!(!manager_methods().iter().any(|method| method == "status"));
}
