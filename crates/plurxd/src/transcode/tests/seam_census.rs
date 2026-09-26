//! TRANSCODE-DECOMPOSITION-PLAN §7 Q3: the `syn` pass over the test-only
//! seams in shipping code.
//!
//! `validation/cfg_test_census.py` classifies each literal `#[cfg(test)]` by
//! the first line it gates, which mis-files `match` arms and struct-literal
//! initialisers as fields and cannot tell a pause from a fault. This pass
//! parses every production source file in the workspace and classifies each
//! gated statement inside a shipping function by what it does:
//!
//! - `pause`: it waits on a rendezvous or a delay (`wait`, `notified`,
//!   `recv`, `acquire`, `changed`, `sleep`, `yield_now`, or awaiting a held
//!   receiver). These are ordering pauses, the hook-trait candidates of §3.9.
//! - `fault`: it injects a failure (`Err(..)`, `?` on a test-only call,
//!   `panic!`, a `pending()` hang). These are the one-shot faults, the
//!   `fail`-crate candidates of §3.9.
//! - `override`: it substitutes a value or a result the production code then
//!   uses (an assignment, or an early `return`/`break`/`continue` of a
//!   test-supplied value) — an alternate implementation, not a fault.
//! - `record`: it only records what happened for a test to read (counters,
//!   pushes, notifications without a wait).
//! - `plumbing`: a `let` that only carries a slot to one of the above.
//!
//! Gated struct fields, struct-literal initialisers and `match` arms are
//! counted apart from statements, each against the owner that holds it.
//! Files reached only through a `#[cfg(test)] mod` (or `include!`d from one)
//! are test code and are skipped, as are `#[cfg(test)]` items and `#[test]`
//! functions.
//!
//! Run `cargo test -p plurxd --bin plurxd seam_census -- --nocapture` to
//! print the table the plan records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use syn::visit::Visit;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Seam {
    Pause,
    Fault,
    Override,
    Record,
    Plumbing,
    Field,
    Initialiser,
    Arm,
}

impl Seam {
    const ALL: [Seam; 8] = [
        Seam::Pause,
        Seam::Fault,
        Seam::Override,
        Seam::Record,
        Seam::Plumbing,
        Seam::Field,
        Seam::Initialiser,
        Seam::Arm,
    ];

    fn label(self) -> &'static str {
        match self {
            Seam::Pause => "pause",
            Seam::Fault => "fault",
            Seam::Override => "override",
            Seam::Record => "record",
            Seam::Plumbing => "plumbing",
            Seam::Field => "field",
            Seam::Initialiser => "initialiser",
            Seam::Arm => "arm",
        }
    }
}

/// One gated site: the file, the owner (`Type::method`, a free function, or
/// the struct that holds a field) and its class.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    file: String,
    owner: String,
    seam: Seam,
}

/// `#[cfg(test)]` or `#[cfg(all(test, ..))]`: present only in test builds.
fn cfg_test(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        let syn::Meta::List(list) = &attribute.meta else {
            return false;
        };
        let predicate = list.tokens.to_string();
        let predicate = predicate.trim();
        predicate == "test"
            || predicate
                .strip_prefix("all")
                .map(str::trim)
                .and_then(|rest| rest.strip_prefix('('))
                .and_then(|rest| rest.strip_suffix(')'))
                .is_some_and(|arguments| arguments.split(',').any(|term| term.trim() == "test"))
    })
}

fn test_function(attributes: &[syn::Attribute]) -> bool {
    cfg_test(attributes)
        || attributes.iter().any(|attribute| {
            attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        })
}

fn expression_attributes(expression: &syn::Expr) -> &[syn::Attribute] {
    macro_rules! attributes {
        ($($variant:ident),*) => {
            match expression {
                $(syn::Expr::$variant(inner) => &inner.attrs,)*
                _ => &[],
            }
        };
    }
    attributes!(
        Array, Assign, Async, Await, Binary, Block, Break, Call, Cast, Closure, Const, Continue,
        Field, ForLoop, Group, If, Index, Let, Lit, Loop, Macro, Match, MethodCall, Paren, Path,
        Range, Reference, Repeat, Return, Struct, Try, Tuple, Unary, Unsafe, While
    )
}

