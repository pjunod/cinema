//! A text burn's frozen Fontconfig environment.
//!
//! A text-burn recipe used to say "the fonts I saw" and then re-enumerate
//! Fontconfig before every launch and every segment, because checking only
//! the captured files catches replacements and removals but not additions.
//! That cost two `fc-*` children and a stat of every font per segment.
//!
//! Instead, capture now *builds* the Fontconfig environment the producer
//! reads, from what the live enumeration saw, in a directory only `plurxd`
//! writes, and the producer runs with `FONTCONFIG_SYSROOT` pointing at it
//! (docs/streaming/FONT-ATTESTATION-AND-BLOCKING-IO.md §3.2 and §7). A font
//! or rule installed later is not visible to the child at all, so there is
//! nothing to re-enumerate; what remains per check is a stat of the captured
//! objects.
//!
//! **Why a sysroot and not a rewritten configuration.** Fontconfig applies a
//! file's rules in document order with every `<include>` expanded in place:
//! at each include it flushes the rules read so far, then parses the included
//! files (`fcxml.c`, `FcParseInclude`). `fc-conflist` reports files in the
//! order they *finished* loading, which puts a parent after its children
//! even when its rules run first — `fonts.conf`'s `mono → monospace` rewrite
//! is the common example. Reproducing that order from outside would mean
//! reimplementing include resolution (search path, `~`, XDG prefixes,
//! directory sorting, real-path deduplication). Under a sysroot, Fontconfig
//! resolves every `<dir>`, `<include>` and `<cachedir>` itself, against
//! byte-for-byte copies of the files it loaded, placed at their original
//! paths — so the tree, and therefore the rule order, is its own.
//!
//! ```text
//! <runtime_cache>/fontenv/<digest16>-<n>/
//!   root/            FONTCONFIG_SYSROOT
//!     <each loaded configuration file's path>   a byte copy
//!     <each listed font file's path>            a symlink to that file
//!     …                                        caches Fontconfig writes
//!   probe.ass        the one-frame script the capture renders
//!   manifest.json    what each copy and link came from, for an operator
//! ```
//!
//! Three properties are proved at capture, not assumed: under the system
//! `fc-*` tools the frozen environment loads exactly the configuration files
//! the live one does, in the same order, and lists exactly the same faces;
//! and the producer's own ffmpeg, whose libass links whatever libfontconfig
//! that build bundles, loads only configuration inside the sysroot and the
//! same set of files. A closure the freeze cannot reproduce, or a producer
//! library that ignores the sysroot, fails one of them and the burn is
//! refused rather than rendered with other fonts or rules.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// Where frozen environments live under the runtime cache.
pub(crate) const FONT_ENVIRONMENT_DIR: &str = "fontenv";

/// The live closure was `plurx/font-render/engine-v1`; a frozen digest can
/// never equal one.
const FROZEN_FONT_DIGEST_VERSION: &[u8] = b"plurx/font-render/engine-v2\0";

/// What `fc-list` prints on both sides of the parity check: one line per
/// face, so a difference that drops, renames or restyles a face is visible,
/// not only one that drops a file.
pub(crate) const FONT_LISTING_FORMAT: &str = "--format=%{file}\t%{index}\t%{family}\t%{style}\n";

