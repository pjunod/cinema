//! Discover and validate daemon-owned Dolby Vision helper bundles.

use plurx_core::transcode::dv_processing::{DvBackendIdentity, DvDigest};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    graph: String,
    abi: BTreeMap<String, u32>,
    tools: BTreeMap<String, Artifact>,
    libraries: Vec<Artifact>,
    parser_sha256: String,
    source_sha256: String,
    environment: BTreeMap<String, String>,
}
#[derive(Clone)]
pub(crate) struct ToolSet {
    pub(crate) renderer: PathBuf,
    pub(crate) scanner: PathBuf,
    pub(crate) renderer_env: Vec<(String, std::ffi::OsString)>,
    pub(crate) identity: DvBackendIdentity,
    /// The scanner executable's own digest. It is statically linked, so this
    /// is everything that decides a complete-title aggregate.
    scanner_sha256: String,
    /// The libav majors the helper decodes with (`avcodec=…,avformat=…,avutil=…`):
    /// with [`REFIT_MODE_VERSION`], what an F+ fit's statistics depend on.
    decoder_abi: String,
    artifacts: Arc<Vec<(PathBuf, String)>>,
}

/// The refit mode's own version: `segment_decode_render --refit-stats`, its
/// pooling, statistics and resume rules (`refit_mode.h`) and the composition
/// it measures (`refit_compose.h`). Bump it whenever either changes what a
/// run prints; [`REFIT_MODE_SOURCES`] makes a change to them fail a test
/// until it is bumped and re-pinned.
pub(crate) const REFIT_MODE_VERSION: &str = "plurx-refit-mode-v2";
/// sha256 of `refit_mode.h` followed by `refit_compose.h`, as
/// [`REFIT_MODE_VERSION`] was last reviewed against. The diagnostic-only
/// TopLeft export addition preserves the v2 statistics, samples and pooling;
/// its independent representation has its own version in `refit_topleft.h`.
#[cfg(test)]
pub(crate) const REFIT_MODE_SOURCES: &str =
    "de639eb4bc1d59caaa0c7b05db667f6d7ff6862ac84f2af03ba52592cd6a2019";

#[cfg(test)]
impl ToolSet {
    /// An explicitly controlled scanner result with the ordinary executable
    /// artifact-current check, used only by admitted lifecycle fixtures.
    pub(crate) fn lifecycle_fixture_with_scanner(
        identity: DvBackendIdentity,
        scanner: PathBuf,
    ) -> Result<Self, String> {
        let digest = hash_file(&scanner)?;
        let mut tools = Self::lifecycle_fixture(identity);
        tools.scanner = scanner.clone();
        tools.scanner_sha256 = digest.clone();
        tools.artifacts = Arc::new(vec![(scanner, digest)]);
        Ok(tools)
    }

    /// Lifecycle fixtures never launch this renderer: they hold the exclusive
    /// worker claim while exercising attachment of a checked completed plan.
    pub(crate) fn lifecycle_fixture(identity: DvBackendIdentity) -> Self {
        Self {
            renderer: PathBuf::from("/bin/false"),
            scanner: PathBuf::from("/bin/false"),
            renderer_env: Vec::new(),
            identity,
            scanner_sha256: hash(b"/bin/false"),
            decoder_abi: "avcodec=61,avformat=61,avutil=59".into(),
            artifacts: Arc::new(Vec::new()),
        }
    }