const WAITS: &[&str] = &[
    "wait",
    "notified",
    "recv",
    "recv_timeout",
    "acquire",
    "acquire_owned",
    "changed",
    "sleep",
    "sleep_until",
    "yield_now",
];

const FAULT_MACROS: &[&str] = &["panic", "unreachable", "todo", "unimplemented", "bail"];

/// Calls that manufacture a failure or a hang.
const FAULT_CALLS: &[&str] = &["Err", "pending", "from_raw_os_error"];

/// What one gated statement does, gathered over its whole syntax tree.
#[derive(Default)]
struct Effects {
    waits: bool,
    faults: bool,
    assigns: bool,
    calls: bool,
}

impl<'ast> Visit<'ast> for Effects {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if WAITS.contains(&call.method.to_string().as_str()) {
            self.waits = true;
        }
        self.calls = true;
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            if let Some(last) = path.path.segments.last() {
                let name = last.ident.to_string();
                if WAITS.contains(&name.as_str()) {
                    self.waits = true;
                }
                if FAULT_CALLS.contains(&name.as_str()) {
                    self.faults = true;
                }
            }
        }
        self.calls = true;
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some(last) = mac.path.segments.last() {
            if FAULT_MACROS.contains(&last.ident.to_string().as_str()) {
                self.faults = true;
            }
        }
        self.calls = true;
    }

    fn visit_expr_await(&mut self, expression: &'ast syn::ExprAwait) {
        // Awaiting a held receiver or future (`release.await`) is a
        // rendezvous; awaiting a call is judged by the call.
        if matches!(
            expression.base.as_ref(),
            syn::Expr::Path(_) | syn::Expr::Field(_)
        ) {
            self.waits = true;
        }
        syn::visit::visit_expr_await(self, expression);
    }

    fn visit_expr_return(&mut self, expression: &'ast syn::ExprReturn) {
        self.assigns = true;
        syn::visit::visit_expr_return(self, expression);
    }

    fn visit_expr_try(&mut self, expression: &'ast syn::ExprTry) {
        self.faults = true;
        syn::visit::visit_expr_try(self, expression);
    }

    fn visit_expr_break(&mut self, expression: &'ast syn::ExprBreak) {
        self.assigns = true;
        syn::visit::visit_expr_break(self, expression);
    }

    fn visit_expr_continue(&mut self, expression: &'ast syn::ExprContinue) {
        self.assigns = true;
        syn::visit::visit_expr_continue(self, expression);
    }

    fn visit_expr_assign(&mut self, expression: &'ast syn::ExprAssign) {
        self.assigns = true;
        syn::visit::visit_expr_assign(self, expression);
    }

    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        use syn::BinOp::*;
        if matches!(
            expression.op,
            AddAssign(_)
                | SubAssign(_)
                | MulAssign(_)
                | DivAssign(_)
                | RemAssign(_)
                | BitXorAssign(_)
                | BitAndAssign(_)
                | BitOrAssign(_)
                | ShlAssign(_)
                | ShrAssign(_)
        ) {
            self.assigns = true;
        }
        syn::visit::visit_expr_binary(self, expression);
    }
}

fn classify_statement(statement: &syn::Stmt) -> Seam {
    let mut effects = Effects::default();
    effects.visit_stmt(statement);
    if effects.waits {
        Seam::Pause
    } else if effects.faults {
        Seam::Fault
    } else if effects.assigns {
        Seam::Override
    } else if matches!(statement, syn::Stmt::Local(_)) {
        Seam::Plumbing
    } else if effects.calls {
        Seam::Record
    } else {
        Seam::Plumbing
    }
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map_or_else(String::new, |segment| segment.ident.to_string()),
        _ => "_".to_owned(),
    }
}

