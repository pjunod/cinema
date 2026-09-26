//! The frozen Fontconfig environment (FONT-ATTESTATION-AND-BLOCKING-IO.md
//! §5.2). Everything but the XML rewrite runs the real Fontconfig tools: the
//! property under test is what Fontconfig resolves, and a model of it would
//! pass whatever it was told.

use super::*;
use crate::ffmpeg::EncodedEngine;

/// Three installed TrueType fonts with different first families.
fn host_fonts() -> Vec<(PathBuf, String)> {
    let listing = fc("fc-list", &host_fc(), &["--format=%{file}\n"]);
    let mut files: Vec<PathBuf> = listing
        .lines()
        .map(PathBuf::from)
        .filter(|file| {
            file.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ttf"))
        })
        .collect();
    files.sort();
    files.dedup();
    let mut chosen: Vec<(PathBuf, String)> = Vec::new();
    for file in files {
        let family = fc(
            "fc-scan",
            &host_fc(),
            &[
                "--format=%{family[0]}\n",
                file.to_str().expect("UTF-8 font path"),
            ],
        )
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
        if !family.is_empty() && !chosen.iter().any(|(_, known)| *known == family) {
            chosen.push((file, family));
        }
        if chosen.len() == 3 {
            return chosen;
        }
    }
    panic!(
        "these tests need three installed TrueType fonts with different families \
         (fonts-dejavu-core provides them); found {chosen:?}"
    );
}

/// The Fontconfig variables a tool runs under; empty is the host's own
/// configuration.
type FcEnv = Vec<(&'static str, std::ffi::OsString)>;

fn host_fc() -> FcEnv {
    Vec::new()
}

fn live_fc(config: &Path) -> FcEnv {
    vec![("FONTCONFIG_FILE", config.into())]
}

/// Exactly what a producer for this recipe is given.
fn frozen_fc(engine: &EncodedEngine) -> FcEnv {
    engine
        .child_env()
        .into_iter()
        .map(|(name, value)| (name, value.to_owned()))
        .collect()
}

fn apply(command: &mut std::process::Command, env: &FcEnv) {
    command
        .env_remove("FONTCONFIG_FILE")
        .env_remove("FONTCONFIG_SYSROOT")
        .env_remove("FONTCONFIG_PATH");
    for (name, value) in env {
        command.env(name, value);
    }
}

fn fc(tool: &str, env: &FcEnv, args: &[&str]) -> String {
    let mut command = std::process::Command::new(tool);
    command.args(args);
    apply(&mut command, env);
    let output = command.output().unwrap_or_else(|error| {
        panic!(
            "these tests need Fontconfig's tools, as text burn does at runtime; \
             running {tool} failed: {error}"
        )
    });
    assert!(
        output.status.success(),
        "{tool} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 Fontconfig output")
}

fn listed_files(env: &FcEnv) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fc("fc-list", env, &["--format=%{file}\n"])
        .lines()
        .map(|file| std::fs::canonicalize(file).expect("listed font exists"))
        .collect();
    files.sort();
    files.dedup();
    files
}

fn matched_family(env: &FcEnv, pattern: &str) -> String {
    fc("fc-match", env, &["--format=%{family[0]}", pattern])
}

/// A stand-in for a host's configuration with both scopes the freeze must
/// resolve: a system `<dir>` and `conf.d`, and a user-scope include.
struct Fixture {
    base: tempfile::TempDir,
    runtime: PathBuf,
    fonts: PathBuf,
    config: PathBuf,
    rule: PathBuf,
    installed: Vec<PathBuf>,
    families: Vec<String>,
    spare: PathBuf,
}

fn probe_rule(family: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<fontconfig>\n  <alias binding=\"same\">\n    \
         <family>PlurxProbe</family>\n    <prefer><family>{family}</family></prefer>\n  \
         </alias>\n</fontconfig>\n"
    )
}