    /// A fixture whose renderer is `renderer` (a script standing in for
    /// `segment_decode_render --refit-stats`), current while it is unchanged.
    pub(crate) fn refit_fixture(renderer: PathBuf) -> Result<Self, String> {
        let identity = DvBackendIdentity::new(
            digest(b"refit renderer")?,
            digest(b"libraries")?,
            digest(b"parser")?,
            digest(b"sources")?,
            1,
            digest(b"manifest")?,
        )
        .map_err(|e| e.to_string())?;
        let digest = hash_file(&renderer)?;
        let mut tools = Self::lifecycle_fixture(identity);
        tools.renderer = renderer.clone();
        tools.artifacts = Arc::new(vec![(renderer, digest)]);
        Ok(tools)
    }
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn renderer_source_identity() -> String {
    let inputs: &[(&str, &[u8])] = &[
        (
            "author_p81.c",
            include_bytes!("../../../tools/dv_processing/author_p81.c"),
        ),
        (
            "base_dv_renderer.h",
            include_bytes!("../../../tools/dv_processing/base_dv_renderer.h"),
        ),
        (
            "display_target.h",
            include_bytes!("../../../tools/dv_processing/display_target.h"),
        ),
        (
            "dm_closed_form.h",
            include_bytes!("../../../tools/dv_processing/dm_closed_form.h"),
        ),
        (
            "dm_cm29_gamut_tables.h",
            include_bytes!("../../../tools/dv_processing/dm_cm29_gamut_tables.h"),
        ),
        (
            "dv_dm.h",
            include_bytes!("../../../tools/dv_processing/dv_dm.h"),
        ),
        (
            "dv_metadata.h",
            include_bytes!("../../../tools/dv_processing/dv_metadata.h"),
        ),
        (
            "dv_trace.h",
            include_bytes!("../../../tools/dv_processing/dv_trace.h"),
        ),
        (
            "fel_renderer.h",
            include_bytes!("../../../tools/dv_processing/fel_renderer.h"),
        ),
        (
            "hevc-rasl-frame-thread.patch",
            include_bytes!("../../../tools/dv_processing/hevc-rasl-frame-thread.patch"),
        ),
        (
            "movie_source_io.h",
            include_bytes!("../../../tools/dv_processing/movie_source_io.h"),
        ),
        (
            "mux_rgb.c",
            include_bytes!("../../../tools/dv_processing/mux_rgb.c"),
        ),
        (
            "nlq_clipping.h",
            include_bytes!("../../../tools/dv_processing/nlq_clipping.h"),
        ),
        (
            "nut_timing.h",
            include_bytes!("../../../tools/dv_processing/nut_timing.h"),
        ),
        (
            "refit_compose.h",
            include_bytes!("../../../tools/dv_processing/refit_compose.h"),
        ),
        (
            "refit_mode.h",
            include_bytes!("../../../tools/dv_processing/refit_mode.h"),
        ),
        (
            "refit_topleft.h",
            include_bytes!("../../../tools/dv_processing/refit_topleft.h"),
        ),
        (
            "rgb48.h",
            include_bytes!("../../../tools/dv_processing/rgb48.h"),
        ),
        (
            "scan_title.c",
            include_bytes!("../../../tools/dv_processing/scan_title.c"),
        ),
        (
            "segment_decode_render.c",
            include_bytes!("../../../tools/dv_processing/segment_decode_render.c"),
        ),
        (
            "source_reader.h",
            include_bytes!("../../../tools/dv_processing/source_reader.h"),
        ),
        (
            "title_constants.h",
            include_bytes!("../../../tools/dv_processing/title_constants.h"),
        ),
        (
            "yuv420p10.h",
            include_bytes!("../../../tools/dv_processing/yuv420p10.h"),
        ),
    ];
    let mut identity = Sha256::new();
    for (name, bytes) in inputs {
        identity.update(name.as_bytes());
        identity.update([0]);
        identity.update(Sha256::digest(bytes));
    }
    hex::encode(identity.finalize())
}
/// The installed bundle's identity as this daemon accepts it, or why it does
/// not. One validation path: the same capture the worker performs.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) async fn installed_helper_identity() -> Result<String, String> {
    let tools = ToolSet::capture()
        .await?
        .ok_or_else(|| "no DV helper bundle is installed".to_string())?;
    Ok(serde_json::json!({
        "source_sha256": renderer_source_identity(),
        "backend_digest": tools.backend_digest(),
    })
    .to_string())
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}
pub(crate) fn digest(bytes: &[u8]) -> Result<DvDigest, String> {
    DvDigest::new(hash(bytes)).map_err(|e| e.to_string())
}

fn contained(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("DV artifact path is not relative to its owned root".into());
    }
    let resolved = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err("DV artifact escapes its owned root".into());
    }
    Ok(resolved)
}
impl ToolSet {
    /// Only parents of the canonical executables accepted by bundle validation.
    /// Never infer trust from the configured bundle root before capture succeeds.
    pub(crate) fn executable_directories(&self) -> Vec<PathBuf> {
        let mut directories: Vec<_> = [&self.renderer, &self.scanner]
            .into_iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect();
        directories.sort();
        directories.dedup();
        directories
    }

