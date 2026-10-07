-- Candidate catalogue-order maintenance, applied atomically on both backends.
-- Membership and the stored BINARY key change the revision; descriptions,
-- artwork, analysis, and filter-only changes keep live-list semantics.
INSERT INTO sharing_catalogue_revisions(library_id, order_revision)
SELECT id, 1 FROM libraries WHERE true
ON CONFLICT(library_id) DO NOTHING;
CREATE INDEX sharing_catalogue_binary_order ON items(library_id, sort_title COLLATE BINARY, id);
CREATE INDEX sharing_catalogue_children_order ON items(library_id, parent_id, printf('%010d:%010d:%s',coalesce(season_number,2147483647),coalesce(episode_number,2147483647),sort_title) COLLATE BINARY, id);
CREATE TRIGGER sharing_catalogue_library_insert AFTER INSERT ON libraries
WHEN NOT EXISTS(SELECT 1 FROM item_identity_watermark WHERE importing=1) BEGIN
    INSERT INTO sharing_catalogue_revisions(library_id, order_revision) VALUES(new.id,1);
END;
CREATE TRIGGER sharing_catalogue_item_insert AFTER INSERT ON items
WHEN NOT EXISTS(SELECT 1 FROM item_identity_watermark WHERE importing=1) BEGIN
    UPDATE sharing_catalogue_revisions SET order_revision=order_revision+1 WHERE library_id=new.library_id;
END;
CREATE TRIGGER sharing_catalogue_item_delete AFTER DELETE ON items
WHEN NOT EXISTS(SELECT 1 FROM item_identity_watermark WHERE importing=1) BEGIN
    UPDATE sharing_catalogue_revisions SET order_revision=order_revision+1 WHERE library_id=old.library_id;
END;
CREATE TRIGGER sharing_catalogue_item_move AFTER UPDATE OF library_id ON items
WHEN old.library_id != new.library_id AND NOT EXISTS(SELECT 1 FROM item_identity_watermark WHERE importing=1) BEGIN
    UPDATE sharing_catalogue_revisions SET order_revision=order_revision+1 WHERE library_id IN (old.library_id,new.library_id);
END;
CREATE TRIGGER sharing_catalogue_item_sort AFTER UPDATE OF sort_title,parent_id,kind,season_number,episode_number ON items
WHEN NOT EXISTS(SELECT 1 FROM item_identity_watermark WHERE importing=1) AND old.library_id=new.library_id AND (old.sort_title COLLATE BINARY != new.sort_title COLLATE BINARY OR old.parent_id IS NOT new.parent_id OR old.kind != new.kind OR old.season_number IS NOT new.season_number OR old.episode_number IS NOT new.episode_number) BEGIN
    UPDATE sharing_catalogue_revisions SET order_revision=order_revision+1 WHERE library_id=new.library_id;
END;