fn fixture() -> Fixture {
    let host = host_fonts();
    let base = crate::test_tempdir().expect("font fixture");
    let fonts = base.path().join("system-fonts");
    let conf_d = base.path().join("conf.d");
    std::fs::create_dir_all(&fonts).expect("font dir");
    std::fs::create_dir_all(&conf_d).expect("conf.d");
    let installed: Vec<PathBuf> = host[..2]
        .iter()
        .map(|(file, _)| {
            let copy = fonts.join(file.file_name().expect("font name"));
            std::fs::copy(file, &copy).expect("install font");
            copy
        })
        .collect();
    let rule = conf_d.join("50-probe.conf");
    // The second family, so the host's own default for an unknown name (its
    // first sans face) can never pass for the frozen answer.
    std::fs::write(&rule, probe_rule(&host[1].1)).expect("probe rule");
    let user = base.path().join("user.conf");
    std::fs::write(
        &user,
        "<?xml version=\"1.0\"?>\n<fontconfig>\n  <dir>/nonexistent/user-fonts</dir>\n  \
         <match target=\"font\"><edit name=\"embolden\" mode=\"assign\"><bool>false</bool></edit></match>\n\
         </fontconfig>\n",
    )
    .expect("user rule");
    let config = base.path().join("fonts.conf");
    std::fs::write(
        &config,
        format!(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n\
             <fontconfig>\n  <dir>{}</dir>\n  <cachedir>{}</cachedir>\n  \
             <include ignore_missing=\"yes\">{}</include>\n  \
             <include ignore_missing=\"yes\">{}</include>\n</fontconfig>\n",
            fonts.display(),
            base.path().join("system-cache").display(),
            conf_d.display(),
            user.display()
        ),
    )
    .expect("system config");
    Fixture {
        runtime: base.path().join("runtime"),
        fonts,
        config,
        rule,
        installed,
        families: host.iter().map(|(_, family)| family.clone()).collect(),
        spare: host[2].0.clone(),
        base,
    }
}

impl Fixture {
    /// Install the third font into the "system" directory and refresh its
    /// cache, the way a package install would.
    fn install_spare(&self) {
        std::fs::copy(
            &self.spare,
            self.fonts.join(self.spare.file_name().expect("font name")),
        )
        .expect("install spare font");
        fc("fc-cache", &live_fc(&self.config), &[]);
    }

    fn captured(&self) -> Vec<PathBuf> {
        let mut captured: Vec<PathBuf> = self
            .installed
            .iter()
            .map(|file| std::fs::canonicalize(file).expect("installed font"))
            .collect();
        captured.sort();
        captured
    }
}

async fn capture(fixture: &Fixture) -> EncodedEngine {
    plurx_core::testfixtures::require_ffmpeg();
    EncodedEngine::capture_from(Some(&fixture.runtime), Some(&fixture.config))
        .await
        .expect("a text burn freezes its Fontconfig environment")
}

fn environment(engine: &EncodedEngine) -> &Arc<FontEnvironment> {
    engine
        .font_environment()
        .expect("a text-burn recipe has a frozen environment")
}

/// Where the frozen environment keeps its copy of a live file.
fn frozen_copy(engine: &EncodedEngine, original: &Path) -> PathBuf {
    environment(engine)
        .root()
        .join(original.strip_prefix("/").expect("absolute path"))
}

#[test]
fn a_frozen_environment_that_loads_other_rules_is_refused() {
    let root = Path::new("/cache/fontenv/abc-1/root");
    let live = [
        PathBuf::from("/etc/fonts/conf.d/10-a.conf"),
        PathBuf::from("/etc/fonts/fonts.conf"),
    ];
    let same = "+ /cache/fontenv/abc-1/root/etc/fonts/conf.d/10-a.conf: A\n\
                + /cache/fontenv/abc-1/root/etc/fonts/fonts.conf: Default\n\
                - /usr/share/fontconfig/conf.avail/70-x.conf: not loaded\n";
    rules_match(root, &live, same).expect("the same files in the same order");
    let reordered = "+ /cache/fontenv/abc-1/root/etc/fonts/fonts.conf: Default\n\
                     + /cache/fontenv/abc-1/root/etc/fonts/conf.d/10-a.conf: A\n";
    let error = rules_match(root, &live, reordered).expect_err("another order is other rules");
    assert!(error.contains("first difference at position 0"), "{error}");
    let fallback = "+ /cache/fontenv/abc-1/root/etc/fonts/fonts.conf: Default\n";
    assert!(rules_match(root, &live, fallback).is_err());
}