    /// The whole processing backend: renderer, libraries, parser and helper
    /// sources. FEL verdicts are measurements of that renderer, so they are
    /// keyed by it.
    #[cfg(unix)]
    pub(crate) fn backend_digest(&self) -> String {
        hash(&serde_json::to_vec(&self.identity).expect("validated backend identity serializes"))
    }
    /// What a complete-title aggregate depends on: the scanner executable and
    /// nothing else. Binding title demand to the whole backend re-queued every
    /// Dolby Vision title whenever the renderer, a shader or libplacebo changed.
    #[cfg(unix)]
    pub(crate) fn title_scanner_digest(&self) -> String {
        hash(format!("plurx-dv-title-scanner-v1\0{}", self.scanner_sha256).as_bytes())
    }
    /// What an F+ fit's statistics depend on (decision G): the refit mode's
    /// version and the decoder ABI it runs on, not the whole backend digest,
    /// so rebuilding the bundle for an unrelated change does not decode every
    /// FEL title again. The fit method is named separately
    /// (`dv_refit::FIT_VERSION`).
    pub(crate) fn refit_helper_identity(&self) -> String {
        hash(
            format!(
                "plurx-dv-refit-identity-v1\0{REFIT_MODE_VERSION}\0{}",
                self.decoder_abi
            )
            .as_bytes(),
        )
    }
    /// The decoder ABI, as the Developer card reports it.
    pub(crate) fn decoder_abi(&self) -> &str {
        &self.decoder_abi
    }
    /// Configuration is captured by the worker, never supplied by a playback
    /// request. Absence preserves ordinary output and the saved preference.
    pub(crate) async fn capture() -> Result<Option<Arc<Self>>, String> {
        let root = std::env::var_os("PLURX_DV_PROCESSING_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/lib/plurx/dv-processing".into());
        Self::capture_at(root).await
    }

