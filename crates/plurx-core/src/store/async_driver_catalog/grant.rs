//! Child-side loader for the closed `LocalPhysicalGrant1` descriptor.
//!
//! The parent deducts every delegated allocation in its one ledger before the
//! spawn and passes a canonical, private, read-only descriptor plus the already
//! held lease descriptor. This child view contains ONLY those allocations and
//! read-only attachments; it never reconstructs the full policy capacity. The
//! grant is local and transient: it is not an uploaded receipt, a ResourcePlan
//! field or proof credit.
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{canonical, closed_fields, digest, number, strict_json, CatalogError, CatalogResult};

pub const ENV_GRANT_FD: &str = "PLURX_ASYNC_PHYSICAL_GRANT_FD";
pub const ENV_GRANT_SHA256: &str = "PLURX_ASYNC_PHYSICAL_GRANT_SHA256";
pub const ENV_LEASE_FD: &str = "PLURX_ASYNC_PHYSICAL_LEASE_FD";
const MAXIMUM_GRANT_BYTES: u64 = 4 * 1024 * 1024;
const FAMILIES: [(&str, &str); 8] = [
    ("catalog_main", "catalog"),
    ("verifier_main", "verifier"),
    ("catalog_export_scratch", "catalog_export"),
    ("verifier_continuation_scratch", "verifier_continuation"),
    ("catalog_or_verifier_restore_scratch", "snapshot_restore"),
    ("publication_pack_scratch", "snapshot_transport"),
    ("authentication_index", "provenance"),
    ("publication_pending_scratch", "session_controller"),
];
const POOLS: [(&str, &str); 6] = [
    ("snapshot_spool_bytes", "snapshot_transport"),
    ("restore_staging_bytes", "snapshot_restore"),
    ("transfer_staging_bytes", "snapshot_transport"),
    ("cas_bytes", "provenance"),
    ("source_runtime_scratch_bytes", "session_controller"),
    ("control_bytes", "session_controller"),
];
const COMPANIONS: [&str; 3] = ["-wal", "-shm", "-journal"];

fn refuse<T>(message: &str) -> CatalogResult<T> {
    Err(CatalogError::SourceMismatch(format!(
        "physical grant: {message}"
    )))
}

/// One allocation or read-only attachment of the descriptor.
#[derive(Clone, Debug)]
pub struct GrantEntry {
    pub reservation_id: String,
    pub resource: String,
    pub component: String,
    pub bytes: u64,
    pub relative_path: PathBuf,
    pub recursive: bool,
}
impl GrantEntry {
    #[cfg(test)]
    pub(super) fn for_test(resource: &str, component: &str, bytes: u64, path: &str) -> Self {
        Self {
            reservation_id: "c".repeat(64),
            resource: resource.to_owned(),
            component: component.to_owned(),
            bytes,
            relative_path: PathBuf::from(path),
            recursive: false,
        }
    }
    /// Owned-root-relative claims; a database's fixed companions are resolved
    /// inside its family and anonymous FILE temporaries own no addressable file.
    fn claims(&self) -> Vec<(PathBuf, bool)> {
        let is_family = FAMILIES.iter().any(|(name, _)| *name == self.resource);
        if is_family && self.component == "journal" {
            return COMPANIONS
                .iter()
                .map(|suffix| {
                    let mut name = self.relative_path.as_os_str().to_os_string();
                    name.push(suffix);
                    (PathBuf::from(name), false)
                })
                .collect();
        }
        if is_family && self.component == "temp" && !self.recursive {
            return Vec::new();
        }
        vec![(self.relative_path.clone(), self.recursive)]
    }
}

/// What the child independently expects the descriptor to be bound to.
pub struct GrantExpectation<'a> {
    pub source: &'a Value,
    pub session_id: &'a str,
    pub resource_plan_sha256: &'a str,
    /// The plan's physical policy; allocations may not exceed it.
    pub policy: &'a Value,
}

