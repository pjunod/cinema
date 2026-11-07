use super::*;
use std::{
    fs,
    io::Cursor,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
};

const NOW: u64 = 1500;
const CODE: &[u8] = b"#!/bin/sh\nexit 0\n";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let uid = installation::native_uid();
        let mut candidates = Vec::new();
        if let Some(target) = std::env::var_os("CARGO_TARGET_DIR") {
            candidates.push(PathBuf::from(target));
        }
        if let Ok(home) = installation::home_for(uid) {
            candidates.push(home);
        }
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let mut refusals = Vec::new();
        for candidate in candidates {
            let Ok(base) = candidate.canonicalize() else {
                continue;
            };
            if let Err(error) = secure_fs::open_dir(&base, uid) {
                refusals.push(format!("{}: {error}", base.display()));
                continue;
            }
            let path = base.join(format!(".plurx-helper-fixture-{}", uuid::Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("exclusive private fixture directory");
            secure_fs::open_dir(&path, uid).expect("new fixture retains verified ancestry");
            return Self(path);
        }
        panic!("no trusted fixture ancestry for uid {uid}: {refusals:?}");
    }
    fn dir(&self) -> SecureDir {
        secure_fs::open_dir(&self.0, installation::native_uid()).expect("trusted fixture")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            if let Ok(metadata) = fs::symlink_metadata(path) {
                if metadata.is_dir() {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                    if let Ok(entries) = fs::read_dir(path) {
                        for entry in entries.flatten() {
                            writable(&entry.path());
                        }
                    }
                }
            }
        }
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn archive(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (name, kind, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_mode(0o755);
        header.set_size(bytes.len() as u64);
        if kind.is_symlink() {
            header.set_link_name("/bin/sh").expect("link fixture");
        }
        header.set_cksum();
        archive
            .append_data(&mut header, name, *bytes)
            .expect("fixture tar entry");
    }
    archive
        .into_inner()
        .expect("finished tar")
        .finish()
        .expect("finished gzip")
}
struct Fixture {
    _scratch: Scratch,
    i: InstallationV1,
    base: SecureDir,
    metadata: MigrationHelperV1,
    verified: VerifiedManifest,
    bytes: Vec<u8>,
    signature: Vec<u8>,
    keys: Vec<update::TrustKey>,
    ledger: AcceptedReleasesV1,
    bundle: Vec<u8>,
}
impl Fixture {
    fn new(bundle: Vec<u8>, executable_sha256: String) -> Self {
        let scratch = Scratch::new();
        let root = scratch.dir();
        let runtime = root.child("runtime", true).expect("runtime");
        let base = runtime.child(STORE, true).expect("helper base");
        let mut i = update::helper_fixture_installation();
        i.owner_uid = installation::native_uid();
        i.service_uid = i.owner_uid;
        i.paths.runtime = scratch.0.join("runtime");
        i.paths.data = scratch.0.join("data");
        root.child("data", true).expect("data");
        let mut metadata = update::helper_fixture_metadata("x86_64-unknown-linux-gnu");
        metadata.executable_sha256 = executable_sha256;
        metadata.artifact.size = bundle.len() as u64;
        metadata.artifact.sha256 = hex::encode(Sha256::digest(&bundle));
        metadata.bundle_sha256 = metadata.artifact.sha256.clone();
        let mut value =
            serde_json::to_value(update::helper_fixture_manifest()).expect("manifest value");
        value["schema_version"] = serde_json::json!(2);
        value["migration_helpers"] = serde_json::json!([metadata.clone()]);
        let bytes = serde_json::to_vec(&value).expect("signed bytes");
        let (signer, keys) = update::helper_fixture_signer();
        let signature = signer.sign(&bytes).as_ref().to_vec();
        let verified = update::verify_manifest(
            &bytes,
            &signature,
            &keys,
            &i,
            &AcceptedReleasesV1::default(),
            NOW,
        )
        .expect("real signature verification");
        let mut ledger = AcceptedReleasesV1::default();
        verified.accept(&mut ledger);
        Self {
            _scratch: scratch,
            i,
            base,
            metadata,
            verified,
            bytes,
            signature,
            keys,
            ledger,
            bundle,
        }
    }
    fn ordinary() -> Self {
        Self::new(
            archive(&[(EXECUTABLE, tar::EntryType::Regular, CODE)]),
            hex::encode(Sha256::digest(CODE)),
        )
    }
    fn publish(&self) -> Result<String> {
        let selected = helper_for(
            &self.verified,
            &self.i,
            &self.metadata.daemon_build,
            MigrationHelperPurpose::LegacyDataUpgrade,
        )?;
        publish(
            &self.i,
            &self.base,
            &self.verified,
            &selected,
            &mut Cursor::new(&self.bundle),
            Publication {
                bytes: &self.bytes,
                signature: &self.signature,
                ledger: self.ledger.clone(),
                keys: &self.keys,
                now: NOW,
            },
        )
    }
    fn load(&self, id: &str) -> Result<QualifiedGeneration> {
        load_from(
            &prospective_installation(&self.i, &self.verified),
            &self.base,
            id,
            &self.metadata.daemon_build,
            &self.metadata.source_cohorts,
            &self.keys,
            NOW,
        )
    }
    fn generation(&self, id: &str) -> SecureDir {
        self.base
            .child(GENERATIONS, false)
            .expect("generations")
            .child(id, false)
            .expect("generation")
    }
    fn rewrite(&self, id: &str, name: &str, bytes: &[u8]) {
        let dir = self.generation(id);
        dir_mode(&dir, 0o755).expect("owner writable");
        dir.write(name, bytes, 0o444)
            .expect("trusted owner rewrite");
        dir_mode(&dir, 0o555).expect("readonly");
    }
}

#[test]
fn signed_generation_is_immutable_revalidated_and_shared_leased() {
    let fixture = Fixture::ordinary();
    let id = fixture.publish().expect("authenticated publication");
    assert_eq!(fixture.publish().expect("verified reuse"), id);
    let qualified = fixture.load(&id).expect("qualified generation");
    qualified.revalidate().expect("current identity");
    assert_eq!(
        serde_json::to_value(qualified.metadata()).expect("loaded metadata"),
        serde_json::to_value(&fixture.metadata).expect("signed metadata")
    );
    assert_eq!(qualified.manifest_digest(), fixture.verified.digest());
    assert_eq!(
        fs::read(qualified.executable()).expect("actual executable"),
        CODE
    );
    assert_eq!(
        cleanup_lease(&fixture.i, &fixture.base, &id)
            .expect_err("live process lease blocks cleanup")
            .code,
        ErrorCode::Busy
    );
    drop(qualified);
    let cleanup = cleanup_lease(&fixture.i, &fixture.base, &id).expect("released shared lease");
    assert!(
        fixture.load(&id).is_err(),
        "exclusive cleanup excludes new processes"
    );
    drop(cleanup);
    fixture.load(&id).expect("released cleanup lease");
}

#[test]
fn signed_bundle_and_executable_hashes_are_independent_and_both_required() {
    let mut bad_bundle = Fixture::ordinary();
    bad_bundle.bundle[0] ^= 1;
    assert!(
        bad_bundle.publish().is_err(),
        "signed bundle hash is mandatory"
    );
    let bad_exe = Fixture::new(
        archive(&[(EXECUTABLE, tar::EntryType::Regular, CODE)]),
        "0".repeat(64),
    );
    assert!(
        bad_exe.publish().is_err(),
        "correct bundle hash cannot substitute for executable binding"
    );
    assert!(
        bad_exe.base.open(CURRENT).is_err(),
        "failed publication cannot select code"
    );
}

#[test]
fn archive_profile_rejects_links_duplicates_extra_files_and_trailing_members() {
    let profiles = vec![
        archive(&[(EXECUTABLE, tar::EntryType::Symlink, b"")]),
        archive(&[
            (EXECUTABLE, tar::EntryType::Regular, CODE),
            (EXECUTABLE, tar::EntryType::Regular, CODE),
        ]),
        archive(&[
            (EXECUTABLE, tar::EntryType::Regular, CODE),
            ("extra.so", tar::EntryType::Regular, b"not declared"),
        ]),
        archive(&[(
            "nested/plurx-storage-migrate",
            tar::EntryType::Regular,
            CODE,
        )]),
    ];
    for bundle in profiles {
        assert!(Fixture::new(bundle, hex::encode(Sha256::digest(CODE)))
            .publish()
            .is_err());
    }
    let original = archive(&[(EXECUTABLE, tar::EntryType::Regular, CODE)]);
    for extra in [b"trailing bytes".to_vec(), original.clone()] {
        let mut bundle = original.clone();
        bundle.extend(extra);
        assert!(Fixture::new(bundle, hex::encode(Sha256::digest(CODE)))
            .publish()
            .is_err());
    }
    let mut invalid_crc = original;
    let end = invalid_crc.len() - 8;
    invalid_crc[end] ^= 1;
    assert!(
        Fixture::new(invalid_crc, hex::encode(Sha256::digest(CODE)))
            .publish()
            .is_err(),
        "CRC must be read even after tar EOF"
    );
}

#[test]
fn immutable_capsule_requires_real_signature_acceptance_and_installation_binding() {
    let fixture = Fixture::ordinary();
    let id = fixture.publish().expect("publication");
    let mut wrong_signature = fixture.signature.clone();
    wrong_signature[0] ^= 1;
    fixture.rewrite(&id, update::SIGNATURE_NAME, &wrong_signature);
    assert!(
        fixture.load(&id).is_err(),
        "valid bundle alone is not trusted code"
    );
    fixture.rewrite(&id, update::SIGNATURE_NAME, &fixture.signature);
    fixture.rewrite(
        &id,
        ACCEPTANCE,
        &serde_json::to_vec(&AcceptedCapsule {
            version: 1,
            installation_id: fixture.i.installation_id.clone(),
            ledger: AcceptedReleasesV1::default(),
        })
        .expect("empty ledger"),
    );
    assert!(
        fixture.load(&id).is_err(),
        "signature alone does not prove installed acceptance"
    );
    fixture.rewrite(
        &id,
        ACCEPTANCE,
        &serde_json::to_vec(&AcceptedCapsule {
            version: 1,
            installation_id: uuid::Uuid::new_v4().to_string(),
            ledger: fixture.ledger.clone(),
        })
        .expect("foreign capsule"),
    );
    assert!(
        fixture.load(&id).is_err(),
        "cross-installation capsules refuse"
    );
}

#[test]
fn executable_replacement_and_existing_corrupt_generation_are_never_repaired() {
    let fixture = Fixture::ordinary();
    let id = fixture.publish().expect("publication");
    let held = fixture.load(&id).expect("held identity");
    fixture.rewrite(&id, EXECUTABLE, CODE);
    mode(
        &fixture
            .generation(&id)
            .open(EXECUTABLE)
            .expect("replaced file"),
        0o555,
    )
    .expect("execute");
    assert!(
        held.revalidate().is_err(),
        "identical bytes in a replaced inode refuse"
    );
    fixture.rewrite(&id, EXECUTABLE, b"changed code");
    mode(
        &fixture
            .generation(&id)
            .open(EXECUTABLE)
            .expect("changed file"),
        0o555,
    )
    .expect("execute");
    assert!(fixture.load(&id).is_err());
    assert!(
        fixture.publish().is_err(),
        "never repair corrupt content-addressed generations"
    );
    assert_eq!(
        fs::read(held.executable()).expect("retained changed bytes"),
        b"changed code"
    );
}

#[test]
fn symlinks_and_unknown_destination_are_not_followed_or_replaced() {
    let fixture = Fixture::ordinary();
    let id = fixture.publish().expect("publication");
    let dir = fixture.generation(&id);
    dir_mode(&dir, 0o755).expect("owner write");
    dir.remove(EXECUTABLE).expect("remove fixture exe");
    symlink(
        "/bin/sh",
        fixture
            .i
            .paths
            .runtime
            .join(STORE)
            .join(GENERATIONS)
            .join(&id)
            .join(EXECUTABLE),
    )
    .expect("substitution");
    dir_mode(&dir, 0o555).expect("readonly");
    assert!(fixture.load(&id).is_err(), "nofollow every executable leaf");
    let root = fixture._scratch.dir();
    root.child("incoming", true).expect("incoming");
    root.child("existing", true).expect("unknown existing");
    assert!(
        rename_new_directory(
            &root,
            &CString::new("incoming").expect("name"),
            &CString::new("existing").expect("name")
        )
        .is_err(),
        "never replace even an empty unknown destination"
    );
}

#[test]
fn fresh_native_upgrade_needs_no_helper_and_pending_intent_fences_every_object_type() {
    let fixture = Fixture::ordinary();
    assert!(!legacy_present(&fixture.i).expect("fresh native"));
    require_no_pending_migration(&fixture.i).expect("no operation");
    let data = secure_fs::open_dir(&fixture.i.paths.data, fixture.i.service_uid).expect("data");
    data.write("hiqlite-activated.json", b"retained breadcrumb", 0o400)
        .expect("breadcrumb");
    assert!(legacy_present(&fixture.i).expect("legacy evidence"));
    data.write("storage-mode.json", br#"{"mode":"sqlite"}"#, 0o400)
        .expect("native binding");
    assert!(!legacy_present(&fixture.i).expect("retained breadcrumb is not a helper trigger"));
    data.write(
        "storage-migration.json",
        b"malformed retained operation",
        0o400,
    )
    .expect("intent");
    assert_eq!(
        require_no_pending_migration(&fixture.i)
            .expect_err("pending operation")
            .code,
        ErrorCode::RecoveryRequired
    );
    data.remove("storage-migration.json")
        .expect("remove test intent");
    symlink(
        "/missing",
        fixture.i.paths.data.join("storage-migration.json"),
    )
    .expect("intent symlink");
    assert_eq!(
        require_no_pending_migration(&fixture.i)
            .expect_err("never follow intent symlink")
            .code,
        ErrorCode::RecoveryRequired
    );
}
