-- Candidate only. Install after all writers advertise item_identity_v1.
CREATE TABLE item_identity_watermark (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 high_water INTEGER NOT NULL CHECK(high_water>=0),
 importing INTEGER NOT NULL DEFAULT 0 CHECK(importing IN (0,1))
) STRICT;
INSERT INTO item_identity_watermark(singleton,high_water)
SELECT 1,coalesce(max(id),0) FROM items;
CREATE TRIGGER item_identity_no_reuse BEFORE INSERT ON items
WHEN new.id <= 0 OR EXISTS(SELECT 1 FROM item_identity_watermark WHERE importing=0 AND new.id<=high_water) BEGIN
 SELECT RAISE(ABORT,'item_identity_reuse');
END;
CREATE TRIGGER item_identity_observe AFTER INSERT ON items BEGIN
 UPDATE item_identity_watermark SET high_water=max(high_water,new.id) WHERE singleton=1;
END;