#[test]
fn a_frozen_environment_that_resolves_other_faces_is_refused() {
    let root = Path::new("/cache/fontenv/abc-1/root");
    let live = "/usr/share/fonts/a.ttf\t0\tA\tBook\n/usr/share/fonts/b.ttf\t0\tB\tBook\n";
    // Font files come back with the sysroot stripped, but a prefixed answer
    // reads the same.
    faces_match(
        root,
        live,
        "/cache/fontenv/abc-1/root/usr/share/fonts/b.ttf\t0\tB\tBook\n\
         /usr/share/fonts/a.ttf\t0\tA\tBook\n",
    )
    .expect("the same faces");
    let error = faces_match(root, live, "/usr/share/fonts/a.ttf\t0\tA\tBook\n")
        .expect_err("a missing face");
    assert!(
        error.contains("resolves 1 faces where the live one resolves 2 (1 missing, 0 different)"),
        "{error}"
    );
    let error = faces_match(
        root,
        live,
        "/usr/share/fonts/a.ttf\t0\tA\tBold\n/usr/share/fonts/b.ttf\t0\tB\tBook\n",
    )
    .expect_err("a restyled face");
    assert!(error.contains("(1 missing, 1 different)"), "{error}");
}

#[tokio::test]
async fn a_frozen_environment_names_only_the_captured_fonts() {
    let fixture = fixture();
    let engine = capture(&fixture).await;
    fixture.install_spare();
    assert_eq!(
        listed_files(&live_fc(&fixture.config)).len(),
        3,
        "the live configuration sees the newly installed font"
    );

    // Through the production spawn builder, with exactly the environment a
    // producer for this recipe gets.
    let env = engine.child_env();
    assert_eq!(
        env,
        vec![
            (
                "FONTCONFIG_SYSROOT",
                environment(&engine).root().as_os_str()
            ),
            ("FONTCONFIG_FILE", fixture.config.as_os_str()),
        ]
    );
    let mut spawned = crate::producer_spawn::spawn(
        Path::new("fc-list"),
        &["--format=%{file}\n".to_owned()],
        crate::producer_spawn::SpawnOptions {
            runtime_cache: &fixture.runtime,
            progress: crate::producer_spawn::Progress::None,
            descriptors: crate::producer_spawn::Descriptors::default(),
            env: &env,
            work: crate::process_control::ChildWork::background("frozen font test"),
        },
    )
    .expect("spawn fc-list as a producer");
    let mut listing = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut spawned.stdout, &mut listing)
        .await
        .expect("child listing");
    assert!(spawned.child.wait().await.expect("child exit").success());
    let mut seen: Vec<PathBuf> = listing
        .lines()
        .map(|file| std::fs::canonicalize(file).expect("listed font"))
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen, fixture.captured());

    let copy_only = EncodedEngine::capture_test_objects(&[], "p")
        .await
        .expect("non-burn engine");
    assert!(
        copy_only.child_env().is_empty(),
        "a recipe that burns no text leaves the child's Fontconfig alone"
    );
}

#[tokio::test]
async fn a_new_system_font_does_not_withdraw_a_frozen_recipe() {
    let fixture = fixture();
    let engine = capture(&fixture).await;
    fixture.install_spare();
    let (current, charged) = engine.is_current_charged_for_test().await;
    assert!(current, "an addition the child cannot see changes nothing");
    assert_eq!(
        charged,
        vec![("font", "stat", 1), ("media", "stat", 1)],
        "a frozen check stats its environment once and spawns no Fontconfig tool"
    );
}

