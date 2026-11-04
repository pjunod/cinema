//! Native restore retires credentials whose revocation history may roll back.
//! Ciphertext and physical ingress custody remain intact. Re-pairing issues
//! new logical authority; storage generation alone cannot fence old tokens.
use crate::{error::StoreError, secrets::CredentialKey};
use rusqlite::Connection;

/// Called inside the offline restore transaction, before its qualified image
/// fingerprint and manifest are published. Never mints a key or closure proof.
pub(super) fn fence(connection: &Connection, key: &CredentialKey) -> Result<(), StoreError> {
    crate::store::sharing_purpose_keys::archive_native_for_restore(connection, key)?;
    connection.execute("INSERT INTO settings(key,value,updated_at) VALUES('sharing_enabled','false',unixepoch()) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",[])?;
    connection.execute("UPDATE sharing_exports SET state='revoked',scope_generation=scope_generation+1,mutation_generation=mutation_generation+1 WHERE state!='revoked'",[])?;
    connection.execute(
        "UPDATE sharing_invitations SET state='cancelled' WHERE state='open'",
        [],
    )?;
    connection.execute("UPDATE sharing_imports SET state='disabled',lifecycle_generation=lifecycle_generation+1 WHERE state NOT IN ('disabled','revoked')",[])?;
    connection.execute("UPDATE sharing_delivery_grants SET state='revoked'", [])?;
    // Retain ingress custody verbatim: an old image cannot prove that an
    // accepted driver closed, nor authorize adopting an earlier process.
    connection.execute("DELETE FROM sharing_identity", [])?;
    Ok(())
}