#[derive(Clone, Debug)]
pub struct ChildGrant {
    grant_id: String,
    sha256: String,
    owned_root: PathBuf,
    allocations: Vec<GrantEntry>,
    attachments: Vec<GrantEntry>,
}

fn safe_relative(text: &str) -> CatalogResult<PathBuf> {
    let parts: Vec<&str> = text.split('/').collect();
    let segment = |part: &&str| {
        let mut chars = part.chars();
        part.len() <= 128
            && chars
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if text.is_empty() || text.len() > 1024 || parts.len() > 32 || !parts.iter().all(segment) {
        return refuse("unsafe relative path");
    }
    Ok(PathBuf::from(text))
}
fn related(a: &Path, a_tree: bool, b: &Path, b_tree: bool) -> bool {
    a == b || (a_tree && b.starts_with(a)) || (b_tree && a.starts_with(b))
}
fn overlap(left: &[(PathBuf, bool)], right: &[(PathBuf, bool)]) -> bool {
    left.iter()
        .any(|(a, at)| right.iter().any(|(b, bt)| related(a, *at, b, *bt)))
}
fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
/// Linux process start identity (field 22 of /proc/<pid>/stat).
pub fn process_start_ticks(pid: u64) -> CatalogResult<u64> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|_| CatalogError::SourceMismatch("physical grant: parent is not live".into()))?;
    let tail = raw
        .rfind(')')
        .map(|at| &raw[at + 1..])
        .ok_or_else(|| CatalogError::SourceMismatch("physical grant: parent stat".into()))?;
    tail.split_whitespace()
        .nth(19)
        .and_then(|ticks| ticks.parse().ok())
        .ok_or_else(|| CatalogError::SourceMismatch("physical grant: parent start ticks".into()))
}
fn effective_uid() -> CatalogResult<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

fn entry(value: &Value, attachment: bool, expected_policy: &Value) -> CatalogResult<GrantEntry> {
    let mut fields = vec![
        "reservation_id",
        "owner",
        "resource",
        "component",
        "bytes",
        "relative_path",
        "recursive",
    ];
    if attachment {
        fields.push("access");
    }
    closed_fields(value, &fields)?;
    let text = |field: &str| {
        value[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| CatalogError::SourceMismatch(format!("physical grant: {field}")))
    };
    let (resource, component, owner) = (text("resource")?, text("component")?, text("owner")?);
    let id = text("reservation_id")?;
    let family = FAMILIES.iter().find(|(name, _)| *name == resource);
    let pool = POOLS.iter().find(|(name, _)| *name == resource);
    let expected_owner = family.or(pool).map(|(_, owner)| *owner);
    let valid_component = match (family, pool) {
        (Some(_), _) => matches!(component.as_str(), "main" | "journal" | "temp"),
        (_, Some(_)) => component == "bytes",
        _ => false,
    };
    let recursive = value["recursive"]
        .as_bool()
        .ok_or_else(|| CatalogError::SourceMismatch("physical grant: recursive flag".into()))?;
    if !hex64(&id)
        || expected_owner != Some(owner.as_str())
        || !valid_component
        || (family.is_some() && matches!(component.as_str(), "main" | "journal") && recursive)
        || (attachment && value["access"] != "read_only")
    {
        return refuse("unknown owner/component or malformed entry");
    }
    let bytes = number(value, "bytes")?;
    let limit = if family.is_some() {
        expected_policy["families"][resource.as_str()][format!("{component}_bytes")].as_u64()
    } else {
        expected_policy["pools"][resource.as_str()].as_u64()
    };
    if bytes == 0 || limit.is_none_or(|limit| bytes > limit) {
        return refuse("allocation exceeds the immutable policy");
    }
    Ok(GrantEntry {
        reservation_id: id,
        resource,
        component,
        bytes,
        relative_path: safe_relative(&text("relative_path")?)?,
        recursive,
    })
}

