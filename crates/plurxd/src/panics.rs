//! The process panic hook: a panic anywhere becomes a log line and a counter.
//!
//! Before this existed, `grep -rn "panic::set_hook" crates/plurxd/src` was
//! empty. A panicking detached task wrote to stderr and nothing else: it never
//! reached the in-memory ring the product shows at Settings → System → Logs,
//! and no counter anywhere moved, so a task that died on every attempt looked
//! exactly like a task that was never asked to run.
//!
//! **This is a report, not a repair.** A task that panics mid-publication
//! still leaves whatever it left; making that visible is this module's whole
//! job, and repairing it belongs to the efforts that own those lifecycles.

use std::cell::Cell;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use crate::redact::redact_bounded;

const MAX_PANIC_MESSAGE: usize = 512;
const MAX_BACKTRACE_FRAMES: usize = 32;
const MAX_BACKTRACE_FRAME: usize = 200;

/// The bounded subsystem vocabulary.
///
/// A panic location is a source path such as `crates/plurxd/src/http/mod.rs`,
/// and the label is the first module component after `crates/<crate>/src/`,
/// kept only when it is on this list. Everything else — a panic in a
/// dependency, in a module nobody listed here, or with no location at all —
/// is `other`. That is what makes the label space closed: a new module cannot
/// mint a series by existing, and neither can a third-party crate.
const PANIC_SUBSYSTEMS: [&str; 13] = [
    "http",
    "store",
    "cluster",
    "transcode",
    "vod",
    "live_tv",
    "media_sessions",
    "playback_control",
    "scan",
    "metadata",
    "produce",
    "main",
    "other",
];

const OTHER_SUBSYSTEM: usize = PANIC_SUBSYSTEMS.len() - 1;

static PANICS: LazyLock<[AtomicU64; PANIC_SUBSYSTEMS.len()]> =
    LazyLock::new(|| std::array::from_fn(|_| AtomicU64::new(0)));

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Runs `body` unless this thread is already inside the hook.
///
/// A panic raised *inside* the reporting path — a poisoned mutex in the log
/// ring, a subscriber layer that panics on third-party payload text — would
/// otherwise re-enter the hook and recurse until the stack is gone. The guard
/// is checked first and released before the default hook runs, so a later,
/// unrelated panic on the same thread still reports.
fn with_hook_guard<T>(body: impl FnOnce() -> T) -> Option<T> {
    if IN_HOOK.with(|flag| flag.replace(true)) {
        return None;
    }
    let outcome = body();
    IN_HOOK.with(|flag| flag.set(false));
    Some(outcome)
}

/// Installs the reporting hook in front of whatever hook is already set.
///
/// The previous hook is **chained, not replaced**: it always runs, at the end,
/// outside the recursion guard. The process's stderr output and its
/// abort-versus-unwind behaviour are unchanged by this module.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report_panic(info);
        previous(info);
    }));
}

/// The reporting half of the hook, separated so it can be exercised without
/// the test having to own the process-wide hook.
fn report_panic(info: &PanicHookInfo<'_>) {
    with_hook_guard(|| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()));
        let subsystem = subsystem_of(info.location().map(|location| location.file()));
        let message = redact_bounded(&panic_message(info), MAX_PANIC_MESSAGE);
        PANICS[subsystem].fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            target: "plurxd::panic",
            subsystem = PANIC_SUBSYSTEMS[subsystem],
            location = location.as_deref().unwrap_or("unknown"),
            backtrace = %bounded_backtrace(),
            "panic: {message}"
        );
    });
}

/// A panic payload is third-party text: `panic!("{}", response_body)` puts
/// whatever a remote said into it. It is read out of the two shapes the
/// standard library produces and is redacted by the caller before it is
/// logged.
fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

