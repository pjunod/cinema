-- Person-curated tags, saved rows, Home layouts and the grants that let a
-- non-admin curate for the household (docs/features/CUSTOM-TAGS-PLAN.md).
-- An independently recognized extension: it creates only its own objects.
CREATE TABLE library_curation_schema(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 version INTEGER NOT NULL CHECK(version=1)
) STRICT;
-- next statement
CREATE TABLE library_curation_grants(
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 grant_name TEXT NOT NULL CHECK(grant_name IN ('manage_tags')),
 granted_by INTEGER NOT NULL,
 granted_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_id,grant_name)
) STRICT;
-- next statement
CREATE TABLE library_curation_tags(
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 256),
 name_key TEXT NOT NULL,
 owner_user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
 color TEXT NOT NULL,
 suggest_terms TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(suggest_terms) AND json_type(suggest_terms)='array'),
 archived_at_ms INTEGER,
 created_by INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE UNIQUE INDEX library_curation_tags_key ON library_curation_tags(COALESCE(owner_user_id,0),name_key);
-- next statement
CREATE TABLE library_curation_item_tags(
 tag_id INTEGER NOT NULL REFERENCES library_curation_tags(id) ON DELETE CASCADE,
 item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 added_by INTEGER NOT NULL,
 added_at_ms INTEGER NOT NULL,
 source TEXT NOT NULL CHECK(source IN ('manual','suggestion')),
 PRIMARY KEY(tag_id,item_id)
) STRICT;
-- next statement
CREATE INDEX library_curation_item_tags_item ON library_curation_item_tags(item_id);
-- next statement
CREATE TABLE library_curation_rejections(
 tag_id INTEGER NOT NULL REFERENCES library_curation_tags(id) ON DELETE CASCADE,
 item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 rejected_by INTEGER NOT NULL,
 rejected_at_ms INTEGER NOT NULL,
 PRIMARY KEY(tag_id,item_id)
) STRICT;
-- next statement
CREATE INDEX library_curation_rejections_item ON library_curation_rejections(item_id);
-- next statement
CREATE TABLE library_curation_rows(
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 owner_user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
 title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 320),
 spec TEXT NOT NULL CHECK(json_valid(spec) AND json_type(spec)='object'),
 created_by INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE TABLE library_curation_home(
 user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
 layout TEXT NOT NULL CHECK(json_valid(layout) AND json_type(layout)='object'),
 updated_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
INSERT INTO library_curation_schema VALUES(1,1);