impl ChildGrant {
    /// Load the grant named by the three source-owned environment variables.
    pub fn from_environment(expected: &GrantExpectation<'_>) -> CatalogResult<Self> {
        let var = |name: &str| {
            std::env::var(name).map_err(|_| {
                CatalogError::ResourceIncomplete(format!(
                    "allocation-capable child requires explicit physical grant variable {name}"
                ))
            })
        };
        let descriptor: i32 = var(ENV_GRANT_FD)?
            .parse()
            .map_err(|_| CatalogError::SourceMismatch("physical grant fd is not decimal".into()))?;
        let lease: i32 = var(ENV_LEASE_FD)?
            .parse()
            .map_err(|_| CatalogError::SourceMismatch("physical lease fd is not decimal".into()))?;
        Self::load(descriptor, &var(ENV_GRANT_SHA256)?, lease, expected)
    }

    /// Verify every ABI check; the result is all the capacity this child owns.
    pub fn load(
        grant_fd: i32,
        expected_sha256: &str,
        lease_fd: i32,
        expected: &GrantExpectation<'_>,
    ) -> CatalogResult<Self> {
        if grant_fd < 0 || lease_fd < 0 || grant_fd == lease_fd || !hex64(expected_sha256) {
            return refuse("invalid inherited descriptors or hash");
        }
        // Re-open the inherited descriptor by its /proc name: an independent
        // read-only description at offset zero over the same inode.
        let flags = fs::read_to_string(format!("/proc/self/fdinfo/{grant_fd}"))
            .ok()
            .and_then(|info| {
                info.lines()
                    .find_map(|line| line.strip_prefix("flags:").map(|v| v.trim().to_owned()))
            })
            .and_then(|octal| i64::from_str_radix(&octal, 8).ok())
            .ok_or_else(|| CatalogError::SourceMismatch("physical grant fd flags".into()))?;
        if flags & 3 != 0 {
            return refuse("descriptor is writable");
        }
        let mut file = fs::File::open(format!("/proc/self/fd/{grant_fd}"))?;
        let info = file.metadata()?;
        if !info.is_file()
            || info.nlink() != 1
            || info.mode() & 0o077 != 0
            || info.uid() != effective_uid()?
            || info.len() > MAXIMUM_GRANT_BYTES
        {
            return refuse("descriptor is not a private regular file");
        }
        let mut raw = Vec::new();
        file.by_ref()
            .take(MAXIMUM_GRANT_BYTES + 1)
            .read_to_end(&mut raw)?;
        if digest(&raw) != expected_sha256 {
            return refuse("descriptor hash differs");
        }
        let value = strict_json(&raw)?;
        if canonical(&value).as_bytes() != raw.as_slice() {
            return refuse("descriptor is not canonical");
        }
        closed_fields(
            &value,
            &[
                "schema_version",
                "kind",
                "grant_id",
                "source",
                "session_id",
                "resource_plan_sha256",
                "physical_policy_sha256",
                "owned_root",
                "lease",
                "parent",
                "allocations",
                "attachments",
            ],
        )?;
        if value["schema_version"] != 1
            || value["kind"] != "local_physical_grant"
            || !value["grant_id"].as_str().is_some_and(hex64)
            || value["source"] != *expected.source
            || value["session_id"] != expected.session_id
            || value["resource_plan_sha256"] != expected.resource_plan_sha256
            || value["physical_policy_sha256"] != digest(canonical(expected.policy).as_bytes())
        {
            return refuse("source/session/plan/policy binding differs");
        }
        let (owned, lease, parent) = (&value["owned_root"], &value["lease"], &value["parent"]);
        closed_fields(owned, &["path", "device", "inode"])?;
        closed_fields(lease, &["relative_path", "device", "inode"])?;
        closed_fields(parent, &["pid", "start_ticks"])?;
        if process_start_ticks(number(parent, "pid")?)? != number(parent, "start_ticks")? {
            return refuse("parent is not the live identity");
        }
        let root = PathBuf::from(
            owned["path"]
                .as_str()
                .ok_or_else(|| CatalogError::SourceMismatch("physical grant: root path".into()))?,
        );
        let named = fs::symlink_metadata(&root)?;
        if !root.is_absolute()
            || !named.is_dir()
            || (named.dev(), named.ino()) != (number(owned, "device")?, number(owned, "inode")?)
        {
            return refuse("owned root device/inode differs");
        }
        let lease_relative = safe_relative(lease["relative_path"].as_str().unwrap_or_default())?;
        let held = fs::metadata(format!("/proc/self/fd/{lease_fd}"))?;
        let by_name = fs::symlink_metadata(root.join(&lease_relative))?;
        let recorded = (number(lease, "device")?, number(lease, "inode")?);
        if !held.is_file()
            || !by_name.is_file()
            || (held.dev(), held.ino()) != recorded
            || (by_name.dev(), by_name.ino()) != recorded
        {
            return refuse("inherited lease device/inode/path differs");
        }
        let mut allocations = Vec::new();
        let mut attachments = Vec::new();
        let mut identifiers = BTreeSet::new();
        for (list, attachment, target) in [
            (&value["allocations"], false, &mut allocations),
            (&value["attachments"], true, &mut attachments),
        ] {
            for item in list
                .as_array()
                .ok_or_else(|| CatalogError::SourceMismatch("physical grant: entry list".into()))?
            {
                let parsed = entry(item, attachment, expected.policy)?;
                if !identifiers.insert(parsed.reservation_id.clone()) {
                    return refuse("duplicate reservation id");
                }
                target.push(parsed);
            }
        }
        if allocations.is_empty() && attachments.is_empty() {
            return refuse("empty grant");
        }
        let claimed: Vec<Vec<(PathBuf, bool)>> =
            allocations.iter().map(GrantEntry::claims).collect();
        for (index, left) in claimed.iter().enumerate() {
            if claimed[index + 1..]
                .iter()
                .any(|right| overlap(left, right))
                || left.iter().any(|(path, _)| *path == lease_relative)
            {
                return refuse("overlapping allocations or lease alias");
            }
        }
        if attachments
            .iter()
            .any(|a| claimed.iter().any(|c| overlap(&a.claims(), c)))
        {
            return refuse("attachment aliases a writable allocation");
        }
        Ok(Self {
            grant_id: value["grant_id"].as_str().unwrap_or_default().to_owned(),
            sha256: expected_sha256.to_owned(),
            owned_root: root,
            allocations,
            attachments,
        })
    }

