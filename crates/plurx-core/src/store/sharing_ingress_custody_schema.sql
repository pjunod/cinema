-- Physical closure is proved by the daemon; this adjunct retains exact debts.
-- No foreign key: deleting an obsolete route must not erase unresolved custody.
CREATE TABLE sharing_ingress_custody (
    principal_kind TEXT NOT NULL CHECK(principal_kind IN ('source','receiver')),
    incarnation_id TEXT NOT NULL CHECK(length(incarnation_id)=36),
    owner_identity TEXT NOT NULL CHECK(length(owner_identity)=64),
    custody_json TEXT NOT NULL CHECK(json_valid(custody_json) AND length(CAST(custody_json AS BLOB))<=65536),
    revision INTEGER NOT NULL CHECK(revision>0),
    PRIMARY KEY(principal_kind,incarnation_id)
) STRICT;
