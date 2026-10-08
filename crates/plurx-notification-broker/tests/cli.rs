// Test-only subprocess boundary follows the repository clippy.toml convention.
#![cfg_attr(test, allow(clippy::disallowed_methods))]
//! Executes the shipped CLI against synthetic files, never provider credentials.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Command};
fn key(path: &Path, byte: u8) -> std::io::Result<()> {
    std::fs::write(path, [byte; 32])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
fn manifest(
    path: &Path,
    key_path: &Path,
    generation: &str,
    proof_byte: u8,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let proof = URL_SAFE_NO_PAD.encode([proof_byte; 32]);
    let value = serde_json::json!({"version":"cinema.broker.operator.v1","generation":generation,"master_key_file":key_path,"publishers":[{"publisher_id":"f482d5c0-12a4-4b07-9082-7484b60d7c58","server_instance_id":"synthetic-home","proof_hash":hex::encode(Sha256::digest(proof.as_bytes())),"apple":null,"android":null}]});
    std::fs::write(path, serde_json::to_vec(&value)?)?;
    Ok(())
}
fn run(manifest: &Path, database: &Path, command: &str) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_plurx-notification-broker"))
        .arg("--manifest")
        .arg(manifest)
        .arg("--database")
        .arg(database)
        .arg(command)
        .output()
}
#[test]
fn actual_cli_init_never_replaces_and_explicit_restore_requires_all_rotations(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let operator = dir.path().join("operator.json");
    let master = dir.path().join("master.key");
    let database = dir.path().join("broker.sqlite");
    let generation = uuid::Uuid::new_v4().to_string();
    key(&master, 7)?;
    manifest(&operator, &master, &generation, 5)?;
    assert!(run(&operator, &database, "init")?.status.success());
    let bytes = std::fs::read(&database)?;
    assert!(!run(&operator, &database, "init")?.status.success());
    assert_eq!(std::fs::read(&database)?, bytes);
    key(&master, 8)?;
    let failed = run(&operator, &database, "serve")?;
    assert!(!failed.status.success());
    assert_eq!(
        std::str::from_utf8(&failed.stderr)?,
        "Cinema broker: restore_fence_required\n"
    );
    assert!(!run(&operator, &database, "restore-fence")?.status.success());
    let fresh = uuid::Uuid::new_v4().to_string();
    manifest(&operator, &master, &fresh, 5)?;
    assert!(!run(&operator, &database, "restore-fence")?.status.success());
    manifest(&operator, &master, &fresh, 6)?;
    assert!(run(&operator, &database, "restore-fence")?.status.success());
    let connection = rusqlite::Connection::open(&database)?;
    assert_eq!(
        connection.query_row("SELECT generation FROM marker", [], |row| row
            .get::<_, String>(0))?,
        fresh
    );
    assert_eq!(
        connection.query_row("SELECT count(*) FROM capabilities", [], |row| row
            .get::<_, i64>(0))?,
        0
    );
    Ok(())
}