    /// A verified-shape grant for tests of the narrowed views (no descriptor I/O).
    #[cfg(test)]
    pub(super) fn for_test(
        owned_root: &Path,
        allocations: Vec<GrantEntry>,
        attachments: Vec<GrantEntry>,
    ) -> Self {
        Self {
            grant_id: "a".repeat(64),
            sha256: "b".repeat(64),
            owned_root: owned_root.to_path_buf(),
            allocations,
            attachments,
        }
    }
    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn owned_root(&self) -> &Path {
        &self.owned_root
    }
    /// Sum of the granted bytes for one component; `None` when never granted.
    pub fn limit(&self, resource: &str, component: &str) -> Option<u64> {
        let mut found = false;
        let mut total = 0u64;
        for allocation in &self.allocations {
            if allocation.resource == resource && allocation.component == component {
                found = true;
                total = total.saturating_add(allocation.bytes);
            }
        }
        found.then_some(total)
    }
    /// The read-only attachment covering `path`, if any.
    pub fn attachment_for(&self, path: &Path) -> Option<&GrantEntry> {
        let relative = path.strip_prefix(&self.owned_root).ok()?;
        self.attachments.iter().find(|a| {
            relative == a.relative_path || (a.recursive && relative.starts_with(&a.relative_path))
        })
    }
    /// Is `path` inside a writable allocation of `resource`/`component`?
    pub fn allocation_covers(&self, resource: &str, component: &str, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.owned_root) else {
            return false;
        };
        self.allocations
            .iter()
            .filter(|a| a.resource == resource && a.component == component)
            .any(|a| {
                a.claims().iter().any(|(claimed, tree)| {
                    relative == claimed || (*tree && relative.starts_with(claimed))
                }) || (!a.recursive && relative == a.relative_path)
            })
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    use serde_json::json;