/// Walks one production file, skipping test-only items.
struct Census<'a> {
    file: &'a str,
    impl_type: Vec<String>,
    function: Vec<String>,
    sites: Vec<Site>,
}

impl Census<'_> {
    fn owner(&self) -> String {
        match (self.impl_type.last(), self.function.last()) {
            (Some(ty), Some(function)) => format!("{ty}::{function}"),
            (None, Some(function)) => function.clone(),
            (Some(ty), None) => ty.clone(),
            (None, None) => "<module>".to_owned(),
        }
    }

    fn record(&mut self, owner: String, seam: Seam) {
        self.sites.push(Site {
            file: self.file.to_owned(),
            owner,
            seam,
        });
    }
}

impl<'ast> Visit<'ast> for Census<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if cfg_test(&item.attrs) {
            return;
        }
        self.impl_type.push(type_name(&item.self_ty));
        syn::visit::visit_item_impl(self, item);
        self.impl_type.pop();
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if cfg_test(&item.attrs) {
            return;
        }
        self.impl_type.push(item.ident.to_string());
        syn::visit::visit_item_trait(self, item);
        self.impl_type.pop();
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if test_function(&item.attrs) {
            return;
        }
        // A function nested in a method is its own owner.
        self.function.push(item.sig.ident.to_string());
        let outer = std::mem::take(&mut self.impl_type);
        syn::visit::visit_item_fn(self, item);
        self.impl_type = outer;
        self.function.pop();
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if test_function(&item.attrs) {
            return;
        }
        self.function.push(item.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, item);
        self.function.pop();
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if test_function(&item.attrs) {
            return;
        }
        self.function.push(item.sig.ident.to_string());
        syn::visit::visit_trait_item_fn(self, item);
        self.function.pop();
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if cfg_test(&item.attrs) {
            return;
        }
        let owner = item.ident.to_string();
        for field in &item.fields {
            if cfg_test(&field.attrs) {
                self.record(owner.clone(), Seam::Field);
            }
        }
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if cfg_test(&item.attrs) {
            return;
        }
        let owner = item.ident.to_string();
        for variant in &item.variants {
            if cfg_test(&variant.attrs) {
                self.record(owner.clone(), Seam::Field);
            }
        }
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        // Test-only consts, statics, uses, types and macros are helpers, not
        // seams (§3.9 "What is not migrated").
        let skip = match item {
            syn::Item::Const(inner) => cfg_test(&inner.attrs),
            syn::Item::Static(inner) => cfg_test(&inner.attrs),
            syn::Item::Use(inner) => cfg_test(&inner.attrs),
            syn::Item::Type(inner) => cfg_test(&inner.attrs),
            syn::Item::Macro(inner) => cfg_test(&inner.attrs),
            _ => false,
        };
        if !skip {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_expr_struct(&mut self, expression: &'ast syn::ExprStruct) {
        let owner = expression
            .path
            .segments
            .last()
            .map_or_else(String::new, |segment| segment.ident.to_string());
        let owner = if owner == "Self" {
            self.impl_type.last().cloned().unwrap_or(owner)
        } else {
            owner
        };
        for field in &expression.fields {
            if cfg_test(&field.attrs) {
                self.record(owner.clone(), Seam::Initialiser);
            }
        }
        syn::visit::visit_expr_struct(self, expression);
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if cfg_test(&arm.attrs) {
            let owner = self.owner();
            self.record(owner, Seam::Arm);
            return;
        }
        syn::visit::visit_arm(self, arm);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        for statement in &block.stmts {
            let (attributes, item) = match statement {
                syn::Stmt::Local(local) => (&local.attrs[..], false),
                syn::Stmt::Macro(mac) => (&mac.attrs[..], false),
                syn::Stmt::Expr(expression, _) => (expression_attributes(expression), false),
                syn::Stmt::Item(_) => (&[][..], true),
            };
            if item {
                self.visit_stmt(statement);
            } else if cfg_test(attributes) {
                let owner = self.owner();
                self.record(owner, classify_statement(statement));
            } else {
                self.visit_stmt(statement);
            }
        }
    }
}

