INSERT INTO libraries (id,name,kind,paths) VALUES (991,'prune semantics','movies','[]');
INSERT INTO items (id,library_id,kind,title,sort_title) VALUES (991,991,'movie','semantics','semantics');
INSERT INTO files (id,item_id,path,size,mtime) VALUES (991,991,'/retention.mkv',100,10);
INSERT INTO cluster_fragment_index_jobs
(cache_key,target_node_id,file_id,source_size,source_mtime,source_sha256,pipeline_sha256,state,not_before_ms,created_at_ms,updated_at_ms) VALUES
('','node',991,100,10,'source','pipeline','failed',1,1,1),
('delete-ready','node',991,100,10,'source','pipeline','ready',1,1,1),
('delete-cancelled','node',991,100,10,'source','pipeline','cancelled',1,1,1),
('delete-obsolete','node',991,100,9,'source','pipeline','failed',1,1,1),
('delete-forced','node',991,100,10,'source','pipeline','failed',1,1,1),
('keep-current','node',991,100,10,'source','pipeline','failed',1,1,1),
('keep-wrong-force','node',991,100,10,'source','pipeline','failed',1,1,1),
('keep-recent','node',991,100,10,'source','pipeline','ready',1,1,100),
('keep-active','node',991,100,10,'source','pipeline','running',1,1,1),
('keep-active-request','node',991,100,10,'source','pipeline','ready',1,1,1),
('keep-shared-active','node',991,100,10,'source','pipeline','ready',1,1,1),
('keep-shared-active','other',991,100,10,'source','pipeline','queued',1,1,1),
('keep-location','node',991,100,10,'source','pipeline','ready',1,1,1);
INSERT INTO analysis_requests
(request_id,file_id,source_size,source_mtime,component,force_rebuild,target_node_id,state,not_before_ms,created_at_ms,updated_at_ms,result_cache_key) VALUES
('empty',991,100,10,'fragment_index',1,'node','failed',1,1,1,''),
('forced',991,100,10,'fragment_index',1,'node','failed',1,1,1,'delete-forced'),
('wrong-force',991,100,10,'fragment_index',1,'other','failed',1,1,1,'keep-wrong-force'),
('active',991,100,10,'fragment_index',0,'node','submitted',1,1,1,'keep-active-request');
INSERT INTO cluster_fragment_index_artifacts
(cache_key,file_id,source_size,source_mtime,source_sha256,pipeline_sha256,blob_sha256,bytes,built_by_node_id,built_at_ms)
VALUES ('keep-location',991,100,10,'source','pipeline','blob',128,'node',1);
INSERT INTO cluster_fragment_index_heads
(logical_cache_key,generation_cache_key,request_id,updated_at_ms)
VALUES ('logical','keep-location','request',1);
INSERT INTO cluster_fragment_index_locations
(cache_key,node_id,bytes,verified_at_ms,last_seen_at_ms) VALUES ('keep-location','node',128,1,100);