    use super::*;

    struct Fixture {
        directory: tempfile::TempDir,
        policy: Value,
        source: Value,
        session: String,
        plan: String,
    }
    impl Fixture {
        fn new() -> Self {
            let config = super::super::fixture_configuration(PathBuf::from("/nonexistent"));
            let limits = super::super::ResourceLimits::from_plan(&config.resource_plan)
                .expect("fixture limits");
            Self {
                directory: tempfile::tempdir().expect("owned root"),
                policy: limits.physical_policy.clone(),
                source: serde_json::to_value(&config.source).expect("source"),
                session: config.session_id.to_string(),
                plan: config.resource_plan.sha256(),
            }
        }
        fn root(&self) -> PathBuf {
            self.directory
                .path()
                .canonicalize()
                .expect("canonical root")
        }
        fn lease(&self) -> fs::File {
            let path = self.root().join("lease");
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .expect("lease file");
            fs::File::open(path).expect("held lease")
        }
        fn entry(id: char, resource: &str, component: &str, bytes: u64, path: &str) -> Value {
            let owner = FAMILIES
                .iter()
                .chain(POOLS.iter())
                .find(|(name, _)| *name == resource)
                .map_or("unknown", |(_, owner)| *owner);
            json!({"reservation_id":id.to_string().repeat(64),"owner":owner,"resource":resource,
                "component":component,"bytes":bytes,"relative_path":path,"recursive":false})
        }
        fn descriptor(
            &self,
            lease: &fs::File,
            allocations: Vec<Value>,
            attachments: Vec<Value>,
        ) -> Value {
            let root = fs::symlink_metadata(self.root()).expect("root metadata");
            let held = lease.metadata().expect("lease metadata");
            let me = u64::from(std::process::id());
            json!({"schema_version":1,"kind":"local_physical_grant","grant_id":"a".repeat(64),
                "source":self.source,"session_id":self.session,"resource_plan_sha256":self.plan,
                "physical_policy_sha256":digest(canonical(&self.policy).as_bytes()),
                "owned_root":{"path":self.root(),"device":root.dev(),"inode":root.ino()},
                "lease":{"relative_path":"lease","device":held.dev(),"inode":held.ino()},
                "parent":{"pid":me,"start_ticks":process_start_ticks(me).expect("ticks")},
                "allocations":allocations,"attachments":attachments})
        }
        fn write(&self, value: &Value, mode: u32) -> (fs::File, String) {
            let text = canonical(value);
            let path = self.root().join(format!("grant-{mode:o}.json"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(mode)
                .open(&path)
                .expect("descriptor");
            std::io::Write::write_all(&mut file, text.as_bytes()).expect("write descriptor");
            (
                fs::File::open(path).expect("read-only descriptor"),
                digest(text.as_bytes()),
            )
        }
        fn load(
            &self,
            descriptor: &fs::File,
            sha: &str,
            lease: &fs::File,
        ) -> CatalogResult<ChildGrant> {
            ChildGrant::load(
                descriptor.as_raw_fd(),
                sha,
                lease.as_raw_fd(),
                &GrantExpectation {
                    source: &self.source,
                    session_id: &self.session,
                    resource_plan_sha256: &self.plan,
                    policy: &self.policy,
                },
            )
        }
        fn try_mutation(&self, mutate: impl FnOnce(&mut Value)) -> CatalogResult<ChildGrant> {
            let lease = self.lease();
            let mut value = self.descriptor(
                &lease,
                vec![Self::entry(
                    '1',
                    "catalog_main",
                    "main",
                    1024 * 1024,
                    "out/catalog.sqlite",
                )],
                Vec::new(),
            );
            mutate(&mut value);
            let (file, sha) = self.write(&value, 0o600);
            self.load(&file, &sha, &lease)
        }
    }

    #[test]
    fn exact_grant_loads_and_exposes_only_granted_limits() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let value = fixture.descriptor(
            &lease,
            vec![
                Fixture::entry(
                    '1',
                    "catalog_main",
                    "main",
                    4 * 1024 * 1024,
                    "out/catalog.sqlite",
                ),
                Fixture::entry(
                    '2',
                    "catalog_main",
                    "journal",
                    9 * 1024 * 1024,
                    "out/catalog.sqlite",
                ),
            ],
            vec![{
                let mut item = Fixture::entry(
                    '3',
                    "restore_staging_bytes",
                    "bytes",
                    8 * 1024 * 1024,
                    "stage/catalog.sqlite",
                );
                item["access"] = json!("read_only");
                item
            }],
        );
        let (file, sha) = fixture.write(&value, 0o600);
        let grant = fixture.load(&file, &sha, &lease).expect("verified grant");
        assert_eq!(grant.limit("catalog_main", "main"), Some(4 * 1024 * 1024));
        assert_eq!(
            grant.limit("catalog_main", "journal"),
            Some(9 * 1024 * 1024)
        );
        // Never granted: absent, not the policy's remaining capacity.
        assert_eq!(grant.limit("catalog_main", "temp"), None);
        assert_eq!(grant.limit("verifier_main", "main"), None);
        assert!(grant
            .attachment_for(&fixture.root().join("stage/catalog.sqlite"))
            .is_some());
        assert!(grant.allocation_covers(
            "catalog_main",
            "main",
            &fixture.root().join("out/catalog.sqlite")
        ));
        assert!(!grant.allocation_covers(
            "catalog_main",
            "main",
            &fixture.root().join("out/other.sqlite")
        ));
    }