#[tokio::test]
async fn a_replaced_font_file_still_withdraws_the_recipe() {
    let fixture = fixture();
    let engine = capture(&fixture).await;
    assert!(engine.is_current().await);
    let mut font = std::fs::OpenOptions::new()
        .append(true)
        .open(&fixture.installed[0])
        .expect("open captured font");
    std::io::Write::write_all(&mut font, b"\0").expect("replace font bytes");
    drop(font);
    assert!(!engine.is_current().await);
}

#[tokio::test]
async fn an_edited_config_copy_withdraws_the_recipe() {
    let fixture = fixture();
    let engine = capture(&fixture).await;
    let copy = frozen_copy(&engine, &fixture.rule);
    let mut rules = std::fs::OpenOptions::new()
        .append(true)
        .open(&copy)
        .expect("open rules copy");
    std::io::Write::write_all(&mut rules, b"\n").expect("edit rules copy");
    drop(rules);
    assert!(!engine.is_current().await);
}

/// A missing rule file changes what Fontconfig loads (for a required include
/// it falls back to a default configuration with no rules at all), so the
/// stat fence refuses it before the producer launches.
#[tokio::test]
async fn a_deleted_config_copy_withdraws_the_recipe() {
    let fixture = fixture();
    let engine = capture(&fixture).await;
    std::fs::remove_file(frozen_copy(&engine, &fixture.rule)).expect("delete rules copy");
    assert!(!engine.is_current().await);
}

#[tokio::test]
async fn an_edited_system_config_does_not_reach_a_frozen_recipe() {
    let fixture = fixture();
    let engine = capture(&fixture).await;
    let frozen = frozen_fc(&engine);
    assert_eq!(matched_family(&frozen, "PlurxProbe"), fixture.families[1]);

    std::fs::write(&fixture.rule, probe_rule(&fixture.families[0])).expect("edit system rule");
    assert_eq!(
        matched_family(&live_fc(&fixture.config), "PlurxProbe"),
        fixture.families[0],
        "the live configuration follows the edit"
    );
    assert_eq!(
        matched_family(&frozen, "PlurxProbe"),
        fixture.families[1],
        "the frozen configuration reads its own copy"
    );
    assert!(engine.is_current().await);
}

#[tokio::test]
async fn recipes_frozen_from_one_closure_share_it_until_the_last_is_released() {
    let fixture = fixture();
    let first = capture(&fixture).await;
    let second = capture(&fixture).await;
    assert!(Arc::ptr_eq(environment(&first), environment(&second)));
    let dir = environment(&first).dir().to_path_buf();
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(dir.exists(), "a recipe still names the environment");
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(10);
    while dir.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!dir.exists(), "the last release removes the environment");
    let _ = &fixture.base;
}

#[tokio::test]
async fn a_process_clears_environments_its_predecessor_left() {
    let fixture = fixture();
    let stale = fixture
        .runtime
        .join(FONT_ENVIRONMENT_DIR)
        .join("0123456789abcdef-1");
    std::fs::create_dir_all(&stale).expect("stale environment");
    std::fs::write(stale.join("fonts.conf"), b"<fontconfig/>").expect("stale config");
    let engine = capture(&fixture).await;
    assert!(!stale.exists(), "a predecessor's environment is swept");
    assert!(environment(&engine).dir().exists());
}

