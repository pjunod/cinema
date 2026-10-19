//! Local signed staging fixture. This does not exercise GitHub discovery/download.
use plurx_desktop_control::{
    installation::{self, InstallMethod, InstallationV1},
    native, package,
    protocol::Operation,
    transaction::{Phase, TransactionStore},
    update,
};
use std::{env, fs, path::PathBuf};

fn main() {
    if let Err(error) = execute() {
        eprintln!("local-staging fixture refused: {error}");
        std::process::exit(1);
    }
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        return Err(
            "usage: plurx-qualification-stage ABS_SIGNED_ASSETS ABS_OVERLAY_RECEIPT".into(),
        );
    }
    if env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || env::var("RUNNER_ENVIRONMENT").as_deref() != Ok("github-hosted")
        || env::var("GITHUB_SERVER_URL").as_deref() != Ok("https://github.com")
        || env::var("PLURX_QUALIFICATION_MUTATE").as_deref() != Ok("disposable-hosted-vm")
    {
        return Err("local staging requires deliberate disposable hosted VM qualification".into());
    }
    let assets = PathBuf::from(&args[1]).canonicalize()?;
    let overlay_path = PathBuf::from(&args[2]).canonicalize()?;
    if !PathBuf::from(&args[1]).is_absolute() || !PathBuf::from(&args[2]).is_absolute() {
        return Err("fixture input paths must be absolute".into());
    }
    let overlay: serde_json::Value = serde_json::from_slice(&fs::read(overlay_path)?)?;
    let candidate = env::var("GITHUB_SHA")?;
    if overlay["candidate"].as_str() != Some(&candidate)
        || overlay["scope"].as_str() != Some("isolated-local-trust-overlay")
        || overlay["committed_overlay_sha"]
            .as_str()
            .is_none_or(|s| s.len() != 40)
        || overlay["committed_overlay_tree"]
            .as_str()
            .is_none_or(|s| s.len() != 40)
        || overlay["diff_sha256"]
            .as_str()
            .is_none_or(|s| s.len() != 64)
    {
        return Err("overlay receipt does not identify the dispatched committed test build".into());
    }
    let keys = update::production_keys()?;
    if keys.len() != 1 || keys[0].id != "qualification-local-only" {
        return Err(
            "driver must be built against the isolated ephemeral public-root overlay".into(),
        );
    }
    if option_env!("PLURX_QUALIFICATION_OVERLAY_SHA") != overlay["committed_overlay_sha"].as_str()
        || Some(
            keys[0]
                .public_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )
        .as_deref()
            != overlay["public_key_hex"].as_str()
    {
        return Err(
            "compiled fixture overlay identity/public root differs from retained receipt".into(),
        );
    }
    let i = InstallationV1::load(&installation::default_record()?)?;
    i.managed()?;
    let bytes = fs::read(assets.join(update::MANIFEST_NAME))?;
    let signature = fs::read(assets.join(update::SIGNATURE_NAME))?;
    let store = TransactionStore::locked(&i)?;
    let candidate = update::verify_manifest(
        &bytes,
        &signature,
        &keys,
        &i,
        &store.ledger()?,
        plurx_desktop_control::unix_time()?,
    )?;
    let mut journal = store.create(&i, Operation::Upgrade, Some(&candidate))?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let dir = store.dir(&journal.transaction_id)?;
        dir.write(update::MANIFEST_NAME, &bytes, 0o400)?;
        dir.write(update::SIGNATURE_NAME, &signature, 0o400)?;
        for artifact in update::select_cohort(&candidate, &i)? {
            let source = assets.join(&artifact.name);
            if !fs::symlink_metadata(&source)?.is_file() {
                return Err("fixture artifact must be a regular file".into());
            }
            update::stage_artifact(&mut fs::File::open(source)?, artifact, &dir)?;
        }
        if i.method == InstallMethod::MacosNative {
            let artifact = update::select_cohort(&candidate, &i)?[0];
            dir.child("extracted", true)?;
            let destination = i
                .paths
                .state
                .join("transactions")
                .join(&journal.transaction_id)
                .join("extracted");
            update::extract_archive(
                dir.open(&artifact.name)?,
                &destination,
                artifact.size.saturating_mul(8).min(32 * 1024 * 1024 * 1024),
            )?;
            let package_root = native::archive_package_root(
                &destination,
                artifact,
                &candidate.manifest().version,
            )?;
            native::verify_closure(&package_root, &candidate.manifest().version)?;
        } else {
            let plan = package::preflight(&i, &candidate, &store, &journal)?;
            journal.package_inventory_before = Some(package::inventory(&i, &plan.packages)?);
        }
        store.accept(&candidate)?;
        store.save(&journal)?;
        Ok(())
    })();
    if let Err(error) = result {
        journal.transition(Phase::Cancelled)?;
        journal.detail = Some(error.to_string());
        store.save(&journal)?;
        return Err(error);
    }
    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1,
            "scope": "local-signed-staging-fixture-not-github-download",
            "candidate": env::var("GITHUB_SHA")?,
            "transaction_id": journal.transaction_id,
            "manifest_digest": candidate.digest(),
            "version": candidate.manifest().version,
            "overlay": overlay,
        })
    );
    Ok(())
}