fn census_of(file: &str, syntax: &syn::File) -> Vec<Site> {
    let mut census = Census {
        file,
        impl_type: Vec::new(),
        function: Vec::new(),
        sites: Vec::new(),
    };
    census.visit_file(syntax);
    census.sites
}

fn path_attribute(attributes: &[syn::Attribute]) -> Option<String> {
    attributes
        .iter()
        .find_map(|attribute| match &attribute.meta {
            syn::Meta::NameValue(pair) if pair.path.is_ident("path") => match &pair.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(literal),
                    ..
                }) => Some(literal.value()),
                _ => None,
            },
            _ => None,
        })
}

/// The files a `mod name;` declaration in `declaring` can load.
fn module_file(declaring: &Path, item: &syn::ItemMod) -> Option<PathBuf> {
    let directory = declaring.parent()?;
    if let Some(path) = path_attribute(&item.attrs) {
        return Some(directory.join(path));
    }
    let name = item.ident.to_string();
    let stem = declaring.file_stem()?.to_string_lossy().into_owned();
    [
        directory.join(&stem).join(format!("{name}.rs")),
        directory.join(&stem).join(&name).join("mod.rs"),
        directory.join(format!("{name}.rs")),
        directory.join(&name).join("mod.rs"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// Every out-of-line module declared under `#[cfg(test)]`, and every file a
/// test-only file declares or `include!`s.
struct TestFiles<'a> {
    declaring: &'a Path,
    test_only_file: bool,
    depth_test: usize,
    found: Vec<PathBuf>,
}

impl<'ast> Visit<'ast> for TestFiles<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let test = cfg_test(&item.attrs);
        if item.content.is_none() {
            if self.test_only_file || self.depth_test > 0 || test {
                if let Some(file) = module_file(self.declaring, item) {
                    self.found.push(file);
                }
            }
            return;
        }
        if test {
            self.depth_test += 1;
        }
        syn::visit::visit_item_mod(self, item);
        if test {
            self.depth_test -= 1;
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if !(self.test_only_file || self.depth_test > 0) || !mac.path.is_ident("include") {
            return;
        }
        if let Ok(literal) = mac.parse_body::<syn::LitStr>() {
            if let Some(directory) = self.declaring.parent() {
                self.found.push(directory.join(literal.value()));
            }
        }
    }
}

fn rust_sources(root: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The census of every production file under `crates/*/src`, and how many
/// test-only files it skipped.
fn workspace_census(workspace: &Path) -> (Vec<Site>, usize, usize) {
    let mut sources = Vec::new();
    let mut crates: Vec<PathBuf> = std::fs::read_dir(workspace.join("crates"))
        .expect("workspace crates")
        .map(|entry| entry.expect("crate entry").path().join("src"))
        .filter(|path| path.is_dir())
        .collect();
    crates.sort();
    for source in &crates {
        rust_sources(source, &mut sources);
    }
    sources.sort();
    let parsed: BTreeMap<PathBuf, syn::File> = sources
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("Rust source");
            let syntax = syn::parse_file(&text)
                .unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
            (canonical(path), syntax)
        })
        .collect();
    let mut test_only: BTreeSet<PathBuf> = BTreeSet::new();
    let mut frontier: Vec<(PathBuf, bool)> =
        parsed.keys().map(|path| (path.clone(), false)).collect();
    while let Some((path, test_only_file)) = frontier.pop() {
        let Some(syntax) = parsed.get(&path) else {
            continue;
        };
        let mut finder = TestFiles {
            declaring: &path,
            test_only_file,
            depth_test: 0,
            found: Vec::new(),
        };
        finder.visit_file(syntax);
        for file in finder.found {
            let file = canonical(&file);
            if test_only.insert(file.clone()) {
                frontier.push((file, true));
            }
        }
    }
    let mut sites = Vec::new();
    let mut production = 0;
    for (path, syntax) in &parsed {
        if test_only.contains(path) {
            continue;
        }
        production += 1;
        let relative = path
            .strip_prefix(canonical(workspace))
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        sites.extend(census_of(&relative, syntax));
    }
    let skipped = parsed
        .keys()
        .filter(|path| test_only.contains(*path))
        .count();
    (sites, production, skipped)
}