/// A configuration file larger than this is not a Fontconfig rule file.
const RULE_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The script the capture burns once under the producer's own ffmpeg, beside
/// (not inside) the sysroot. One event, so libass resolves a face and the
/// producer library's first scan — on the new root's empty cache — builds the
/// cache every later launch reads.
const PROBE_SCRIPT_NAME: &str = "probe.ass";
const PROBE_SCRIPT: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 64\nPlayResY: 64\n\n\
     [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, BackColour, \
     Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
     Style: Default,sans-serif,20,&H00FFFFFF,&H00000000,&H00000000,0,0,1,1,0,2,0,0,0,1\n\n\
     [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
     Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,Ag\n";

/// Fontconfig's `FC_DBG_CONFIG`: the library prints every configuration file
/// and directory it loads or scans to stdout.
const FC_DEBUG_CONFIG: &str = "1024";

/// What the live enumeration saw, in the shape freezing needs.
pub(crate) struct FontSources<'a> {
    /// The live closure's digest: both listings and every object version.
    pub(crate) digest: &'a str,
    /// Every configuration file Fontconfig loaded, in `fc-conflist` order
    /// (the order each finished loading; the root configuration is last).
    pub(crate) rules: &'a [PathBuf],
    /// Every font file, sorted and unique.
    pub(crate) fonts: &'a [PathBuf],
    /// The version the live enumeration attested for each rule and font.
    pub(crate) versions: &'a HashMap<PathBuf, String>,
    /// The live `fc-list` answer in [`FONT_LISTING_FORMAT`].
    pub(crate) listing: &'a str,
}

/// One frozen environment, shared by every recipe captured from the same
/// closure. Dropping the last reference removes its directory.
#[derive(Debug)]
pub(crate) struct FontEnvironment {
    dir: PathBuf,
    root: PathBuf,
    config: PathBuf,
    digest: String,
    objects: Arc<[(PathBuf, String)]>,
}

impl FontEnvironment {
    /// The child-local variables a producer renders under: the sysroot, and
    /// the root configuration by its original path, which Fontconfig reads
    /// under that sysroot.
    pub(crate) fn child_env(&self) -> [(&'static str, &std::ffi::OsStr); 2] {
        [
            ("FONTCONFIG_SYSROOT", self.root.as_os_str()),
            ("FONTCONFIG_FILE", self.config.as_os_str()),
        ]
    }

    /// The frozen digest: the live closure and the root it was loaded from.
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    /// Everything the child reads, with the version captured for it: every
    /// configuration copy, every link (statted through it, so a replaced
    /// target and a repointed link both change the answer), and every
    /// directory holding one, so an entry added inside the sysroot does too.
    pub(crate) fn objects(&self) -> Arc<[(PathBuf, String)]> {
        Arc::clone(&self.objects)
    }

    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for FontEnvironment {
    fn drop(&mut self) {
        release(std::mem::take(&mut self.dir));
    }
}

/// Remove an environment's directory on the blocking pool. Called with the
/// last recipe's release, which can be on a runtime worker, and when a
/// capture that owned the directory fails or is cancelled; the next
/// process's sweep covers a removal that never runs.
fn release(dir: PathBuf) {
    let remove = move || {
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    target: "plurxd::ffmpeg",
                    path = %dir.display(),
                    "could not remove a released font environment: {error}"
                );
            }
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => drop(handle.spawn_blocking(remove)),
        Err(_) => remove(),
    }
}

/// The directory of an environment still being built. It exists before the
/// first write and is moved into every blocking task that writes there and
/// back out, so whichever way the capture ends — refused, or cancelled by its
/// caller's deadline while a task or an `fc-*` child is in flight — dropping
/// it removes the directory; only [`Owned::hand_over`] to the finished
/// [`FontEnvironment`] keeps it. §7.4: ownership, not age.
struct Owned {
    dir: PathBuf,
    kept: bool,
}

impl Owned {
    fn new(dir: PathBuf) -> Self {
        Self { dir, kept: false }
    }

    fn path(&self) -> &Path {
        &self.dir
    }

    fn hand_over(mut self) -> PathBuf {
        self.kept = true;
        std::mem::take(&mut self.dir)
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.kept {
            release(std::mem::take(&mut self.dir));
        }
    }
}

/// What freezing cost, charged by the caller with the rest of the capture.
#[derive(Default)]
pub(crate) struct FreezeCost {
    pub(crate) spawn: Duration,
    pub(crate) stat: Duration,
}

#[derive(Default)]
struct Registry {
    /// Roots whose predecessor-process environments this process removed.
    swept: HashSet<PathBuf>,
    live: HashMap<(PathBuf, String), Weak<FontEnvironment>>,
    next: u64,
}

fn registry() -> &'static tokio::sync::Mutex<Registry> {
    static REGISTRY: std::sync::OnceLock<tokio::sync::Mutex<Registry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Run filesystem work on the blocking pool; a panicked task is a failure,
/// never a silent success.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(format!("font environment task failed: {error}")))
}

/// Freeze the live closure under `runtime_cache`, or reuse the environment
/// another recipe froze from the same closure if it is still exactly as
/// captured.
pub(crate) async fn freeze(
    runtime_cache: &Path,
    sources: FontSources<'_>,
) -> (Result<Arc<FontEnvironment>, String>, FreezeCost) {
    let mut cost = FreezeCost::default();
    let result = freeze_charged(runtime_cache, sources, &mut cost).await;
    (result, cost)
}