    #[test]
    fn descriptor_hash_must_match_the_inherited_bytes() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let value = fixture.descriptor(
            &lease,
            vec![Fixture::entry(
                '1',
                "catalog_main",
                "main",
                1024,
                "out/catalog.sqlite",
            )],
            Vec::new(),
        );
        let (file, _) = fixture.write(&value, 0o600);
        assert!(fixture.load(&file, &"0".repeat(64), &lease).is_err());
    }

    #[test]
    fn descriptor_must_be_private_and_not_group_readable() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let value = fixture.descriptor(
            &lease,
            vec![Fixture::entry(
                '1',
                "catalog_main",
                "main",
                1024,
                "out/catalog.sqlite",
            )],
            Vec::new(),
        );
        let (file, sha) = fixture.write(&value, 0o640);
        assert!(fixture.load(&file, &sha, &lease).is_err());
    }

    #[test]
    fn writable_descriptor_is_refused() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let value = fixture.descriptor(
            &lease,
            vec![Fixture::entry(
                '1',
                "catalog_main",
                "main",
                1024,
                "out/catalog.sqlite",
            )],
            Vec::new(),
        );
        let (_, sha) = fixture.write(&value, 0o600);
        let writable = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.root().join("grant-600.json"))
            .expect("writable handle");
        assert!(fixture.load(&writable, &sha, &lease).is_err());
    }

    #[test]
    fn noncanonical_descriptor_bytes_are_refused() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let value = fixture.descriptor(
            &lease,
            vec![Fixture::entry(
                '1',
                "catalog_main",
                "main",
                1024,
                "out/catalog.sqlite",
            )],
            Vec::new(),
        );
        let spaced = serde_json::to_string_pretty(&value).expect("pretty");
        let path = fixture.root().join("pretty.json");
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, spaced.as_bytes()))
            .expect("write pretty");
        let file = fs::File::open(&path).expect("open pretty");
        assert!(fixture
            .load(&file, &digest(spaced.as_bytes()), &lease)
            .is_err());
    }

    #[test]
    fn binding_parent_root_and_lease_identity_are_all_checked() {
        let fixture = Fixture::new();
        assert!(fixture.try_mutation(|_| {}).is_ok());
        assert!(fixture
            .try_mutation(|v| v["session_id"] = json!("other"))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["resource_plan_sha256"] = json!("0".repeat(64)))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["physical_policy_sha256"] = json!("0".repeat(64)))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["source"] = json!({"other":1}))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["parent"]["start_ticks"] = json!(1))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["parent"]["pid"] = json!(4_000_000_000u64))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["owned_root"]["inode"] = json!(1))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["lease"]["inode"] = json!(1))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["lease"]["relative_path"] = json!("elsewhere"))
            .is_err());
    }

    #[test]
    fn inherited_lease_must_be_the_recorded_object() {
        let fixture = Fixture::new();
        let lease = fixture.lease();
        let value = fixture.descriptor(
            &lease,
            vec![Fixture::entry(
                '1',
                "catalog_main",
                "main",
                1024,
                "out/catalog.sqlite",
            )],
            Vec::new(),
        );
        let (file, sha) = fixture.write(&value, 0o600);
        let other = std::fs::File::open(fixture.root().join("grant-600.json")).expect("other file");
        assert!(fixture.load(&file, &sha, &other).is_err());
    }

    #[test]
    fn limits_owners_components_duplicates_and_overlaps_are_refused() {
        let fixture = Fixture::new();
        let policy_main = fixture.policy["families"]["catalog_main"]["main_bytes"]
            .as_u64()
            .expect("main");
        let too_big = |v: &mut Value| v["allocations"][0]["bytes"] = json!(policy_main + 1);
        assert!(fixture.try_mutation(too_big).is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"][0]["owner"] = json!("verifier"))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"][0]["component"] = json!("bytes"))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"][0]["resource"] = json!("unknown_pool"))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"][0]["relative_path"] = json!("../escape"))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"][0]["bytes"] = json!(0))
            .is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"][0]["reservation_id"] = json!("short"))
            .is_err());
        let duplicate = |v: &mut Value| {
            let mut second =
                Fixture::entry('1', "verifier_main", "main", 1024, "out/verifier.sqlite");
            second["reservation_id"] = v["allocations"][0]["reservation_id"].clone();
            v["allocations"].as_array_mut().expect("list").push(second);
        };
        assert!(fixture.try_mutation(duplicate).is_err());
        let overlap = |v: &mut Value| {
            let alias = Fixture::entry(
                '2',
                "snapshot_spool_bytes",
                "bytes",
                1024,
                "out/catalog.sqlite",
            );
            v["allocations"].as_array_mut().expect("list").push(alias);
        };
        assert!(fixture.try_mutation(overlap).is_err());
        let lease_alias = |v: &mut Value| {
            let alias = Fixture::entry('3', "snapshot_spool_bytes", "bytes", 1024, "lease");
            v["allocations"].as_array_mut().expect("list").push(alias);
        };
        assert!(fixture.try_mutation(lease_alias).is_err());
        let attachment_alias = |v: &mut Value| {
            let mut attach = Fixture::entry(
                '4',
                "restore_staging_bytes",
                "bytes",
                1024,
                "out/catalog.sqlite",
            );
            attach["access"] = json!("read_only");
            v["attachments"].as_array_mut().expect("list").push(attach);
        };
        assert!(fixture.try_mutation(attachment_alias).is_err());
        assert!(fixture
            .try_mutation(|v| v["allocations"] = json!([]))
            .is_err());
    }
}