/// §7.5's safety proof on the host's real configuration: the frozen
/// environment built from it matches every probe pattern to the same face
/// with the same rendering properties, and libass draws the same pixels.
#[tokio::test]
async fn a_frozen_host_environment_matches_and_renders_like_the_live_one() {
    plurx_core::testfixtures::require_ffmpeg();
    let base = crate::test_tempdir().expect("runtime cache");
    let engine = EncodedEngine::capture(Some(base.path()))
        .await
        .expect("the host configuration freezes");
    let frozen = frozen_fc(&engine);
    let format = "--format=%{family[0]}|%{style[0]}|%{index}|%{hintstyle}|%{hinting}|\
                  %{autohint}|%{antialias}|%{rgba}|%{lcdfilter}|%{embolden}|%{file}";
    let answer = |env: &FcEnv, pattern: &str| {
        let line = fc("fc-match", env, &[format, pattern]);
        let (properties, file) = line.rsplit_once('|').expect("formatted match");
        format!(
            "{properties}|{}",
            std::fs::canonicalize(file).expect("matched font").display()
        )
    };
    for pattern in [
        "sans-serif",
        "serif",
        "monospace",
        "mono",
        "sans",
        "Arial",
        "Helvetica",
        "Times New Roman",
        "Courier New",
        "Verdana",
        "DejaVu Sans:bold",
        "DejaVu Serif:italic",
        "system-ui",
        "emoji",
        ":lang=ja",
        ":lang=ar",
        "sans-serif:pixelsize=8",
        "monospace:pixelsize=10",
    ] {
        assert_eq!(
            answer(&frozen, pattern),
            answer(&host_fc(), pattern),
            "fc-match {pattern}"
        );
    }

    let script = base.path().join("glyphs.ass");
    let style = |name: &str, font: &str, alignment: u8| {
        format!(
            "Style: {name},{font},28,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,\
             100,100,0,0,1,1,0,{alignment},10,10,10,1\n"
        )
    };
    std::fs::write(
        &script,
        format!(
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 480\nPlayResY: 270\n\n\
             [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, \
             OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, \
             Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, \
             MarginV, Encoding\n{}{}{}\n[Events]\nFormat: Layer, Start, End, Style, Name, \
             MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:00.00,0:00:05.00,Sans,,0,0,0,,Sans glyphs Qg 0123\n\
             Dialogue: 0,0:00:00.00,0:00:05.00,Serif,,0,0,0,,Serif glyphs Qg 0123\n\
             Dialogue: 0,0:00:00.00,0:00:05.00,Mono,,0,0,0,,Mono glyphs Qg 0123\n",
            style("Sans", "Arial", 7),
            style("Serif", "Times New Roman", 4),
            style("Mono", "monospace", 1),
        ),
    )
    .expect("subtitle fixture");
    let render = |env: &FcEnv, burn: bool| {
        let mut command = std::process::Command::new(crate::ffmpeg::ffmpeg_bin());
        command.args([
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=480x270:d=1",
        ]);
        if burn {
            command.args(["-vf", &format!("subtitles={}", script.display())]);
        }
        command.args(["-frames:v", "1", "-f", "framemd5", "-"]);
        apply(&mut command, env);
        let output = command.output().expect("ffmpeg");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("framemd5")
            .lines()
            .rfind(|line| !line.starts_with('#'))
            .expect("one frame")
            .rsplit(',')
            .next()
            .expect("frame hash")
            .trim()
            .to_owned()
    };
    let live = render(&host_fc(), true);
    assert_ne!(live, render(&host_fc(), false), "the burn drew glyphs");
    assert_eq!(render(&frozen, true), live);
}

/// Test-only seams in `build`, keyed by the runtime cache a capture freezes
/// under so parallel tests never see each other's.
pub(crate) mod hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::sync::Notify;

    #[derive(Clone, Default)]
    pub(crate) struct Hooks {
        /// `(reached, resume)`: a capture signals `reached` once its
        /// environment is on disk, then waits for `resume`.
        pub(crate) pause: Option<(Arc<Notify>, Arc<Notify>)>,
        /// Run in place of the encoder for the producer-library proof.
        pub(crate) producer_program: Option<PathBuf>,
    }

    fn table() -> &'static Mutex<HashMap<PathBuf, Hooks>> {
        static TABLE: OnceLock<Mutex<HashMap<PathBuf, Hooks>>> = OnceLock::new();
        TABLE.get_or_init(Default::default)
    }

    fn get(runtime_cache: &Path) -> Hooks {
        table()
            .lock()
            .expect("hook table")
            .get(runtime_cache)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn install(runtime_cache: &Path, hooks: Hooks) {
        table()
            .lock()
            .expect("hook table")
            .insert(runtime_cache.to_path_buf(), hooks);
    }

    pub(crate) async fn paused(runtime_cache: &Path) {
        if let Some((reached, resume)) = get(runtime_cache).pause {
            reached.notify_one();
            resume.notified().await;
        }
    }

    pub(crate) fn producer_program(runtime_cache: &Path) -> Option<PathBuf> {
        get(runtime_cache).producer_program
    }
}