/// `crates/plurxd/src/http/mod.rs` → `http`;
/// `crates/plurx-core/src/store/sqlite/media.rs` → `store`;
/// `crates/plurxd/src/media_sessions.rs` → `media_sessions`.
fn subsystem_of(file: Option<&str>) -> usize {
    let Some(file) = file else {
        return OTHER_SUBSYSTEM;
    };
    // Normalise Windows separators so a cross-compiled location classifies the
    // same way as a Unix one.
    let normalised = file.replace('\\', "/");
    let Some((_, after_src)) = normalised.split_once("/src/") else {
        return OTHER_SUBSYSTEM;
    };
    if !normalised.starts_with("crates/") {
        return OTHER_SUBSYSTEM;
    }
    let component = after_src
        .split('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".rs");
    PANIC_SUBSYSTEMS
        .iter()
        .take(OTHER_SUBSYSTEM)
        .position(|known| *known == component)
        .unwrap_or(OTHER_SUBSYSTEM)
}

/// A bounded, path-free backtrace.
///
/// Captured only when the operator asked for one, because resolving symbols is
/// expensive and a panic storm would pay it per panic. The `at /path/file.rs:12`
/// lines are dropped rather than redacted: they name the build host's
/// filesystem, and the symbol line beside each one says the same thing without
/// a path. Every kept line still goes through `redact_bounded`, so a symbol
/// that does carry a path or a credential-shaped word is replaced.
fn bounded_backtrace() -> String {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        return "unavailable: RUST_BACKTRACE is unset".to_owned();
    }
    let captured = std::backtrace::Backtrace::force_capture().to_string();
    captured
        .lines()
        .filter(|line| !line.trim_start().starts_with("at "))
        .take(MAX_BACKTRACE_FRAMES)
        .map(|line| redact_bounded(line.trim(), MAX_BACKTRACE_FRAME))
        .collect::<Vec<_>>()
        .join(" | ")
}

pub(crate) fn prometheus_panics() -> String {
    let mut out = String::from(
        "# HELP plurx_panics_total Panics observed by the process hook, by bounded subsystem.\n\
         # TYPE plurx_panics_total counter\n",
    );
    for (index, subsystem) in PANIC_SUBSYSTEMS.iter().enumerate() {
        out.push_str(&format!(
            "plurx_panics_total{{subsystem=\"{subsystem}\"}} {}\n",
            PANICS[index].load(Ordering::Relaxed)
        ));
    }
    out
}

