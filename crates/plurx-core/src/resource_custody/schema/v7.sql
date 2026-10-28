CREATE TABLE journal_meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 format_version INTEGER NOT NULL CHECK(format_version=7),
 journal_id BLOB NOT NULL CHECK(length(journal_id)=16),
 installation_id BLOB NOT NULL CHECK(length(installation_id)=16),
 node_id TEXT NOT NULL CHECK(length(CAST(node_id AS BLOB)) BETWEEN 1 AND 256),
 cluster_id TEXT NOT NULL CHECK(length(CAST(cluster_id AS BLOB)) BETWEEN 1 AND 256),
 next_sequence INTEGER NOT NULL CHECK(next_sequence>=0),
 current_owner_id BLOB REFERENCES owners(owner_id) CHECK(current_owner_id IS NULL OR length(current_owner_id)=16)
) STRICT;
CREATE TABLE owners (
 owner_id BLOB PRIMARY KEY NOT NULL CHECK(length(owner_id)=16),
 node_id TEXT NOT NULL, cluster_id TEXT NOT NULL,
 os_boot_json TEXT NOT NULL CHECK(length(CAST(os_boot_json AS BLOB))<=65536),
 daemon_process_json TEXT NOT NULL CHECK(length(CAST(daemon_process_json AS BLOB))<=65536),
 platform_domain_json TEXT NOT NULL CHECK(length(CAST(platform_domain_json AS BLOB))<=65536),
 state TEXT NOT NULL CHECK(state IN ('current','retired'))
) STRICT;
CREATE UNIQUE INDEX one_current_owner ON owners(state) WHERE state='current';
CREATE TABLE obligations (
 obligation_id BLOB PRIMARY KEY NOT NULL CHECK(length(obligation_id)=16),
 owner_id BLOB NOT NULL REFERENCES owners(owner_id),
 graph_id BLOB UNIQUE NOT NULL CHECK(length(graph_id)=16),
 allocation_epoch BLOB NOT NULL CHECK(length(allocation_epoch)=8),
 resources BLOB NOT NULL CHECK(length(resources)=56),
 trial_group_id BLOB CHECK(trial_group_id IS NULL OR length(trial_group_id)=16),
 phase TEXT NOT NULL CHECK(phase IN ('prepared','acquisition_possible','sealed','unknown_held','settled')),
 sequence INTEGER NOT NULL CHECK(sequence>=0),
 last_operation_id BLOB NOT NULL CHECK(length(last_operation_id)=16),
 manifest_json TEXT NOT NULL CHECK(length(CAST(manifest_json AS BLOB))<=65536),
 hold_reason TEXT,
 settlement_json TEXT CHECK(settlement_json IS NULL OR length(CAST(settlement_json AS BLOB))<=65536),
 CHECK((phase='settled')=(settlement_json IS NOT NULL)),
 CHECK((phase='unknown_held')=(hold_reason IS NOT NULL))
) STRICT;
CREATE TABLE family_roles (
 family_id BLOB NOT NULL CHECK(length(family_id)=16),
 role TEXT NOT NULL CHECK(role IN ('video','audio','companion')),
 obligation_id BLOB NOT NULL REFERENCES obligations(obligation_id),
 PRIMARY KEY(family_id,role)
) STRICT;
CREATE INDEX family_roles_by_obligation_role ON family_roles(obligation_id,role);
CREATE TABLE members (
 obligation_id BLOB NOT NULL REFERENCES obligations(obligation_id),
 member_id BLOB NOT NULL CHECK(length(member_id)=16),
 kind TEXT NOT NULL CHECK(kind IN ('containment','root','task','typed_resource')),
 adapter TEXT NOT NULL CHECK(adapter IN ('linux_cgroup_v2','windows_job','process_root','root_pipe_task','scratch','tuner_session','remote_attempt','source_file','vod_output_task','direct_source_file','direct_read_domain','fixed_probe_discover_source','fixed_probe_task','fixed_probe_discover_task','fixed_planning_engine_task','fixed_probe_metadata')),
 adapter_version INTEGER NOT NULL CHECK(adapter_version=1),
 phase TEXT NOT NULL CHECK(phase IN ('planned','acquisition_possible','acquired','closed')),
 identity_json TEXT NOT NULL CHECK(length(CAST(identity_json AS BLOB))<=65536),
 proof_json TEXT CHECK(proof_json IS NULL OR length(CAST(proof_json AS BLOB))<=65536),
 PRIMARY KEY(obligation_id,member_id),
 CHECK((phase='closed')=(proof_json IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX unique_member_identity ON members(member_id);
CREATE TABLE operations (
 operation_id BLOB PRIMARY KEY NOT NULL CHECK(length(operation_id)=16),
 request_json TEXT NOT NULL CHECK(length(CAST(request_json AS BLOB))<=65536),
 result_json TEXT NOT NULL CHECK(length(CAST(result_json AS BLOB))<=65536)
) STRICT;
CREATE TABLE asset_operation_membership (
 operation_id BLOB PRIMARY KEY NOT NULL REFERENCES operations(operation_id) CHECK(length(operation_id)=16),
 sequence INTEGER NOT NULL UNIQUE CHECK(sequence>0),
 kind TEXT NOT NULL CHECK(kind IN ('asset_v2','asset_v3','retirement')),
 asset_id BLOB NOT NULL CHECK(length(asset_id)=16),
 obligation_id BLOB NOT NULL CHECK(length(obligation_id)=16),
 graph_id BLOB NOT NULL CHECK(length(graph_id)=16),
 member_id BLOB CHECK(member_id IS NULL OR length(member_id)=16),
 CHECK((kind='retirement')=(member_id IS NULL))
) STRICT;
CREATE INDEX asset_operation_by_asset_sequence ON asset_operation_membership(asset_id,sequence);
CREATE TABLE retained_asset_owners (
 obligation_id BLOB PRIMARY KEY NOT NULL REFERENCES obligations(obligation_id) CHECK(length(obligation_id)=16),
 asset_id BLOB NOT NULL UNIQUE CHECK(length(asset_id)=16),
 rendition_id TEXT NOT NULL CHECK(length(CAST(rendition_id AS BLOB)) BETWEEN 1 AND 4096),
 generation_epoch BLOB NOT NULL CHECK(length(generation_epoch)=8),
 custody_json TEXT NOT NULL CHECK(length(CAST(custody_json AS BLOB))<=65536)
) STRICT;
ALTER TABLE obligations ADD COLUMN memory_claim TEXT NOT NULL DEFAULT 'bounded_active' CHECK(memory_claim IN ('bounded_active','bounded_resident','unknown_producer','planning_metadata'));
CREATE TABLE recovered_s_holders (
 obligation_id BLOB NOT NULL REFERENCES obligations(obligation_id) CHECK(length(obligation_id)=16),
 adoption_sequence INTEGER NOT NULL CHECK(adoption_sequence>0),
 operation_id BLOB NOT NULL UNIQUE CHECK(length(operation_id)=16),
 previous_owner_id BLOB NOT NULL REFERENCES owners(owner_id) CHECK(length(previous_owner_id)=16),
 current_owner_id BLOB NOT NULL REFERENCES owners(owner_id) CHECK(length(current_owner_id)=16),
 original_s_sequence INTEGER NOT NULL CHECK(original_s_sequence>0),
 head_operation_id BLOB NOT NULL CHECK(length(head_operation_id)=16),
 head_revision INTEGER NOT NULL CHECK(head_revision>0),
 frozen_digest TEXT NOT NULL CHECK(length(frozen_digest)=64),
 PRIMARY KEY(obligation_id,adoption_sequence)
) STRICT;
CREATE TABLE recovered_r_holders (
 obligation_id BLOB NOT NULL REFERENCES obligations(obligation_id) CHECK(length(obligation_id)=16),
 adoption_sequence INTEGER NOT NULL CHECK(adoption_sequence>0),
 operation_id BLOB NOT NULL UNIQUE CHECK(length(operation_id)=16),
 previous_owner_id BLOB NOT NULL REFERENCES owners(owner_id) CHECK(length(previous_owner_id)=16),
 current_owner_id BLOB NOT NULL REFERENCES owners(owner_id) CHECK(length(current_owner_id)=16),
 original_r_sequence INTEGER NOT NULL CHECK(original_r_sequence>0),
 head_operation_id BLOB NOT NULL CHECK(length(head_operation_id)=16),
 head_revision INTEGER NOT NULL CHECK(head_revision>0),
 root_member_id BLOB NOT NULL CHECK(length(root_member_id)=16),
 root_pid INTEGER NOT NULL CHECK(root_pid>0),
 root_creation_marker TEXT NOT NULL CHECK(length(CAST(root_creation_marker AS BLOB)) BETWEEN 1 AND 256),
 PRIMARY KEY(obligation_id,adoption_sequence)
) STRICT;
PRAGMA user_version=7;