fn attested(
    paths: &[PathBuf],
    versions: &HashMap<PathBuf, String>,
    what: &str,
) -> Result<Vec<(PathBuf, String)>, String> {
    paths
        .iter()
        .map(|path| {
            versions
                .get(path)
                .map(|version| (path.clone(), version.clone()))
                .ok_or_else(|| format!("{} was {what} but not attested", path.display()))
        })
        .collect()
}

async fn freeze_charged(
    runtime_cache: &Path,
    sources: FontSources<'_>,
    cost: &mut FreezeCost,
) -> Result<Arc<FontEnvironment>, String> {
    if sources.fonts.is_empty() {
        return Err("Fontconfig listed no font files".to_owned());
    }
    // The root configuration finishes loading last.
    let config = sources
        .rules
        .last()
        .cloned()
        .ok_or_else(|| "Fontconfig reported no loaded configuration".to_owned())?;
    let fonts = attested(sources.fonts, sources.versions, "listed")?;
    let rules = attested(sources.rules, sources.versions, "loaded")?;

    let mut digest = Sha256::new();
    digest.update(FROZEN_FONT_DIGEST_VERSION);
    digest.update(sources.digest.as_bytes());
    digest.update(config.as_os_str().as_encoded_bytes());
    let digest = hex::encode(digest.finalize());

    let mut registry = registry().lock().await;
    let runtime_cache = runtime_cache.to_path_buf();
    if registry.swept.insert(runtime_cache.clone()) {
        // Every encoded key already dies with its process, so nothing a
        // predecessor froze can be in use by this one.
        let previous = runtime_cache.join(FONT_ENVIRONMENT_DIR);
        let started = Instant::now();
        let swept = blocking(move || match std::fs::remove_dir_all(&previous) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("clearing {}: {error}", previous.display())),
        })
        .await;
        cost.stat += started.elapsed();
        swept?;
    }
    registry.live.retain(|_, live| live.strong_count() > 0);
    let key = (runtime_cache.clone(), digest.clone());
    if let Some(existing) = registry.live.get(&key).and_then(Weak::upgrade) {
        let (current, elapsed) =
            crate::ffmpeg::engine_objects_are_current_batch(None, existing.objects()).await;
        cost.stat += elapsed;
        if current {
            return Ok(existing);
        }
    }
    registry.next += 1;
    // Owned before anything is written: a capture its caller abandons (a
    // resurrection's `timeout_at`) removes what it built instead of leaving
    // it for the next process.
    let owned = Owned::new(runtime_cache.join(FONT_ENVIRONMENT_DIR).join(format!(
        "{}-{}",
        &digest[..16],
        registry.next
    )));
    let environment = Arc::new(
        build(
            &runtime_cache,
            owned,
            Frozen {
                digest,
                config,
                rules,
                fonts,
            },
            sources,
            cost,
        )
        .await?,
    );
    registry.live.insert(key, Arc::downgrade(&environment));
    Ok(environment)
}

struct Frozen {
    digest: String,
    config: PathBuf,
    rules: Vec<(PathBuf, String)>,
    fonts: Vec<(PathBuf, String)>,
}

/// Where `path` lives inside the sysroot.
fn under(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let relative = path
        .strip_prefix("/")
        .map_err(|_| format!("{} is not absolute", path.display()))?;
    Ok(root.join(relative))
}

/// Fontconfig reports paths with the sysroot stripped (font files) or kept
/// (configuration files); read both as the original path.
fn outside<'a>(root: &Path, reported: &'a str) -> std::borrow::Cow<'a, str> {
    match Path::new(reported).strip_prefix(root) {
        Ok(relative) => std::borrow::Cow::Owned(format!("/{}", relative.display())),
        Err(_) => std::borrow::Cow::Borrowed(reported),
    }
}