fn render(sites: &[Site], production: usize, skipped: usize) -> String {
    let mut out = format!(
        "seam census (syn): {production} production files, {skipped} test-only files skipped\n"
    );
    let mut totals: BTreeMap<Seam, usize> = BTreeMap::new();
    let mut owners: BTreeMap<(&str, &str), BTreeMap<Seam, usize>> = BTreeMap::new();
    for site in sites {
        *totals.entry(site.seam).or_default() += 1;
        *owners
            .entry((site.file.as_str(), site.owner.as_str()))
            .or_default()
            .entry(site.seam)
            .or_default() += 1;
    }
    out.push_str("class,count\n");
    for seam in Seam::ALL {
        out.push_str(&format!(
            "{},{}\n",
            seam.label(),
            totals.get(&seam).copied().unwrap_or(0)
        ));
    }
    out.push_str("file,owner");
    for seam in Seam::ALL {
        out.push(',');
        out.push_str(seam.label());
    }
    out.push('\n');
    for ((file, owner), counts) in owners {
        out.push_str(&format!("{file},{owner}"));
        for seam in Seam::ALL {
            out.push_str(&format!(",{}", counts.get(&seam).copied().unwrap_or(0)));
        }
        out.push('\n');
    }
    out
}

fn fixture_census(source: &str) -> Vec<(String, Seam)> {
    let syntax = syn::parse_file(source).expect("census fixture is valid Rust");
    census_of("fixture.rs", &syntax)
        .into_iter()
        .map(|site| (site.owner, site.seam))
        .collect()
}

#[test]
fn seam_census_classifies_pauses_faults_and_the_rest() {
    // Spelled `cfg(TEST)` here and swapped before parsing, so the line census
    // (`validation/cfg_test_census.py`), which reads source lines, does not
    // count this fixture's text as seams in this file.
    let sites = fixture_census(
        &r#"
        struct Owner {
            live: u32,
            #[cfg(TEST)]
            pause: Mutex<Option<Arc<Barrier>>>,
        }
        enum Command {
            Run,
            #[cfg(TEST)]
            Inject,
        }
        impl Owner {
            fn new() -> Self {
                Self {
                    live: 0,
                    #[cfg(TEST)]
                    pause: Mutex::new(None),
                }
            }
            async fn run(&self, command: Command) -> Result<(), String> {
                #[cfg(TEST)]
                let pause = self.pause.lock().unwrap().take();
                #[cfg(TEST)]
                if let Some(pause) = pause {
                    pause.wait().await;
                }
                #[cfg(TEST)]
                sleep(test_delay()).await;
                #[cfg(TEST)]
                if let Some(release) = self.release.take() {
                    let _ = release.await;
                }
                #[cfg(TEST)]
                if FAIL_NEXT.swap(false, SeqCst) {
                    return Err("injected".into());
                }
                #[cfg(TEST)]
                fail_once()?;
                #[cfg(TEST)]
                if let Some(result) = self.scripted.take() {
                    return result;
                }
                #[cfg(all(TEST, target_os = "linux"))]
                if HANG.load(SeqCst) {
                    return std::future::pending().await;
                }
                let mut limit = 3;
                #[cfg(TEST)]
                {
                    limit = 1;
                }
                #[cfg(TEST)]
                CALLS.fetch_add(1, SeqCst);
                match command {
                    Command::Run => Ok(()),
                    #[cfg(TEST)]
                    Command::Inject => Err("injected".into()),
                }
            }
            #[cfg(TEST)]
            fn test_helper(&self) {
                #[cfg(TEST)]
                CALLS.fetch_add(1, SeqCst);
            }
        }
        fn free() {
            #[cfg(TEST)]
            CALLS.fetch_add(1, SeqCst);
        }
        #[test]
        fn a_test() {
            #[cfg(TEST)]
            CALLS.fetch_add(1, SeqCst);
        }
        #[cfg(TEST)]
        mod tests {
            fn helper() {
                #[cfg(TEST)]
                CALLS.fetch_add(1, SeqCst);
            }
        }
        "#
        .replace("TEST", "test"),
    );
    let expected: Vec<(String, Seam)> = [
        ("Owner", Seam::Field),
        ("Command", Seam::Field),
        ("Owner", Seam::Initialiser),
        ("Owner::run", Seam::Plumbing),
        ("Owner::run", Seam::Pause),
        ("Owner::run", Seam::Pause),
        ("Owner::run", Seam::Pause),
        ("Owner::run", Seam::Fault),
        ("Owner::run", Seam::Fault),
        ("Owner::run", Seam::Override),
        ("Owner::run", Seam::Fault),
        ("Owner::run", Seam::Override),
        ("Owner::run", Seam::Record),
        ("Owner::run", Seam::Arm),
        ("free", Seam::Record),
    ]
    .into_iter()
    .map(|(owner, seam)| (owner.to_owned(), seam))
    .collect();
    assert_eq!(sites, expected);
}