    // Keep the manifest-validation boundary identical for production discovery
    // and fixture bundles without mutating process-global configuration.
    async fn capture_at(root: PathBuf) -> Result<Option<Arc<Self>>, String> {
        crate::subtitle_models::admitted_work::spawn_blocking(move || {
            let manifest_path = root.join("build-identity.json");
            let bytes = match std::fs::read(&manifest_path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            if bytes.len() > 64 * 1024 {
                return Err("DV build identity exceeds its bound".into());
            }
            let root = root.canonicalize().map_err(|e| e.to_string())?;
            let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if manifest.schema != 1
                || manifest.source_sha256 != renderer_source_identity()
                || manifest.graph != "dv-native-master-or-closed-form-dm-v1"
                || manifest.abi.get("libplacebo") != Some(&387)
                || !["avcodec", "avformat", "avutil"]
                    .iter()
                    .all(|key| manifest.abi.get(*key).is_some_and(|major| *major > 0))
                || !manifest.tools.contains_key("renderer")
                || !manifest.tools.contains_key("scanner")
                || manifest.tools.len() > 4
                || manifest.libraries.is_empty()
                || manifest.libraries.len() > 32
            {
                return Err("unsupported DV build identity".into());
            }
            let mut artifacts = Vec::new();
            let mut tools = BTreeMap::new();
            for (name, artifact) in &manifest.tools {
                if !matches!(name.as_str(), "renderer" | "muxer" | "author" | "scanner") {
                    return Err("unknown DV helper".into());
                }
                // Optional standalone tools do not participate in the actual
                // streaming renderer + daemon packet-authoring graph.
                if !matches!(name.as_str(), "renderer" | "scanner") {
                    continue;
                }
                let path = contained(&root, &artifact.path)?;
                let actual = hash_file(&path)?;
                if actual != artifact.sha256 {
                    return Err("DV helper differs from its build identity".into());
                }
                tools.insert(name.clone(), path.clone());
                artifacts.push((path, actual));
            }
            let mut library_identity = Vec::new();
            for artifact in &manifest.libraries {
                let path = contained(&root, &artifact.path)?;
                let actual = hash_file(&path)?;
                if actual != artifact.sha256 {
                    return Err("DV library differs from its build identity".into());
                }
                library_identity.extend_from_slice(actual.as_bytes());
                artifacts.push((path, actual));
            }
            let mut environment = Vec::new();
            for (name, value) in &manifest.environment {
                if !matches!(name.as_str(), "LD_LIBRARY_PATH" | "VK_ICD_FILENAMES") {
                    return Err("unsupported DV helper environment".into());
                }
                let paths: Result<Vec<_>, String> = value
                    .split(':')
                    .map(|value| {
                        let path = Path::new(value);
                        if path.is_absolute()
                            || path
                                .components()
                                .any(|part| !matches!(part, std::path::Component::Normal(_)))
                        {
                            return Err("DV environment escapes its root".into());
                        }
                        let path = root.join(path).canonicalize().map_err(|e| e.to_string())?;
                        if !path.starts_with(&root) {
                            return Err("DV environment escapes its root".into());
                        }
                        Ok(path)
                    })
                    .collect();
                environment.push((
                    name.clone(),
                    std::env::join_paths(paths?).map_err(|e| e.to_string())?,
                ));
            }
            let executable_identity = manifest
                .tools
                .get("renderer")
                .ok_or("missing renderer")?
                .sha256
                .as_bytes();
            let identity = DvBackendIdentity::new(
                digest(executable_identity)?,
                digest(&library_identity)?,
                DvDigest::new(manifest.parser_sha256).map_err(|e| e.to_string())?,
                DvDigest::new(manifest.source_sha256).map_err(|e| e.to_string())?,
                1,
                digest(&bytes)?,
            )
            .map_err(|e| e.to_string())?;
            artifacts.push((root.join("build-identity.json"), hash(&bytes)));
            let scanner_sha256 = manifest
                .tools
                .get("scanner")
                .ok_or("missing scanner")?
                .sha256
                .clone();
            let decoder_abi = ["avcodec", "avformat", "avutil"]
                .iter()
                .map(|key| format!("{key}={}", manifest.abi[*key]))
                .collect::<Vec<_>>()
                .join(",");
            Ok(Some(Arc::new(Self {
                renderer: tools.remove("renderer").ok_or("missing renderer")?,
                scanner: tools.remove("scanner").ok_or("missing scanner")?,
                renderer_env: environment,
                identity,
                scanner_sha256,
                decoder_abi,
                artifacts: Arc::new(artifacts),
            })))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub(crate) async fn is_current(&self) -> bool {
        let artifacts = self.artifacts.clone();
        crate::subtitle_models::admitted_work::spawn_blocking(move || {
            artifacts
                .iter()
                .all(|(path, expected)| hash_file(path).is_ok_and(|actual| actual == *expected))
        })
        .await
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod pinned_runtime_renderer_sources {
    /// plurxd refuses a DV helper bundle built from other renderer sources
    /// (2026-11-11, after #1481: processed Dolby Vision off on every node).
    /// The bundle is now compiled by the application image's helper stage
    /// from this tree, and stamped by `bundle-manifest.py`. This daemon's own
    /// identity must equal the one that installer computes for the same
    /// sources, or the image build stops at `plurxd dv-helper-identity`.
    ///
    /// The name is the regression's recorded one: when it was written the
    /// bundle came from the pinned media runtime and the identity was a value
    /// recorded beside that pin. Nothing is recorded any more; the installer
    /// is asked.
    #[test]
    fn this_daemon_accepts_the_pinned_media_runtimes_dv_helpers() {
        let sources =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/dv_processing");
        let installer = std::process::Command::new("python3")
            .arg(sources.join("bundle-manifest.py"))
            .arg("--identity")
            .arg(&sources)
            .output()
            .expect("run the bundle installer's identity");
        assert!(
            installer.status.success(),
            "{}",
            String::from_utf8_lossy(&installer.stderr)
        );
        assert_eq!(
            super::renderer_source_identity(),
            String::from_utf8_lossy(&installer.stdout).trim(),
            "the daemon and tools/dv_processing/bundle-manifest.py digest different \
             renderer sources: the helper stage would build a bundle this daemon refuses"
        );
        // The application build is where that bundle comes from, and where
        // the daemon is asked again about the installed bytes.
        let recipe = include_str!("../../../Dockerfile");
        assert!(recipe.contains("COPY tools/dv_processing /build-inputs/dv_processing"));
        assert!(recipe.contains("RUN [\"plurxd\", \"dv-helper-identity\"]"));
        assert!(!recipe.contains("PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256"));
    }
}

#[cfg(test)]
mod title_scanner_identity_tests {
    use super::*;

    fn backend(renderer: &[u8]) -> DvBackendIdentity {
        let digest = |bytes: &[u8]| DvDigest::new(hash(bytes)).expect("digest");
        DvBackendIdentity::new(
            digest(renderer),
            digest(b"libraries"),
            digest(b"parser"),
            digest(b"sources"),
            1,
            digest(b"manifest"),
        )
        .expect("backend")
    }

    fn scanner(directory: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).expect("scanner");
        path
    }

    /// A renderer, shader or libplacebo change must not re-queue every title;
    /// only a different scanner may.
    #[test]
    fn title_demand_is_keyed_by_the_scanner_alone() {
        let directory = tempfile::tempdir().expect("scratch");
        let first = scanner(&directory, "first", b"scanner one");
        let second = scanner(&directory, "second", b"scanner two");
        let before =
            ToolSet::lifecycle_fixture_with_scanner(backend(b"renderer one"), first.clone())
                .expect("tools");
        let new_renderer = ToolSet::lifecycle_fixture_with_scanner(backend(b"renderer two"), first)
            .expect("tools");
        let new_scanner = ToolSet::lifecycle_fixture_with_scanner(backend(b"renderer one"), second)
            .expect("tools");
        assert_ne!(before.backend_digest(), new_renderer.backend_digest());
        assert_eq!(
            before.title_scanner_digest(),
            new_renderer.title_scanner_digest()
        );
        assert_eq!(before.backend_digest(), new_scanner.backend_digest());
        assert_ne!(
            before.title_scanner_digest(),
            new_scanner.title_scanner_digest()
        );
    }

    /// Decision G: an F+ fit is keyed by the refit mode and the decoder ABI,
    /// so a rebuilt bundle (another renderer, shaders, libplacebo) keeps
    /// every stored fit, and a changed refit mode or decoder ABI does not.
    #[test]
    fn refit_identity_is_the_refit_mode_and_the_decoder_abi() {
        let one = ToolSet::lifecycle_fixture(backend(b"renderer one"));
        let two = ToolSet::lifecycle_fixture(backend(b"renderer two"));
        assert_ne!(one.backend_digest(), two.backend_digest());
        assert_eq!(one.refit_helper_identity(), two.refit_helper_identity());
        let mut newer_abi = ToolSet::lifecycle_fixture(backend(b"renderer one"));
        newer_abi.decoder_abi = "avcodec=62,avformat=62,avutil=60".into();
        assert_ne!(
            one.refit_helper_identity(),
            newer_abi.refit_helper_identity()
        );
        use sha2::{Digest, Sha256};
        let mut sources = Sha256::new();
        sources.update(include_bytes!("../../../tools/dv_processing/refit_mode.h"));
        sources.update(include_bytes!(
            "../../../tools/dv_processing/refit_compose.h"
        ));
        assert_eq!(
            hex::encode(sources.finalize()),
            REFIT_MODE_SOURCES,
            "the refit mode changed: bump REFIT_MODE_VERSION if what it prints changed, then re-pin"
        );
    }

    /// Every title-demand reader and writer uses the scanner key; FEL verdicts
    /// keep the backend key because they measure the renderer.
    #[test]
    fn title_demand_call_sites_use_the_scanner_key() {
        for (name, source) in [
            ("title_work.rs", include_str!("source_probe/title_work.rs")),
            ("dv_title.rs", include_str!("source_probe/dv_title.rs")),
        ] {
            assert!(source.contains("title_scanner_digest()"), "{name}");
            // The backend key appears only as the FEL identity it measures.
            assert_eq!(
                source.matches("backend_digest()").count(),
                source
                    .matches("fel_identity(&tools.backend_digest())")
                    .count(),
                "{name}"
            );
        }
        let create = include_str!("transcode/manager/create.rs");
        assert!(create.contains("self.fel_identity(&tools.backend_digest())"));
        // FEL qualification is now a recipe request that carries no title
        // demand, so the creator has no scanner-keyed call left. The backend
        // key may still appear there only as the FEL identity it measures.
        assert_eq!(
            create.matches("backend_digest()").count(),
            create
                .matches("fel_identity(&tools.backend_digest())")
                .count()
                + create
                    .matches("fel_identity_for_encoder(&tools.backend_digest(), ")
                    .count()
        );
        assert!(!create.contains("dv_title_demand"));
        // The index build is the remaining title-demand writer outside the probe.
        assert!(include_str!("fragindex.rs")
            .contains("scanner_digest: self.tools.title_scanner_digest(),"));
    }
}

#[cfg(test)]
mod bundle_capture_tests {
    use super::*;

    /// Generate the actual installer's manifest and enter the same capture
    /// boundary as production. Stub artifacts are never executed.
    #[tokio::test]
    async fn revised_bundle_capture_accepts_current_sources_and_rejects_stale_or_incomplete_identity(
    ) {
        let fixture = tempfile::tempdir().expect("bundle fixture");
        let root = fixture.path().join("bundle");
        for directory in ["bin", "lib", "sources"] {
            std::fs::create_dir_all(root.join(directory)).expect("fixture directory");
        }
        for name in [
            "segment_decode_render",
            "mux_rgb",
            "author_p81",
            "scan_title",
        ] {
            std::fs::write(root.join("bin").join(name), name).expect("stub helper");
        }
        let darwin = cfg!(target_os = "macos");
        let library = if darwin {
            "lib/libplacebo.387.dylib"
        } else {
            "lib/libplacebo.so.387"
        };
        std::fs::write(root.join(library), b"fixture placebo").expect("write stub placebo library");
        std::fs::write(root.join("sources/libdovi.a"), b"fixture parser")
            .expect("write stub parser library");
        std::fs::write(
            root.join("sources/abi.json"),
            br#"{"libplacebo":387,"avcodec":62,"avformat":62,"avutil":60}"#,
        )
        .expect("write fixture ABI");
        let source = fixture.path().join("source");
        std::fs::create_dir(&source).expect("create copied-source directory");
        let actual_source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/dv_processing");
        for entry in std::fs::read_dir(&actual_source).expect("enumerate actual helper sources") {
            let entry = entry.expect("read source entry");
            if entry
                .file_type()
                .expect("inspect source entry type")
                .is_file()
            {
                std::fs::copy(entry.path(), source.join(entry.file_name()))
                    .expect("copy manifest source input");
            }
        }
        let installer = include_str!("../../../tools/dv_processing/bundle-manifest.py");
        let generate = |script: &str| {
            let result = std::process::Command::new("python3")
                .args(["-c", script])
                .arg(&source)
                .arg(&root)
                .args(darwin.then_some("--darwin"))
                .output()
                .expect("run installed manifest generator");
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        generate(installer);
        let tools = ToolSet::capture_at(root.clone())
            .await
            .expect("validate current installer bundle")
            .expect("current bundle accepted");
        assert!(tools.is_current().await);
        let mut config = plurx_core::config::Config::default();
        config.storage.data_dir = fixture.path().join("data");
        let dirs = crate::state::Dirs {
            artwork: fixture.path().join("artwork"),
            transcode: fixture.path().join("transcode"),
            cache: fixture.path().join("cache"),
            subs: fixture.path().join("subs"),
            runtime_cache: fixture.path().join("runtime"),
            renditions: fixture.path().join("renditions"),
        };
        let validated_directories = tools.executable_directories();
        let (_, production_tools) =
            crate::source_custody::access_root_paths(&config, &dirs, &validated_directories);
        for executable in [&tools.renderer, &tools.scanner] {
            let parent = executable.parent().expect("validated helper directory");
            assert!(
                production_tools.iter().any(|path| path == parent),
                "startup must qualify the actual validated helper parent: {parent:?}"
            );
        }

        assert_eq!(
            tools.renderer,
            root.join("bin/segment_decode_render")
                .canonicalize()
                .expect("canonical fixture renderer")
        );
        for missing in ["author_p81.c", "movie_source_io.h", "source_reader.h"] {
            // Exercise the producer's actual INPUTS, independently of source
            // formatting or quote style. The final generated manifest is
            // internally consistent but incomplete for this daemon.
            let incomplete = format!(
                "{installer}\nimport sys\nomitted = {missing:?}\nassert omitted in INPUTS\n\
                 INPUTS = tuple(name for name in INPUTS if name != omitted)\n\
                 build(Path(sys.argv[1]), Path(sys.argv[2]), \"--darwin\" in sys.argv[3:])\n"
            );
            generate(&incomplete);
            assert!(
                matches!(ToolSet::capture_at(root.clone()).await, Err(error) if error == "unsupported DV build identity"),
                "capture rejects a manifest missing {missing}"
            );
        }
        // Self-consistent stale source bytes still cannot authorize this daemon.
        for stale in ["author_p81.c", "movie_source_io.h", "source_reader.h"] {
            let path = source.join(stale);
            let original = std::fs::read(&path).expect("read current fixture source");
            std::fs::write(&path, b"deliberately stale fixture source")
                .expect("write stale fixture source");
            generate(installer);
            assert!(
                matches!(ToolSet::capture_at(root.clone()).await, Err(error) if error == "unsupported DV build identity"),
                "capture rejects stale {stale}"
            );
            std::fs::write(path, original).expect("restore current fixture source");
        }
        generate(installer);
        assert!(ToolSet::capture_at(root)
            .await
            .expect("validate restored current bundle")
            .is_some());
    }
}
