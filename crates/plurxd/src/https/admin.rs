//! Node-local HTTPS intent; no replicated setting or live certificate reload.
use super::{CertificateFacts, Identity};
use plurx_core::config::{
    https::{HttpsConfig, HttpsLocalConfig, HttpsOverrides, HttpsSettings},
    Config,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Snapshot {
    pub node_id: String,
    /// Decimal string on the wire; the local envelope retains a u64 integer.
    pub revision: String,
    pub saved: HttpsConfig,
    pub desired: HttpsConfig,
    pub active: HttpsConfig,
    pub restart_required: bool,
    pub overrides: Vec<String>,
    pub certificate: Option<CertificateFacts>,
    /// Last publication outcome observed by this process, not a disk probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability_confirmed: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
    pub node_id: String,
    pub revision: String,
    pub desired: HttpsConfig,
}

#[derive(Debug)]
pub(crate) enum SaveError {
    Invalid(String),
    Conflict(&'static str, &'static str),
    Internal(&'static str),
}

struct Desired {
    envelope: HttpsSettings,
    effective: HttpsConfig,
    durability: Option<bool>,
}

pub(crate) struct Control {
    node_id: String,
    config: Config,
    overrides: HttpsOverrides,
    active: HttpsConfig,
    identity: Option<Arc<Identity>>,
    desired: Arc<Mutex<Desired>>,
}

impl Control {
    pub(crate) fn new(
        node_id: String,
        config: Config,
        identity: Option<Arc<Identity>>,
    ) -> anyhow::Result<Self> {
        let bind = config.server.bind.ip();
        let active = config.server.https.clone().normalize(bind)?;
        anyhow::ensure!(
            active.enabled == identity.is_some(),
            "HTTPS loaded identity does not match active intent"
        );
        let local = config
            .https_local
            .clone()
            .unwrap_or_else(|| HttpsLocalConfig {
                saved: HttpsSettings::initial(config.server.https.clone().normalize_saved(bind)),
                overrides: HttpsOverrides::default(),
            });
        let mut envelope = local.saved;
        envelope.settings = envelope.settings.normalize_saved(bind);
        let effective = local.overrides.apply(envelope.settings.clone(), bind)?;
        effective.validate_ports(&config)?;
        anyhow::ensure!(
            effective == active,
            "HTTPS configuration metadata is inconsistent"
        );
        Ok(Self {
            node_id,
            config,
            active,
            identity,
            overrides: local.overrides,
            desired: Arc::new(Mutex::new(Desired {
                envelope,
                effective,
                durability: None,
            })),
        })
    }

    fn snapshot_locked(&self, desired: &Desired) -> Snapshot {
        Snapshot {
            node_id: self.node_id.clone(),
            revision: desired.envelope.revision.to_string(),
            saved: desired.envelope.settings.clone(),
            desired: desired.effective.clone(),
            active: self.active.clone(),
            restart_required: desired.effective != self.active,
            overrides: self.overrides.fields(),
            certificate: self
                .identity
                .as_ref()
                .map(|identity| identity.facts.clone()),
            durability_confirmed: desired.durability,
        }
    }

    pub(crate) async fn snapshot(&self) -> Snapshot {
        self.snapshot_locked(&*self.desired.lock().await)
    }

    pub(crate) fn certificate(&self) -> Option<Vec<u8>> {
        self.identity
            .as_ref()
            .map(|identity| identity.public_chain_pem())
    }

    pub(crate) async fn update(self: &Arc<Self>, update: Update) -> Result<Snapshot, SaveError> {
        self.update_using(update, |envelope, root| envelope.persist(root))
            .await
    }

    /// The owned guard moves into the blocking worker. Cancellation of the HTTP
    /// waiter cannot drop serialization between publication and adoption.
    async fn update_using<F>(
        self: &Arc<Self>,
        update: Update,
        publish: F,
    ) -> Result<Snapshot, SaveError>
    where
        F: FnOnce(
                &HttpsSettings,
                &std::path::Path,
            ) -> Result<
                plurx_core::config::https::HttpsPersistence,
                plurx_core::error::ConfigError,
            > + Send
            + 'static,
    {
        if update.node_id != self.node_id {
            return Err(SaveError::Conflict(
                "https_node_conflict",
                "This request reached another node; reload the addressed node before editing.",
            ));
        }
        let expected = parse_revision(&update.revision)?;
        let mut guard = self.desired.clone().lock_owned().await;
        if expected != guard.envelope.revision {
            return Err(SaveError::Conflict(
                "https_revision_conflict",
                "HTTPS settings changed; reload this node before editing again.",
            ));
        }
        let revision = expected.checked_add(1).ok_or(SaveError::Conflict(
            "https_revision_exhausted",
            "The HTTPS revision counter is exhausted; this save was refused.",
        ))?;
        // Compare the raw submitted env-owned fields first. Only effective
        // choices are validated; masked base choices must not become sticky.
        let saved = self.overrides.editable_save(&update.desired, &guard.effective, &guard.envelope.settings)
            .map_err(|_| SaveError::Conflict("https_environment_conflict", "Environment-controlled HTTPS fields cannot be changed; reload their current values."))?
            .normalize_saved(self.config.server.bind.ip());
        let effective = self
            .overrides
            .apply(saved.clone(), self.config.server.bind.ip())
            .map_err(|error| SaveError::Invalid(error.to_string()))?;
        effective
            .validate_ports(&self.config)
            .map_err(|error| SaveError::Invalid(error.to_string()))?;
        let envelope = HttpsSettings {
            version: 1,
            revision,
            settings: saved,
        };
        let control = self.clone();
        tokio::task::spawn_blocking(move || {
            if effective.enabled {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .ok().and_then(|duration| i64::try_from(duration.as_secs()).ok())
                    .ok_or_else(|| SaveError::Invalid("HTTPS requires a valid system clock.".into()))?;
                Identity::load(&effective, &control.config.storage.data_dir, now, false)
                    .map_err(|_| SaveError::Invalid("HTTPS identity validation failed. Check local paths, key match, validity and permissions; managed SAN changes require explicit replacement while stopped.".into()))?;
            }
            let outcome = publish(&envelope, &control.config.storage.data_dir)
                .map_err(|_| SaveError::Internal("Cannot publish node-local HTTPS settings."))?;
            // persist returning Ok means published, including sync uncertainty.
            // Adoption uses already-prepared values before releasing the guard.
            guard.envelope = envelope;
            guard.effective = effective;
            guard.durability = Some(outcome.durability_confirmed);
            Ok(control.snapshot_locked(&guard))
        }).await.map_err(|_| SaveError::Internal("HTTPS settings worker failed."))?
    }
}

fn parse_revision(value: &str) -> Result<u64, SaveError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(SaveError::Invalid(
            "HTTPS revision must be a canonical decimal u64 string.".into(),
        ));
    }
    value.parse().map_err(|_| {
        SaveError::Invalid("HTTPS revision must be a canonical decimal u64 string.".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::config::https::{HttpsPersistence, HTTPS_SETTINGS_FILE};
    use std::{path::Path, time::Duration};

    fn config(root: &Path) -> Config {
        let mut config = Config::default();
        config.storage.data_dir = root.to_path_buf();
        config.server.bind = "127.0.0.1:0".parse().expect("bind");
        config
            .resolve_https(HttpsOverrides::default())
            .expect("node settings");
        config
    }
    fn control(root: &Path) -> Arc<Control> {
        Arc::new(Control::new("node-one".into(), config(root), None).expect("control"))
    }
    fn update(snapshot: &Snapshot) -> Update {
        Update {
            node_id: snapshot.node_id.clone(),
            revision: snapshot.revision.clone(),
            desired: snapshot.desired.clone(),
        }
    }

    #[tokio::test]
    async fn local_https_save_distinguishes_saved_desired_active_and_never_provisions() {
        let root = crate::test_tempdir().expect("tempdir");
        let control = control(root.path());
        let before = control.snapshot().await;
        assert_eq!(before.revision, "0");
        assert!(!before.restart_required);
        assert!(!root.path().join("https").exists());
        let mut request = update(&before);
        request.desired.enabled = true;
        let saved = control.update(request).await.expect("save");
        assert_eq!(saved.revision, "1");
        assert!(saved.saved.enabled && saved.desired.enabled);
        assert!(!saved.active.enabled && saved.certificate.is_none());
        assert!(saved.restart_required);
        assert_eq!(saved.durability_confirmed, Some(true));
        assert!(
            !root.path().join("https").exists(),
            "Save must not create an identity"
        );
        let disk = HttpsSettings::read(root.path(), Default::default()).expect("envelope");
        assert_eq!(disk.revision, 1);
        assert_eq!(disk.settings, saved.saved);
        assert!(control.certificate().is_none());
    }

    #[tokio::test]
    async fn local_https_disable_ignores_missing_supplied_files_and_restart_updates_active() {
        let root = crate::test_tempdir().expect("tempdir");
        let mut config = config(root.path());
        config.server.https.enabled = true;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let managed = Identity::load(&config.server.https, root.path(), now, true)
            .expect("generated fixture")
            .expect("identity");
        let bundle = std::fs::read(
            root.path()
                .join("https")
                .join(super::super::MANAGED_IDENTITY_FILE),
        )
        .expect("synthetic bundle");
        let blocks = super::super::pem_blocks(&bundle).expect("synthetic blocks");
        config.server.https.cert_file = root.path().join("chain.pem");
        config.server.https.key_file = root.path().join("key.pem");
        std::fs::write(&config.server.https.cert_file, managed.public_chain_pem()).expect("chain");
        std::fs::write(&config.server.https.key_file, &blocks[1].1).expect("synthetic key");
        config
            .resolve_https(HttpsOverrides::default())
            .expect("supplied config");
        let identity = Identity::load(&config.server.https, root.path(), now, false)
            .expect("supplied fixture")
            .map(Arc::new);
        let control = Arc::new(
            Control::new("node-one".into(), config.clone(), identity)
                .expect("active supplied control"),
        );
        let before = control.snapshot().await;
        assert_eq!(before.certificate.as_ref().expect("facts").mode, "supplied");
        std::fs::remove_file(&config.server.https.cert_file).expect("remove chain");
        std::fs::remove_file(&config.server.https.key_file).expect("remove key");
        let mut disable = update(&before);
        disable.desired.enabled = false;
        let disabled = control
            .update(disable)
            .await
            .expect("disable without files");
        assert!(!disabled.saved.enabled && !disabled.desired.enabled);
        assert!(disabled.active.enabled && disabled.restart_required);
        assert_eq!(
            disabled.certificate.as_ref().expect("still loaded").sha256,
            before.certificate.as_ref().expect("initial facts").sha256
        );
        let mut enable = update(&disabled);
        enable.desired.enabled = true;
        assert!(matches!(
            control.update(enable).await,
            Err(SaveError::Invalid(_))
        ));
        assert_eq!(control.snapshot().await.revision, "1");
        assert_eq!(
            HttpsSettings::read(root.path(), Default::default())
                .expect("disk")
                .revision,
            1
        );
        config
            .resolve_https(HttpsOverrides::default())
            .expect("restart config");
        let restarted = Control::new("node-one".into(), config, None)
            .expect("disabled restart")
            .snapshot()
            .await;
        assert!(!restarted.active.enabled && !restarted.restart_required);
        assert!(restarted.certificate.is_none());
    }

    #[tokio::test]
    async fn local_https_revision_stays_exact_above_js_integer_range_and_across_restart() {
        let root = crate::test_tempdir().expect("tempdir");
        let base = config(root.path());
        HttpsSettings {
            version: 1,
            revision: 9_007_199_254_740_993,
            settings: base.server.https,
        }
        .persist(root.path())
        .expect("seed large revision");
        let first = control(root.path());
        let snapshot = first.snapshot().await;
        assert_eq!(snapshot.revision, "9007199254740993");
        let wire = serde_json::to_value(&snapshot).expect("snapshot JSON");
        assert!(wire["revision"].is_string());
        let mut request = update(&snapshot);
        request.desired.port = 32444;
        assert_eq!(
            first.update(request).await.expect("save").revision,
            "9007199254740994"
        );
        let restarted = control(root.path());
        let restarted = restarted.snapshot().await;
        assert_eq!(restarted.revision, "9007199254740994");
        assert_eq!(restarted.active.port, 32444);
        assert!(!restarted.restart_required);
        let disk = HttpsSettings::read(root.path(), Default::default()).expect("envelope");
        assert_eq!(disk.revision, 9_007_199_254_740_994);
    }

    #[tokio::test]
    async fn local_https_revision_rejects_noncanonical_values_and_overflow_without_mutation() {
        for value in [
            "",
            "01",
            "+1",
            "1.0",
            "1e3",
            " 1",
            "-1",
            "18446744073709551616",
            "١",
        ] {
            assert!(parse_revision(value).is_err(), "{value}");
        }
        let root = crate::test_tempdir().expect("tempdir");
        HttpsSettings {
            version: 1,
            revision: u64::MAX,
            settings: config(root.path()).server.https,
        }
        .persist(root.path())
        .expect("seed exhausted revision");
        let control = control(root.path());
        let before = control.snapshot().await;
        let bytes = std::fs::read(root.path().join(HTTPS_SETTINGS_FILE)).expect("before");
        assert!(matches!(
            control.update(update(&before)).await,
            Err(SaveError::Conflict("https_revision_exhausted", _))
        ));
        assert_eq!(control.snapshot().await.revision, before.revision);
        assert_eq!(
            std::fs::read(root.path().join(HTTPS_SETTINGS_FILE)).expect("after"),
            bytes
        );
    }

    #[tokio::test]
    async fn local_https_node_and_revision_guards_serialize_concurrent_saves() {
        let root = crate::test_tempdir().expect("tempdir");
        let control = control(root.path());
        let before = control.snapshot().await;
        let mut foreign = update(&before);
        foreign.node_id = "other-node".into();
        assert!(matches!(
            control.update(foreign).await,
            Err(SaveError::Conflict("https_node_conflict", _))
        ));
        assert!(!root.path().join(HTTPS_SETTINGS_FILE).exists());
        let (one, two) = tokio::join!(
            control.update(update(&before)),
            control.update(update(&before))
        );
        assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
        assert!(matches!(
            one.as_ref().err().or(two.as_ref().err()),
            Some(SaveError::Conflict("https_revision_conflict", _))
        ));
        assert_eq!(control.snapshot().await.revision, "1");
        assert_eq!(
            HttpsSettings::read(root.path(), Default::default())
                .expect("envelope")
                .revision,
            1
        );
    }

    #[tokio::test]
    async fn local_https_env_fields_conflict_before_validation_and_preserve_masked_values() {
        let root = crate::test_tempdir().expect("tempdir");
        let mut config = config(root.path());
        config.server.https.port = 0; // Invalid only without the explicit repair.
        let overrides =
            HttpsOverrides::capture(|name| (name == "PLURX_HTTPS_PORT").then(|| "32443".into()));
        config
            .resolve_https(overrides)
            .expect("effective env repair");
        let control = Arc::new(Control::new("node-one".into(), config, None).expect("control"));
        let before = control.snapshot().await;
        assert_eq!(before.saved.port, 0);
        assert_eq!(before.desired.port, 32443);
        let mut rejected = update(&before);
        rejected.desired.port = 0;
        assert!(matches!(
            control.update(rejected).await,
            Err(SaveError::Conflict("https_environment_conflict", _))
        ));
        let mut accepted = update(&before);
        accepted.desired.enabled = true;
        let saved = control
            .update(accepted)
            .await
            .expect("editable field saves");
        assert_eq!(saved.saved.port, 0);
        assert_eq!(saved.desired.port, 32443);
        assert_eq!(
            HttpsSettings::read(root.path(), Default::default())
                .expect("envelope")
                .settings
                .port,
            0
        );
        assert!(!root.path().join("https").exists());
    }

    #[tokio::test]
    async fn local_https_failed_publication_keeps_memory_and_revision_unchanged() {
        let root = crate::test_tempdir().expect("tempdir");
        let control = control(root.path());
        let before = control.snapshot().await;
        let result = control
            .update_using(update(&before), |_, _| {
                Err(plurx_core::error::ConfigError::Value {
                    key: "https".into(),
                    message: "synthetic pre-publication failure".into(),
                })
            })
            .await;
        assert!(matches!(result, Err(SaveError::Internal(_))));
        assert_eq!(control.snapshot().await.revision, "0");
        assert!(!root.path().join(HTTPS_SETTINGS_FILE).exists());
        assert_eq!(
            control
                .update(update(&before))
                .await
                .expect("next save")
                .revision,
            "1"
        );
    }

    #[tokio::test]
    async fn local_https_published_sync_uncertainty_adopts_revision_and_rejects_stale_retry() {
        let root = crate::test_tempdir().expect("tempdir");
        let control = control(root.path());
        let before = control.snapshot().await;
        let saved = control
            .update_using(update(&before), |envelope, root| {
                envelope.persist(root)?;
                Ok(HttpsPersistence {
                    durability_confirmed: false,
                })
            })
            .await
            .expect("published save");
        assert_eq!(saved.revision, "1");
        assert_eq!(saved.durability_confirmed, Some(false));
        assert_eq!(control.snapshot().await.durability_confirmed, Some(false));
        assert!(matches!(
            control.update(update(&before)).await,
            Err(SaveError::Conflict("https_revision_conflict", _))
        ));
        assert_eq!(
            HttpsSettings::read(root.path(), Default::default())
                .expect("envelope")
                .revision,
            1
        );
    }

    #[tokio::test]
    async fn local_https_cancelled_waiter_retains_guard_through_publication_and_adoption() {
        let root = crate::test_tempdir().expect("tempdir");
        let control = control(root.path());
        let before = control.snapshot().await;
        let request = update(&before);
        let (published, publication) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker_control = control.clone();
        let waiter = tokio::spawn(async move {
            worker_control
                .update_using(request, move |envelope, root| {
                    let outcome = envelope.persist(root)?;
                    let _ = published.send(());
                    // A failed test cannot trap the blocking worker indefinitely.
                    // Even a disconnected/timed-out observer must allow adoption.
                    let _ = released.recv_timeout(Duration::from_secs(5));
                    Ok(outcome)
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), publication)
            .await
            .expect("publication bound")
            .expect("published");
        assert_eq!(
            HttpsSettings::read(root.path(), Default::default())
                .expect("published envelope")
                .revision,
            1
        );
        waiter.abort();
        assert!(waiter
            .await
            .expect_err("HTTP waiter cancelled")
            .is_cancelled());
        let reading = control.snapshot();
        tokio::pin!(reading);
        tokio::select! {
            _ = &mut reading => panic!("cancellation must not release the worker's serialization guard"),
            () = tokio::time::sleep(Duration::from_millis(20)) => {},
        }
        release.send(()).expect("finish publication worker");
        let adopted = tokio::time::timeout(Duration::from_secs(1), reading)
            .await
            .expect("observe completed worker adoption");
        assert_eq!(adopted.revision, "1");
        assert!(matches!(
            control.update(update(&before)).await,
            Err(SaveError::Conflict("https_revision_conflict", _))
        ));
        // Acquiring the guard after adoption observes completion of all file
        // work before this fixture's directory can be torn down.
    }
}