#[test]
fn seam_census_skips_files_reached_only_through_test_modules() {
    let root = crate::test_tempdir().expect("census root");
    let source = root.path().join("crates/demo/src");
    std::fs::create_dir_all(source.join("owner")).expect("source tree");
    std::fs::write(
        source.join("lib.rs"),
        "mod owner;\n#[cfg(test)]\n#[path = \"owner/tests.rs\"]\nmod tests;\n\
         fn shipping() {\n    #[cfg(test)]\n    CALLS.fetch_add(1, SeqCst);\n}\n",
    )
    .expect("lib.rs");
    std::fs::write(
        source.join("owner.rs"),
        "#[cfg(test)]\nmod inner;\nfn owned() {\n    #[cfg(test)]\n    PAUSE.wait();\n}\n",
    )
    .expect("owner.rs");
    std::fs::write(
        source.join("owner/inner.rs"),
        "fn helper() {\n    #[cfg(test)]\n    PAUSE.wait();\n}\n",
    )
    .expect("owner/inner.rs");
    std::fs::write(
        source.join("owner/tests.rs"),
        "include!(\"chunk.rs\");\nfn helper() {\n    #[cfg(test)]\n    PAUSE.wait();\n}\n",
    )
    .expect("owner/tests.rs");
    std::fs::write(
        source.join("owner/chunk.rs"),
        "fn chunk() {\n    #[cfg(test)]\n    PAUSE.wait();\n}\n",
    )
    .expect("owner/chunk.rs");
    let (sites, production, skipped) = workspace_census(root.path());
    assert_eq!((production, skipped), (2, 3));
    let owners: Vec<(&str, Seam)> = sites
        .iter()
        .map(|site| (site.owner.as_str(), site.seam))
        .collect();
    assert_eq!(owners, [("shipping", Seam::Record), ("owned", Seam::Pause)]);
}

/// The §7 Q3 census of the workspace, printed for the plan. It pins only what
/// M8 has migrated: an owner whose seams became a hook trait keeps none.
#[test]
fn seam_census_of_the_workspace() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let (sites, production, skipped) = workspace_census(workspace);
    println!("{}", render(&sites, production, skipped));
    assert!(
        sites.iter().any(|site| site.seam == Seam::Pause),
        "the census reads the real tree"
    );
    let migrated: Vec<&Site> = sites
        .iter()
        .filter(|site| {
            site.file.ends_with("transcode/producer/attempt_child.rs")
                || site.owner.starts_with("AttemptChild")
                || site.owner.starts_with("RollingRetirementSettlement")
                || site.owner.starts_with("DecodeFactSource")
        })
        .collect();
    assert!(
        migrated.is_empty(),
        "M8-migrated owners keep no test-only seams: {migrated:?}"
    );
}
