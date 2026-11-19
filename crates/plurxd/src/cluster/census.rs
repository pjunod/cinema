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

use std::collections::BTreeMap;
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
        if ROLE.contains(&name) {
            Some(Self::Role)
        } else if ROSTER.contains(&name) {
            Some(Self::Roster)
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
                    if matches!(function.vis, syn::Visibility::Public(_))
                        && !is_cfg_test(&function.attrs)
                    {
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
    ("background_jobs.rs", "role", 6),
    ("background_jobs/analysis_reconcile.rs", "membership", 3),
    ("background_jobs/analysis_reconcile.rs", "type", 2),
    ("background_jobs/analysis_reconcile.rs", "role", 2),
    ("background_jobs/analysis_reconcile.rs", "roster", 1),
    ("backup.rs", "hiqlite", 4),
    ("cache_preparation.rs", "membership", 2),
    ("cache_preparation.rs", "roster", 2),
    ("clock_offset.rs", "membership", 27),
    ("clock_offset.rs", "type", 7),
    ("clock_offset.rs", "transport", 1),
    ("clock_offset.rs", "role", 2),
    ("clock_offset.rs", "roster", 3),
    ("clock_offset.rs", "manager", 14),
    ("copy_index_v3.rs", "membership", 2),
    ("copy_index_v3.rs", "type", 4),
    ("copy_index_v3.rs", "transport", 1),
    ("copy_index_v3.rs", "roster", 1),
    ("desktop_control.rs", "membership", 4),
    ("desktop_control.rs", "replication", 4),
    ("desktop_control.rs", "serving", 12),
    ("desktop_control.rs", "manager", 1),
    ("diagnostics.rs", "membership", 3),
    ("diagnostics.rs", "serving", 1),
    ("diagnostics.rs", "transport", 1),
    ("diagnostics.rs", "roster", 1),
    ("diagnostics.rs", "manager", 1),
    ("dv_runtime.rs", "membership", 1),
    ("fragment_index_cluster.rs", "type", 4),
    ("fragment_index_cluster.rs", "transport", 1),
    ("fragment_index_cluster.rs", "roster", 1),
    ("http/activity_starts.rs", "membership", 4),
    ("http/activity_starts.rs", "transport", 1),
    ("http/activity_starts.rs", "role", 1),
    ("http/activity_starts.rs", "roster", 1),
    ("http/activity_starts.rs", "manager", 1),
    ("http/background_jobs.rs", "role", 1),
    ("http/cluster.rs", "membership", 22),
    ("http/cluster.rs", "serving", 7),
    ("http/cluster.rs", "role", 2),
    ("http/cluster.rs", "roster", 1),
    ("http/cluster.rs", "manager", 14),
    ("http/cluster_operations.rs", "membership", 58),
    ("http/cluster_operations.rs", "replication", 5),
    ("http/cluster_operations.rs", "serving", 24),
    ("http/cluster_operations.rs", "type", 11),
    ("http/cluster_operations.rs", "transport", 2),
    ("http/cluster_operations.rs", "role", 2),
    ("http/cluster_operations.rs", "roster", 2),
    ("http/cluster_operations.rs", "manager", 5),
    ("http/developer.rs", "membership", 12),
    ("http/developer.rs", "role", 3),
    ("http/developer.rs", "roster", 2),
    ("http/developer.rs", "manager", 4),
    ("http/dvr.rs", "membership", 1),
    ("http/dvr.rs", "role", 1),
    ("http/hls/control.rs", "serving", 6),
    ("http/hls/create.rs", "serving", 3),
    ("http/hls/fence.rs", "serving", 1),
    ("http/hls/fence.rs", "type", 1),
    ("http/hls/preparation.rs", "serving", 5),
    ("http/hls/relay.rs", "membership", 1),
    ("http/hls/relay.rs", "manager", 1),
    ("http/hls/response.rs", "serving", 1),
    ("http/images.rs", "membership", 14),
    ("http/images.rs", "type", 3),
    ("http/images.rs", "role", 5),
    ("http/images.rs", "roster", 5),
    ("http/images.rs", "manager", 6),
    ("http/images_worker.rs", "membership", 2),
    ("http/images_worker.rs", "role", 1),
    ("http/images_worker.rs", "roster", 1),
    ("http/images_worker.rs", "manager", 1),
    ("http/internal_activity.rs", "type", 1),
    ("http/internal_activity.rs", "transport", 1),
    ("http/internal_auth_revocation.rs", "membership", 16),
    ("http/internal_auth_revocation.rs", "type", 4),
    ("http/internal_auth_revocation.rs", "transport", 1),
    ("http/internal_auth_revocation.rs", "role", 3),
    ("http/internal_auth_revocation.rs", "roster", 1),
    ("http/internal_auth_revocation.rs", "manager", 9),
    ("http/internal_clock.rs", "membership", 2),
    ("http/internal_clock.rs", "type", 2),
    ("http/internal_clock.rs", "manager", 4),
    ("http/internal_live_tv.rs", "membership", 3),
    ("http/internal_live_tv.rs", "serving", 7),
    ("http/internal_live_tv.rs", "manager", 3),
    ("http/internal_media.rs", "membership", 2),
    ("http/internal_media.rs", "manager", 2),
    ("http/internal_media_sessions.rs", "membership", 2),
    ("http/internal_media_sessions.rs", "serving", 6),
    ("http/internal_media_sessions.rs", "type", 4),
    ("http/internal_media_sessions.rs", "manager", 2),
    ("http/live_tv.rs", "membership", 1),
    ("http/live_tv.rs", "serving", 7),
    ("http/live_tv.rs", "type", 1),
    ("http/live_tv.rs", "transport", 1),
    ("http/live_tv.rs", "manager", 1),
    ("http/mod.rs", "membership", 5),
    ("http/mod.rs", "replication", 1),
    ("http/mod.rs", "serving", 6),
    ("http/mod.rs", "type", 1),
    ("http/mod.rs", "role", 4),
    ("http/mod.rs", "manager", 1),
    ("http/remote.rs", "membership", 7),
    ("http/remote.rs", "transport", 2),
    ("http/remote.rs", "role", 1),
    ("http/remote.rs", "roster", 2),
    ("http/remote.rs", "manager", 2),
    ("http/remote/invitations/snapshots.rs", "membership", 2),
    ("http/remote/invitations/snapshots.rs", "transport", 1),
    ("http/remote/invitations/snapshots.rs", "roster", 1),
    ("http/shared_library.rs", "membership", 4),
    ("http/shared_library.rs", "serving", 3),
    ("http/shared_library.rs", "role", 2),
    ("http/shared_library.rs", "manager", 2),
    ("http/shared_receiver_forwarding.rs", "membership", 2),
    ("http/shared_receiver_forwarding.rs", "manager", 2),
    ("http/shared_receiver_ingress_custody.rs", "membership", 2),
    ("http/shared_receiver_ingress_custody.rs", "role", 1),
    ("http/shared_receiver_ingress_custody.rs", "manager", 1),
    ("http/shared_receiver_playback.rs", "serving", 1),
    ("http/shared_source_forwarding.rs", "membership", 3),
    ("http/shared_source_forwarding.rs", "roster", 1),
    ("http/shared_source_forwarding.rs", "manager", 2),
    ("http/shared_source_ingress.rs", "membership", 4),
    ("http/shared_source_ingress.rs", "serving", 1),
    ("http/shared_source_ingress.rs", "manager", 4),
    ("http/shared_source_playback.rs", "membership", 3),
    ("http/shared_source_playback.rs", "manager", 3),
    ("http/stream.rs", "serving", 2),
    ("http/system.rs", "membership", 8),
    ("http/system.rs", "replication", 3),
    ("http/system.rs", "type", 2),
    ("http/system.rs", "hiqlite", 2),
    ("http/system.rs", "role", 3),
    ("http/system.rs", "roster", 1),
    ("http/system.rs", "manager", 3),
    ("http/transcode_copies.rs", "membership", 2),
    ("http/transcode_copies.rs", "transport", 1),
    ("http/transcode_copies.rs", "role", 1),
    ("http/transcode_copies.rs", "roster", 1),
    ("job_lease.rs", "role", 1),
    ("library_search.rs", "role", 3),
    ("live_tv.rs", "serving", 13),
    ("live_tv/dvr.rs", "serving", 12),
    ("live_tv/resource.rs", "serving", 3),
    ("main.rs", "membership", 19),
    ("main.rs", "replication", 4),
    ("main.rs", "serving", 7),
    ("main.rs", "type", 5),
    ("main.rs", "hiqlite", 2),
    ("main.rs", "manager", 8),
    ("media_pool.rs", "serving", 3),
    ("media_pool.rs", "type", 2),
    ("media_pool.rs", "transport", 1),
    ("media_sessions.rs", "membership", 2),
    ("media_sessions.rs", "type", 2),
    ("media_sessions.rs", "transport", 1),
    ("media_sessions.rs", "manager", 2),
    ("offline.rs", "serving", 2),
    ("offline.rs", "type", 2),
    ("offline.rs", "role", 1),
    ("pgs_overlay.rs", "manager", 1),
    ("settings_changes.rs", "type", 1),
    ("shared_cache.rs", "type", 2),
    ("shared_cache.rs", "transport", 1),
    ("sharing.rs", "serving", 4),
    ("sharing_connection_custody.rs", "membership", 2),
    ("sharing_connection_custody.rs", "manager", 2),
    ("state.rs", "membership", 6),
    ("state.rs", "replication", 2),
    ("state.rs", "serving", 1),
    ("state.rs", "type", 8),
    ("state.rs", "hiqlite", 1),
    ("state.rs", "role", 10),
    ("state.rs", "manager", 2),
    ("subtitle_models/mod.rs", "membership", 2),
    ("subtitle_models/mod.rs", "serving", 1),
    ("subtitle_models/mod.rs", "role", 1),
    ("subtitle_models/mod.rs", "roster", 1),
    ("subtitle_ranges.rs", "transport", 1),
    ("subtitle_ranges.rs", "roster", 1),
    ("subtitle_source.rs", "membership", 3),
    ("subtitle_source.rs", "type", 4),
    ("subtitle_source.rs", "transport", 1),
    ("subtitle_source.rs", "roster", 1),
    ("subtitle_work.rs", "role", 1),
    ("subtitles.rs", "manager", 2),
    ("trakt.rs", "role", 1),
    ("transcode.rs", "type", 1),
    ("transcode/manager/construct.rs", "type", 1),
    ("transcode/manager/control.rs", "type", 5),
    ("transcode/source_actor.rs", "membership", 6),
    ("transcode/source_actor.rs", "type", 1),
    ("transcode/source_actor.rs", "manager", 5),
    ("transcode/source_actor/growing.rs", "membership", 2),
    ("transcode/source_actor/growing.rs", "manager", 2),
    ("transcode/source_direct.rs", "membership", 3),
    ("transcode/source_direct.rs", "manager", 2),
    ("vod/serve/construct.rs", "type", 3),
    ("vodserve.rs", "type", 1),
    ("waitpool.rs", "serving", 4),
    ("watched.rs", "role", 1),
];

/// The sum of [`REACH_AROUNDS`]; it may only fall.
const CEILING: usize = 750;

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

#[derive(Default)]
struct Counter {
    counts: BTreeMap<Class, usize>,
}

impl Counter {
    fn add(&mut self, class: Class) {
        *self.counts.entry(class).or_default() += 1;
    }

    /// The class of a path given as its segment names, if it counts.
    fn path_class<'a>(mut segments: impl Iterator<Item = &'a str>) -> Option<Class> {
        let first = segments.next()?;
        if TYPES.contains(&first) {
            return Some(Class::Type);
        }
        let mut class = (first == "hiqlite").then_some(Class::Hiqlite);
        for segment in segments {
            if TYPES.contains(&segment) {
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
        if let Some(class) = Self::path_class(segments) {
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
                        } else if let Some(class) = Class::field(&name) {
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
                    if let Some(class) = Self::path_class(segments.iter().map(String::as_str)) {
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

impl<'ast> Visit<'ast> for Counter {
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
            if let Some(class) = Class::field(&name.to_string()) {
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
            if let Some(class) = Class::field(&name.to_string()) {
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
            if let Some(class) = Class::field(&name.to_string()) {
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
        if let Some(class) = Self::path_class(names.iter().map(String::as_str)) {
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

/// Count one file's source.
fn census_of(source: &str) -> BTreeMap<Class, usize> {
    let file = syn::parse_file(source).expect("a production file parses");
    let mut counter = Counter::default();
    counter.visit_file(&file);
    counter.counts
}

type Census = BTreeMap<(String, Class), usize>;

fn src_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every production file's reach-arounds.
fn census() -> Census {
    let root = src_root();
    let mut census = Census::new();
    for path in source_inventory::production_files(&root, &[]) {
        let rel = path
            .strip_prefix(&root)
            .expect("a production file under src")
            .to_string_lossy()
            .replace('\\', "/");
        if exempt(&rel) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read a production file");
        for (class, count) in census_of(&source) {
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
            "async fn f(state: &AppState) { let _ = state.membership.activity_peers().await; }",
            &[Class::Membership, Class::Roster],
        ),
        (
            "split across lines",
            "async fn f(state: &AppState) {\n    let _ = state\n        .membership\n        \
             .activity_peers()\n        .await;\n}",
            &[Class::Membership, Class::Roster],
        ),
        (
            "a cloned field",
            "impl Pool { async fn f(&self) { let _ = self.membership.media_peers().await; } }",
            &[Class::Membership, Class::Roster],
        ),
        (
            "a transport",
            "fn f(m: Manager) -> crate::http::peer_transport::PeerTransport { PeerTransport::new(m) }",
            &[Class::Transport],
        ),
        (
            "inside a macro",
            "async fn f(&self) { let _ = tokio::join!(self.membership.activity_peers(), \
             self.serving.is_ready()); }",
            &[Class::Membership, Class::Roster, Class::Serving],
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

/// The manager's public surface is read from plurx-core, so a new method is
/// counted the day it lands; the hand lists name only real methods.
#[test]
fn every_named_method_is_a_real_manager_method() {
    let all = manager_pub_fns();
    assert!(
        all.len() > 50,
        "the manager's methods were not found: {all:?}"
    );
    for name in ROLE.iter().chain(ROSTER) {
        assert!(
            all.iter().any(|method| method == name),
            "{name} is not a manager method"
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