async fn build(
    runtime_cache: &Path,
    owned: Owned,
    frozen: Frozen,
    sources: FontSources<'_>,
    cost: &mut FreezeCost,
) -> Result<FontEnvironment, String> {
    let started = Instant::now();
    let materialised = {
        let rules = frozen.rules.clone();
        let fonts = frozen.fonts.clone();
        // The guard rides inside the task: a task that outlives its awaiting
        // future hands its output, and so the guard, to the runtime to drop.
        blocking(move || {
            let layout = materialise(owned.path(), &rules, &fonts)?;
            Ok((owned, layout))
        })
        .await
    };
    cost.stat += started.elapsed();
    let (owned, layout) = materialised?;
    #[cfg(test)]
    tests::hooks::paused(runtime_cache).await;

    let command = |program: &Path| {
        let mut command = tokio::process::Command::new(program);
        command
            .env("FONTCONFIG_SYSROOT", &layout.root)
            .env("FONTCONFIG_FILE", &frozen.config);
        command
    };
    let probe = |command: tokio::process::Command, what: &'static str| {
        let started = Instant::now();
        async move {
            let output = crate::ffmpeg::font_probe_output(command).await;
            (
                output.map_err(|error| format!("{what}: {error}")),
                started.elapsed(),
            )
        }
    };

    // The producer's own library runs first, before anything else has read
    // or written this environment. libass links whatever libfontconfig the
    // ffmpeg build bundles (Jellyfin FFmpeg ships its own beside an older
    // system one), so one burn under the child's exact environment, with
    // Fontconfig tracing what it loads, is the only proof that *this*
    // library resolves inside the sysroot. That trace must not depend on
    // cache warmth: before 2.17 a library prints it only when it has no
    // valid cache to read (it reaches `FcInitDebug` through the default
    // configuration a cold scan loads, never through libass's own parse),
    // and the `fc-list` below would otherwise have built one in the same
    // format. Under a sysroot Fontconfig resolves every `<cachedir>` —
    // absolute, `~` or `prefix="xdg"` — inside the sysroot, so this brand-new
    // root is itself the probe's own empty cache and every probe is a cold
    // load. Its scan then leaves the cache, in the producer library's own
    // format, that every later launch of the recipe reads.
    let program = producer_program(runtime_cache)?;
    let mut render = command(&program);
    crate::producer_spawn::configure_ffmpeg_runtime(&mut render, runtime_cache);
    render
        .current_dir(owned.path())
        .env("FC_DEBUG", FC_DEBUG_CONFIG)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:d=0.04",
            "-vf",
        ])
        .arg(format!("subtitles={PROBE_SCRIPT_NAME}"))
        .args(["-frames:v", "1", "-f", "null", "-"]);
    let (traced, elapsed) = probe(render, "rendering under the frozen environment").await;
    cost.spawn += elapsed;

    // Parity under the system library, judged before the producer's trace:
    // a closure the freeze cannot reproduce is named as such, whatever the
    // producer then made of it.
    let (loaded, elapsed) = probe(
        command(Path::new("fc-conflist")),
        "listing the frozen rules",
    )
    .await;
    cost.spawn += elapsed;
    rules_match(
        &layout.root,
        sources.rules,
        &String::from_utf8_lossy(&loaded?),
    )?;
    let mut list = command(Path::new("fc-list"));
    list.arg(FONT_LISTING_FORMAT);
    let (listed, elapsed) = probe(list, "listing the frozen fonts").await;
    cost.spawn += elapsed;
    faces_match(
        &layout.root,
        sources.listing,
        &String::from_utf8_lossy(&listed?),
    )?;
    // A cold probe that still traced nothing is refused here, never skipped.
    library_loads_match(
        &layout.root,
        sources.rules,
        &String::from_utf8_lossy(&traced?),
    )?;

    let started = Instant::now();
    let attested = {
        let digest = frozen.digest.clone();
        let config = frozen.config.clone();
        let rules = frozen.rules;
        let fonts = frozen.fonts;
        blocking(move || {
            let attested = attest(owned.path(), &digest, &config, &rules, &fonts, layout)?;
            Ok((owned, attested))
        })
        .await
    };
    cost.stat += started.elapsed();
    let (owned, (root, objects)) = attested?;
    Ok(FontEnvironment {
        dir: owned.hand_over(),
        root,
        config: frozen.config,
        digest: frozen.digest,
        objects: objects.into(),
    })
}

/// The encoder a producer for this recipe runs.
fn producer_program(_runtime_cache: &Path) -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(program) = tests::hooks::producer_program(_runtime_cache) {
        return Ok(program);
    }
    crate::ffmpeg::encoder_executable_path()
        .ok_or_else(|| "cannot resolve the encoder executable".to_owned())
}

struct Layout {
    root: PathBuf,
    /// Every copy and link, with the original it stands for.
    entries: Vec<(PathBuf, PathBuf)>,
}

