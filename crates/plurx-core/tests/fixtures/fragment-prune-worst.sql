INSERT INTO libraries (id,name,kind,paths) VALUES (991,'prune budget','movies','[]');
INSERT INTO items (id,library_id,kind,title,sort_title) VALUES (991,991,'movie','budget','budget');
WITH RECURSIVE rows(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM rows WHERE id<4000)
INSERT INTO files(id,item_id,path,size,mtime) SELECT id,991,'/prune-budget-'||id||'.mkv',100,10 FROM rows;
INSERT INTO cluster_fragment_index_jobs
(cache_key,target_node_id,file_id,source_size,source_mtime,source_sha256,pipeline_sha256,state,not_before_ms,created_at_ms,updated_at_ms)
SELECT 'retained-'||id,'node',id,100,10,'source','pipeline','failed',1,1,1 FROM files WHERE item_id=991;
INSERT INTO analysis_requests
(request_id,file_id,source_size,source_mtime,component,force_rebuild,target_node_id,state,not_before_ms,created_at_ms,updated_at_ms,result_cache_key)
SELECT 'unrelated-'||id,id,100,10,'fragment_index',0,'node','failed',1,1,1,'unrelated-'||id FROM files WHERE item_id=991;
