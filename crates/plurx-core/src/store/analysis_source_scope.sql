-- Analysis hashes only the file it analyses, so it reads only the library root
-- that holds that file. The view keeps its name, so the claim, renewal and
-- coordination triggers that read it pick up the narrowed rows unchanged. A
-- path under no root keeps requiring every root, as the original view did. The
-- narrowing is background-only: foreground custody and pressure keys stay
-- library-wide and conservative. Existing reservations stay owned until their
-- ordinary fenced renewal or retirement.
DROP VIEW analysis_required_resources;
-- next statement
CREATE VIEW IF NOT EXISTS analysis_required_resources AS
SELECT DISTINCT request.request_id,
    COALESCE('source_io:' || mapping.domain_id, 'source_io') AS resource_key
FROM analysis_requests request JOIN files file ON file.id = request.file_id
JOIN items item ON item.id = file.item_id
LEFT JOIN libraries library ON library.id = item.library_id
LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
LEFT JOIN background_storage_domains mapping ON mapping.library_id = item.library_id
    AND mapping.root_path = root.value
WHERE request.component IN ('fragment_index','skip_markers')
-- A root contains the file when the path is the root or lies below it; a
-- trailing slash on the configured root is ignored, as in the media queries.
AND (file.path = rtrim(root.value, '/')
    OR substr(file.path, 1, length(rtrim(root.value, '/')) + 1) = rtrim(root.value, '/') || '/'
    OR NOT EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) holder
        WHERE file.path = rtrim(holder.value, '/')
        OR substr(file.path, 1, length(rtrim(holder.value, '/')) + 1) = rtrim(holder.value, '/') || '/'));
-- next statement
CREATE TABLE IF NOT EXISTS analysis_source_scope_schema(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    version INTEGER NOT NULL CHECK(version=1)
) STRICT;
-- next statement
INSERT INTO analysis_source_scope_schema VALUES(1,1);