#[cfg(unix)]
fn link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn link(_target: &Path, _link: &Path) -> std::io::Result<()> {
    // Windows libass uses DirectWrite; the live enumeration already refuses
    // a text burn there, so this is unreachable in practice.
    Err(std::io::Error::other(
        "a frozen Fontconfig environment needs symlinks",
    ))
}

fn materialise(
    dir: &Path,
    rules: &[(PathBuf, String)],
    fonts: &[(PathBuf, String)],
) -> Result<Layout, String> {
    let io = |what: &'static str, path: &Path| {
        let path = path.display().to_string();
        move |error: std::io::Error| format!("{what} {path}: {error}")
    };
    let root = dir.join("root");
    std::fs::create_dir_all(&root).map_err(io("creating", &root))?;
    let script = dir.join(PROBE_SCRIPT_NAME);
    std::fs::write(&script, PROBE_SCRIPT).map_err(io("writing", &script))?;
    // Fontconfig realpaths the sysroot before comparing prefixes.
    let root = std::fs::canonicalize(&root).map_err(io("resolving", &root))?;
    let mut entries = Vec::with_capacity(rules.len() + fonts.len());
    for (rule, version) in rules {
        let copy = under(&root, rule)?;
        if let Some(parent) = copy.parent() {
            std::fs::create_dir_all(parent).map_err(io("creating", parent))?;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(rule)
            .and_then(|file| file.take(RULE_MAX_BYTES + 1).read_to_end(&mut bytes))
            .map_err(io("reading", rule))?;
        if bytes.len() as u64 > RULE_MAX_BYTES {
            return Err(format!("{} is not a rule file", rule.display()));
        }
        // The copy must hold the version the live closure attested, or the
        // frozen rules would describe a configuration nobody saw.
        if crate::ffmpeg::engine_path_version(rule)? != *version {
            return Err(format!(
                "{} changed while it was being frozen",
                rule.display()
            ));
        }
        std::fs::write(&copy, &bytes).map_err(io("writing", &copy))?;
        entries.push((copy, rule.clone()));
    }
    for (font, version) in fonts {
        let path = under(&root, font)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io("creating", parent))?;
        }
        link(font, &path).map_err(io("linking", &path))?;
        // Statted through the link: the file the child will open must be the
        // version the live enumeration attested.
        if crate::ffmpeg::engine_path_version(&path)? != *version {
            return Err(format!(
                "{} changed while it was being frozen",
                font.display()
            ));
        }
        entries.push((path, font.clone()));
    }
    Ok(Layout { root, entries })
}

/// The frozen environment must load exactly the configuration files the live
/// one did, in the same order. With the same bytes at the same paths that is
/// the same include tree, and therefore the same rule order.
fn rules_match(root: &Path, live: &[PathBuf], frozen: &str) -> Result<(), String> {
    let loaded: Vec<String> = frozen
        .lines()
        .filter_map(|line| line.strip_prefix("+ "))
        .filter_map(|line| line.split_once(": ").map(|(path, _)| path))
        .map(|path| outside(root, path).into_owned())
        .collect();
    let live: Vec<String> = live.iter().map(|path| path.display().to_string()).collect();
    if loaded == live {
        return Ok(());
    }
    Err(format!(
        "the frozen Fontconfig environment loads {} configuration files where the live one \
         loads {} (first difference at position {}); the burn would render under other rules",
        loaded.len(),
        live.len(),
        loaded
            .iter()
            .zip(&live)
            .position(|(frozen, live)| frozen != live)
            .unwrap_or(loaded.len().min(live.len()))
    ))
}

/// The frozen environment must list exactly the faces the live one does.
fn faces_match(root: &Path, live: &str, frozen: &str) -> Result<(), String> {
    let faces = |listing: &str| -> Vec<String> {
        let mut faces: Vec<String> = listing
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (file, rest) = line.split_once('\t').unwrap_or((line, ""));
                format!("{}\t{rest}", outside(root, file))
            })
            .collect();
        faces.sort();
        faces
    };
    let (frozen, live) = (faces(frozen), faces(live));
    if frozen == live {
        return Ok(());
    }
    let frozen_set: BTreeSet<&String> = frozen.iter().collect();
    let live_set: BTreeSet<&String> = live.iter().collect();
    Err(format!(
        "the frozen Fontconfig environment resolves {} faces where the live one resolves {} \
         ({} missing, {} different); the burn would render with other fonts",
        frozen.len(),
        live.len(),
        live_set.difference(&frozen_set).count(),
        frozen_set.difference(&live_set).count(),
    ))
}

