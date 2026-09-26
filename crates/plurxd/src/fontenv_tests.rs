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
