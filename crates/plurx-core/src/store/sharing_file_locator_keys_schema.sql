-- Candidate only. Coordinated activation provisions stable sealed material;
-- file/detail reads never create, replace or rotate this key.
CREATE TABLE sharing_file_locator_keys (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton = 1),
    server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    locator_envelope TEXT NOT NULL,
    FOREIGN KEY(singleton) REFERENCES sharing_identity(singleton)
);