/// The producer's own Fontconfig, traced with `FC_DEBUG` while it burnt the
/// probe, must have read configuration only inside the sysroot, and loaded
/// the same set of files the live enumeration did. The trace is in start
/// order, not `fc-conflist`'s finish order, and a library may load its
/// configuration more than once, so this compares sets; the order was
/// proved by [`rules_match`] and the tree is the same bytes at the same
/// paths.
fn library_loads_match(root: &Path, live: &[PathBuf], trace: &str) -> Result<(), String> {
    let mut loaded = BTreeSet::new();
    let mut escaped = BTreeSet::new();
    for line in trace.lines() {
        let line = line.trim_start();
        let (loading, rest) = if let Some(rest) = line.strip_prefix("Loading config file ") {
            (true, rest)
        } else if let Some(rest) = line
            .strip_prefix("Scanning config file ")
            .or_else(|| line.strip_prefix("Scanning config dir "))
        {
            (false, rest)
        } else {
            continue;
        };
        let path = rest.strip_prefix("from ").unwrap_or(rest);
        let path = path.strip_suffix(" done").unwrap_or(path);
        if Path::new(path).strip_prefix(root).is_err() {
            escaped.insert(path.to_owned());
        } else if loading {
            loaded.insert(outside(root, path).into_owned());
        }
    }
    if let Some(first) = escaped.first() {
        return Err(format!(
            "the producer's Fontconfig read {} configuration paths outside the frozen \
             environment (first: {first}); its library does not honour FONTCONFIG_SYSROOT, so \
             the burn would render under the live configuration",
            escaped.len()
        ));
    }
    if loaded.is_empty() {
        return Err(
            "the producer's ffmpeg traced no Fontconfig configuration while burning text, so \
             nothing proves which configuration its libass renders under"
                .to_owned(),
        );
    }
    let live: BTreeSet<String> = live.iter().map(|path| path.display().to_string()).collect();
    if loaded == live {
        return Ok(());
    }
    Err(format!(
        "the producer's Fontconfig loads {} configuration files where the live one loads {} \
         ({} missing, {} different); the burn would render under other rules",
        loaded.len(),
        live.len(),
        live.difference(&loaded).count(),
        loaded.difference(&live).count(),
    ))
}

type Attested = (PathBuf, Vec<(PathBuf, String)>);

fn attest(
    dir: &Path,
    digest: &str,
    config: &Path,
    rules: &[(PathBuf, String)],
    fonts: &[(PathBuf, String)],
    layout: Layout,
) -> Result<Attested, String> {
    let version = crate::ffmpeg::engine_path_version;
    let mut objects = Vec::with_capacity(layout.entries.len() * 2);
    let mut parents = BTreeSet::new();
    for (entry, _) in &layout.entries {
        objects.push((entry.clone(), version(entry)?));
        if let Some(parent) = entry.parent() {
            parents.insert(parent.to_path_buf());
        }
    }
    // Taken after the cache and both parity listings, so whatever Fontconfig
    // itself leaves beside a copy or link is part of the captured version.
    for parent in parents {
        let current = version(&parent)?;
        objects.push((parent, current));
    }
    let expected: HashMap<&Path, &str> = rules
        .iter()
        .chain(fonts)
        .map(|(path, version)| (path.as_path(), version.as_str()))
        .collect();
    for (entry, original) in &layout.entries {
        // A link is statted through to its target; a copy has its own
        // identity and was checked against the original when it was read.
        if entry.is_symlink() && expected.get(original.as_path()) != Some(&version(entry)?.as_str())
        {
            return Err(format!(
                "{} changed while it was being frozen",
                original.display()
            ));
        }
    }
    let manifest = serde_json::json!({
        "font_digest": digest,
        "sysroot": layout.root,
        "config": config,
        "entries": layout
            .entries
            .iter()
            .map(|(entry, original)| serde_json::json!({
                "original": original,
                "version": expected.get(original.as_path()),
                "frozen": entry,
                "kind": if entry.is_symlink() { "font link" } else { "rules copy" },
            }))
            .collect::<Vec<_>>(),
    });
    let manifest_path = dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("writing {}: {error}", manifest_path.display()))?;
    Ok((layout.root, objects))
}

#[cfg(test)]
#[path = "fontenv_tests.rs"]
mod tests;