/// Every environment directory a capture left under the fixture's runtime
/// cache, waited for briefly because removal runs on the blocking pool.
async fn environments_left(fixture: &Fixture) -> Vec<PathBuf> {
    let root = fixture.runtime.join(FONT_ENVIRONMENT_DIR);
    let list = || -> Vec<PathBuf> {
        std::fs::read_dir(&root)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok().map(|e| e.path()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !list().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    list()
}

async fn refused(fixture: &Fixture) -> String {
    plurx_core::testfixtures::require_ffmpeg();
    EncodedEngine::capture_from(Some(&fixture.runtime), Some(&fixture.config))
        .await
        .expect_err("the capture refuses the burn")
}

/// Finding 1 of PR #553's review: the rule parity must refuse through
/// `build`, not only as a function. A required `<include>` of an empty
/// directory loads live, but `materialise` copies only loaded *files*, so the
/// directory does not exist under the sysroot and the frozen load falls back
/// to Fontconfig's rule-less default (exit 0, `+ memory`).
#[tokio::test]
async fn a_capture_whose_frozen_rules_diverge_is_refused() {
    let fixture = fixture();
    let empty = fixture.base.path().join("empty-conf.d");
    std::fs::create_dir_all(&empty).expect("empty include");
    let config = std::fs::read_to_string(&fixture.config).expect("fixture config");
    std::fs::write(
        &fixture.config,
        config.replace(
            "</fontconfig>\n",
            &format!("  <include>{}</include>\n</fontconfig>\n", empty.display()),
        ),
    )
    .expect("required include");
    let error = refused(&fixture).await;
    assert!(
        error.contains("the frozen Fontconfig environment loads"),
        "refused by the rule parity: {error}"
    );
    assert_eq!(environments_left(&fixture).await, Vec::<PathBuf>::new());
}

/// Finding 1, the face half. Fontconfig caches a directory's scan keyed on
/// the directory alone, so a live cache written under a scan-time rule that
/// no longer exists still names the renamed family; the frozen environment
/// has its own cache, rescans, and resolves the file's real family. The
/// rules are the same files, so only the face parity can refuse it.
#[tokio::test]
async fn a_capture_whose_frozen_faces_diverge_is_refused() {
    let fixture = fixture();
    let config = std::fs::read_to_string(&fixture.config).expect("fixture config");
    let renaming = fixture.base.path().join("renaming.conf");
    std::fs::write(
        &renaming,
        config.replace(
            "</fontconfig>\n",
            "  <match target=\"scan\"><edit name=\"family\" mode=\"assign\" binding=\"strong\">\
             <string>PlurxRenamed</string></edit></match>\n</fontconfig>\n",
        ),
    )
    .expect("renaming config");
    fc("fc-cache", &live_fc(&renaming), &[]);
    assert!(
        fc(
            "fc-list",
            &live_fc(&fixture.config),
            &["--format=%{family}\n"]
        )
        .contains("PlurxRenamed"),
        "the live listing is served from the stale cache"
    );
    let error = refused(&fixture).await;
    assert!(
        error.contains("the frozen Fontconfig environment resolves"),
        "refused by the face parity: {error}"
    );
    assert_eq!(environments_left(&fixture).await, Vec::<PathBuf>::new());
}

/// Install a stand-in encoder for the producer-library proof: `prelude` (a
/// shell fragment) runs, then the host's real encoder with the same arguments.
fn stand_in(fixture: &Fixture, name: &str, prelude: &str) {
    let program = fixture.base.path().join(name);
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n{prelude}\nexec '{}' \"$@\"\n",
            crate::ffmpeg::encoder_executable_path()
                .expect("an encoder on this host")
                .display()
        ),
    )
    .expect("stand-in encoder");
    #[cfg(unix)]
    std::fs::set_permissions(
        &program,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("executable stand-in");
    hooks::install(
        &fixture.runtime,
        hooks::Hooks {
            producer_program: Some(program),
            ..Default::default()
        },
    );
}

/// Finding 3: the system `fc-*` tools prove nothing about the libfontconfig
/// the producer's ffmpeg links. A producer whose library ignores
/// `FONTCONFIG_SYSROOT` (older than 2.13.1) reads the live root
/// configuration by its original path, and the capture must refuse it. Such
/// a library cannot read the cache a 2.13+ `fc-list` writes (an older cache
/// version), so it scans cold; the stand-in drops the live cache to be that
/// library rather than one that happens to find a warm cache and stay silent.
#[tokio::test]
async fn a_producer_library_that_ignores_the_sysroot_is_refused() {
    let fixture = fixture();
    stand_in(
        &fixture,
        "ffmpeg-without-sysroot",
        &format!(
            "unset FONTCONFIG_SYSROOT\nrm -rf '{}'",
            fixture.base.path().join("system-cache").display()
        ),
    );
    let error = refused(&fixture).await;
    assert!(
        error.contains("does not honour FONTCONFIG_SYSROOT"),
        "refused by the producer-library proof: {error}"
    );
    assert_eq!(environments_left(&fixture).await, Vec::<PathBuf>::new());
}

/// PR #553's fast lane: Fontconfig before 2.17 traces its configuration only
/// on a cold load, and the lane's `fc-list` had warmed the environment's
/// cache before the producer probe ran, so a 2.15 producer printed nothing.
/// The probe must run on the new root before any cache exists in it, on
/// every host. The stand-in refuses to render if Fontconfig has written a
/// cache anywhere under the sysroot; the capture then succeeds only if the
/// probe went first, and the probe's scan leaves the cache later launches
/// read.
#[tokio::test]
async fn the_producer_probe_is_a_cold_load() {
    let fixture = fixture();
    stand_in(
        &fixture,
        "ffmpeg-requiring-a-cold-cache",
        "if [ -n \"$(find \"$FONTCONFIG_SYSROOT\" -name '*.cache-*' -print)\" ]; then\n  \
         echo 'a Fontconfig cache already exists under the sysroot' >&2\n  exit 3\nfi",
    );
    let engine = capture(&fixture).await;
    let caches: Vec<PathBuf> = walk(environment(&engine).root())
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".cache-"))
        })
        .collect();
    assert!(
        !caches.is_empty(),
        "the probe's scan left a cache in the environment for later launches"
    );
}

