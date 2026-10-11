-- Candidate only. The qualified coordinator must provision the random purpose
-- key before activation; catalogue reads never initialize or replace it.
CREATE TABLE sharing_catalogue_keys (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton = 1),
    server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    revision_envelope TEXT NOT NULL,
    FOREIGN KEY(singleton) REFERENCES sharing_identity(singleton)
);