#[cfg(test)]
pub(crate) fn panic_count(subsystem: &str) -> u64 {
    let index = PANIC_SUBSYSTEMS
        .iter()
        .position(|known| *known == subsystem)
        .expect("known subsystem");
    PANICS[index].load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logbuf::testwriter::CapturedWriter;

    /// `RUST_BACKTRACE` is process-wide, so the two assertions that depend on
    /// it share one test rather than racing each other under the parallel
    /// test harness.
    #[test]
    fn the_backtrace_is_bounded_path_free_and_opt_in() {
        let restore = std::env::var_os("RUST_BACKTRACE");
        std::env::remove_var("RUST_BACKTRACE");
        assert_eq!(bounded_backtrace(), "unavailable: RUST_BACKTRACE is unset");

        std::env::set_var("RUST_BACKTRACE", "1");
        let backtrace = bounded_backtrace();
        match restore {
            Some(value) => std::env::set_var("RUST_BACKTRACE", value),
            None => std::env::remove_var("RUST_BACKTRACE"),
        }

        assert!(!backtrace.contains("unavailable"), "{backtrace}");
        let frames = backtrace.split(" | ").collect::<Vec<_>>();
        assert!(
            frames.len() <= MAX_BACKTRACE_FRAMES,
            "{} frames",
            frames.len()
        );
        for frame in frames {
            assert!(!frame.starts_with("at "), "{frame}");
            assert!(frame.chars().count() <= MAX_BACKTRACE_FRAME, "{frame}");
        }
    }

    #[test]
    fn the_subsystem_label_is_from_the_allowlist() {
        assert_eq!(
            PANIC_SUBSYSTEMS[subsystem_of(Some("crates/plurxd/src/http/mod.rs"))],
            "http"
        );
        assert_eq!(
            PANIC_SUBSYSTEMS[subsystem_of(Some("crates/plurx-core/src/store/sqlite/media.rs"))],
            "store"
        );
        assert_eq!(
            PANIC_SUBSYSTEMS[subsystem_of(Some("crates/plurxd/src/media_sessions.rs"))],
            "media_sessions"
        );
        assert_eq!(
            PANIC_SUBSYSTEMS[subsystem_of(Some("crates\\plurxd\\src\\http\\mod.rs"))],
            "http"
        );
        // A real plurxd module nobody listed, a dependency, and no location at
        // all all report `other` rather than minting a series.
        assert_eq!(
            PANIC_SUBSYSTEMS[subsystem_of(Some("crates/plurxd/src/waitpool.rs"))],
            "other"
        );
        assert_eq!(
            PANIC_SUBSYSTEMS[subsystem_of(Some(
                "/root/.cargo/registry/src/index.crates.io-6f17d22bba15001f/hyper-1.7.0/src/proto/h1/conn.rs"
            ))],
            "other"
        );
        assert_eq!(PANIC_SUBSYSTEMS[subsystem_of(None)], "other");
    }

    #[test]
    fn a_nested_report_is_suppressed_by_the_guard() {
        let outcome = with_hook_guard(|| with_hook_guard(|| "inner ran"));
        assert_eq!(outcome, Some(None));
        // The guard is released once the outer body returns, so a later,
        // unrelated panic on this thread still reports.
        assert_eq!(with_hook_guard(|| "later"), Some("later"));
    }

    #[test]
    fn the_exposition_names_every_subsystem_and_nothing_else() {
        let exposition = prometheus_panics();
        assert!(exposition.contains("# TYPE plurx_panics_total counter"));
        let cells = exposition
            .lines()
            .filter(|line| line.starts_with("plurx_panics_total{"))
            .count();
        assert_eq!(cells, PANIC_SUBSYSTEMS.len());
        for subsystem in PANIC_SUBSYSTEMS {
            assert!(
                exposition.contains(&format!("plurx_panics_total{{subsystem=\"{subsystem}\"}} ")),
                "{subsystem}"
            );
        }
    }

    /// Installs the real hook, panics for real, and asserts all four things the
    /// hook promises at once: the panic reaches `tracing` with its location,
    /// its third-party payload is redacted, the bounded counter moves, and the
    /// hook that was already installed still runs afterwards.
    ///
    /// Installing a process-wide hook from a test is deliberate. Testing a
    /// locally rebuilt copy of the same two halves would prove only that the
    /// copy chains.
    #[test]
    fn the_installed_hook_reports_redacts_counts_and_chains() {
        static PREVIOUS_RAN: AtomicU64 = AtomicU64::new(0);

        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            PREVIOUS_RAN.fetch_add(1, Ordering::Relaxed);
        }));
        install_panic_hook();

        let before_panics = panic_count("other");
        let before_previous = PREVIOUS_RAN.load(Ordering::Relaxed);
        let captured = CapturedWriter::new();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let _ = std::panic::catch_unwind(|| {
                panic!("authorization: Bearer sk-live-abcdef");
            });
        });
        std::panic::set_hook(original);

        let logged = captured.text();
        assert!(
            logged.contains("[redacted potentially sensitive operator text]"),
            "payload was not redacted: {logged}"
        );
        assert!(
            !logged.contains("sk-live-abcdef"),
            "the raw payload reached the log: {logged}"
        );
        assert!(
            logged.contains("crates/plurxd/src/panics.rs:"),
            "the location is missing: {logged}"
        );
        // This file's first module component is `panics`, which is not on the
        // allowlist, so the panic must land in `other`.
        assert_eq!(panic_count("other"), before_panics + 1);
        assert_eq!(
            PREVIOUS_RAN.load(Ordering::Relaxed),
            before_previous + 1,
            "the previously installed hook must still run"
        );
    }
}