/// A producer that traces nothing even on a cold load proves nothing about
/// which configuration it renders under: the capture refuses the burn, it
/// never skips the proof.
#[tokio::test]
async fn a_producer_that_traces_nothing_is_refused() {
    let fixture = fixture();
    stand_in(&fixture, "ffmpeg-without-trace", "unset FC_DEBUG");
    let error = refused(&fixture).await;
    assert!(
        error.contains("traced no Fontconfig configuration"),
        "refused by the producer-library proof: {error}"
    );
    assert_eq!(environments_left(&fixture).await, Vec::<PathBuf>::new());
}

/// Every file under `dir`, not following links.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("readable environment") {
            let entry = entry.expect("directory entry");
            let kind = entry.file_type().expect("entry type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                found.push(entry.path());
            }
        }
    }
    found
}

/// The trace as the production image's bundled Fontconfig prints it
/// (the production `plurxd:latest` image, 2026-09-26): start order, a
/// doubled slash after the sysroot for `conf.d` entries, a `done` line per
/// file, and the whole configuration loaded more than once.
#[test]
fn the_producer_library_trace_is_read_as_the_image_prints_it() {
    let root = Path::new("/cache/fontenv/abc-1/root");
    let live = [
        PathBuf::from("/etc/fonts/conf.d/10-a.conf"),
        PathBuf::from("/etc/fonts/fonts.conf"),
    ];
    let once = "\tLoading config file from /cache/fontenv/abc-1/root/etc/fonts/fonts.conf\n\
                \tScanning config dir /cache/fontenv/abc-1/root//etc/fonts/conf.d\n\
                \tLoading config file from /cache/fontenv/abc-1/root//etc/fonts/conf.d/10-a.conf\n\
                \tLoading config file from /cache/fontenv/abc-1/root//etc/fonts/conf.d/10-a.conf done\n\
                \tLoading config file from /cache/fontenv/abc-1/root/etc/fonts/fonts.conf done\n";
    let trace = format!("FC_DEBUG=1024\n{once}{once}");
    library_loads_match(root, &live, &trace).expect("the frozen files, twice");

    let escaped = trace.replace(
        "/cache/fontenv/abc-1/root//etc/fonts/conf.d/10-a.conf",
        "/etc/fonts/conf.d/10-a.conf",
    );
    let error = library_loads_match(root, &live, &escaped).expect_err("a live file was read");
    assert!(
        error.contains("1 configuration paths outside the frozen environment"),
        "{error}"
    );
    let scanned = format!("{trace}\tScanning config dir /usr/share/fontconfig/conf.avail\n");
    assert!(library_loads_match(root, &live, &scanned).is_err());

    let error = library_loads_match(root, &live, "FC_DEBUG=1024\n").expect_err("no trace");
    assert!(
        error.contains("traced no Fontconfig configuration"),
        "{error}"
    );

    let partial = "\tLoading config file from /cache/fontenv/abc-1/root/etc/fonts/fonts.conf\n";
    let error = library_loads_match(root, &live, partial).expect_err("a file not loaded");
    assert!(error.contains("(1 missing, 0 different)"), "{error}");
}

/// Finding 2: a capture its caller abandons — `vod_resurrect_before`'s
/// `timeout_at` dropping `prepare_vod_encoding` after `materialise` — must
/// not leave its directory for the next process. Paused with the
/// environment on disk and before any `fc-*` child, then aborted.
#[tokio::test]
async fn a_cancelled_capture_leaves_no_environment_behind() {
    let fixture = fixture();
    let (reached, resume) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    hooks::install(
        &fixture.runtime,
        hooks::Hooks {
            pause: Some((Arc::clone(&reached), Arc::clone(&resume))),
            ..Default::default()
        },
    );
    plurx_core::testfixtures::require_ffmpeg();
    let (runtime, config) = (fixture.runtime.clone(), fixture.config.clone());
    let capture =
        tokio::spawn(
            async move { EncodedEngine::capture_from(Some(&runtime), Some(&config)).await },
        );
    tokio::time::timeout(Duration::from_secs(60), reached.notified())
        .await
        .expect("the capture materialised its environment");
    let built = std::fs::read_dir(fixture.runtime.join(FONT_ENVIRONMENT_DIR))
        .expect("fontenv")
        .count();
    assert_eq!(
        built, 1,
        "the environment is on disk when the caller gives up"
    );
    capture.abort();
    assert!(capture.await.expect_err("aborted").is_cancelled());
    assert_eq!(
        environments_left(&fixture).await,
        Vec::<PathBuf>::new(),
        "a cancelled capture removes what it built"
    );
    drop(resume);
}
