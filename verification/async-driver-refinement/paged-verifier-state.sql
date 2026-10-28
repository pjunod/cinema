-- Exact private independent-verifier state, algorithm version 1.
-- This is neither the production Store nor the authoritative catalog.
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS verifier_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),algorithm INTEGER NOT NULL CHECK(algorithm=1),source_json BLOB NOT NULL,session TEXT NOT NULL,plan TEXT NOT NULL,initial_image TEXT NOT NULL,private_commit INTEGER NOT NULL CHECK(private_commit>=0),last_result INTEGER NOT NULL CHECK(last_result>=0),cursor BLOB NOT NULL,source_closure_checkpoint TEXT);
CREATE TABLE IF NOT EXISTS unit_work(unit INTEGER PRIMARY KEY,origin BLOB NOT NULL,input_checkpoint TEXT NOT NULL,before_commit INTEGER NOT NULL,before_digest TEXT NOT NULL,before_cursor BLOB NOT NULL,first_result INTEGER NOT NULL,input_snapshot BLOB NOT NULL,finalized INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS export_evidence(scope TEXT,level INTEGER,ordinal INTEGER,payload BLOB,PRIMARY KEY(scope,level,ordinal));
CREATE TABLE IF NOT EXISTS export_result_members(unit INTEGER,sequence INTEGER,member BLOB,PRIMARY KEY(unit,sequence));
CREATE TABLE IF NOT EXISTS cache_bindings(path TEXT PRIMARY KEY,member BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS operation_state(operation TEXT PRIMARY KEY,scope BLOB NOT NULL,state BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS execution_commits(sequence INTEGER PRIMARY KEY,operation TEXT NOT NULL,before_cursor BLOB NOT NULL,after_cursor BLOB NOT NULL,input_digest TEXT NOT NULL,first_result INTEGER NOT NULL,result_count INTEGER NOT NULL CHECK(result_count>0),useful INTEGER NOT NULL CHECK(useful IN (0,1)));
CREATE TABLE IF NOT EXISTS check_results(sequence INTEGER PRIMARY KEY,claim TEXT NOT NULL,coverage BLOB NOT NULL,input_root TEXT NOT NULL,before_cursor BLOB NOT NULL,after_cursor BLOB NOT NULL,first_quantum INTEGER NOT NULL,quantum_count INTEGER NOT NULL CHECK(quantum_count>0),record BLOB,published_unit INTEGER);
CREATE TABLE IF NOT EXISTS credit_links(table_name TEXT NOT NULL,row_key BLOB NOT NULL,result_sequence INTEGER NOT NULL REFERENCES check_results(sequence),PRIMARY KEY(table_name,row_key));
CREATE TABLE IF NOT EXISTS inventory_entries(result_sequence INTEGER NOT NULL REFERENCES check_results(sequence),purpose TEXT NOT NULL CHECK(purpose IN ('inputs','outputs')),ordinal INTEGER NOT NULL,entry BLOB NOT NULL,PRIMARY KEY(result_sequence,purpose,ordinal));
CREATE TABLE IF NOT EXISTS authenticated_units(unit INTEGER PRIMARY KEY,origin BLOB NOT NULL,input_checkpoint TEXT NOT NULL,produced_checkpoint TEXT NOT NULL,progress_sha256 TEXT NOT NULL,backup_sha256 TEXT NOT NULL,result_root BLOB NOT NULL);
-- Current export paths/part assignments are relocatable external bookkeeping;
-- they grant no checked-range or TLC authority and are excluded from logical state.
CREATE TABLE IF NOT EXISTS export_members(unit INTEGER NOT NULL,path TEXT NOT NULL,kind TEXT NOT NULL,bytes INTEGER NOT NULL,digest TEXT NOT NULL,part INTEGER,PRIMARY KEY(unit,path));
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value BLOB NOT NULL);

CREATE TABLE IF NOT EXISTS input_scopes(scope TEXT PRIMARY KEY,checkpoint BLOB NOT NULL,checkpoint_sha256 TEXT NOT NULL,high_water INTEGER NOT NULL,manifest_root TEXT NOT NULL,external_inventory BLOB NOT NULL,manifest_scope TEXT,status TEXT NOT NULL,resume_cursor BLOB NOT NULL,cursor BLOB NOT NULL,owner_result INTEGER NOT NULL REFERENCES check_results(sequence));
CREATE TABLE IF NOT EXISTS input_members(scope TEXT NOT NULL,kind TEXT,key TEXT,reference BLOB NOT NULL,member_ref BLOB NOT NULL,status TEXT NOT NULL,owner_result INTEGER NOT NULL REFERENCES check_results(sequence),PRIMARY KEY(scope,kind,key));
CREATE TABLE IF NOT EXISTS input_work(scope TEXT NOT NULL,pass TEXT NOT NULL,owner_kind TEXT NOT NULL,owner_key TEXT NOT NULL,kind TEXT NOT NULL,key TEXT NOT NULL,status TEXT NOT NULL,cursor BLOB NOT NULL,owner_result INTEGER NOT NULL REFERENCES check_results(sequence),PRIMARY KEY(scope,pass,owner_kind,owner_key,kind,key));
CREATE INDEX IF NOT EXISTS input_pending ON input_work(scope,pass,owner_key,status,kind,key);
CREATE INDEX IF NOT EXISTS input_member_identity ON input_members(kind,key,status,owner_result);
CREATE TABLE IF NOT EXISTS record_members(kind TEXT,key TEXT,member_kind TEXT,member_key TEXT,PRIMARY KEY(kind,key,member_kind,member_key));
CREATE TABLE IF NOT EXISTS blob_descriptors(kind TEXT,key TEXT,digest TEXT NOT NULL,bytes INTEGER NOT NULL,member_ref BLOB NOT NULL,PRIMARY KEY(kind,key));

CREATE TABLE IF NOT EXISTS proposed_records(kind TEXT,key TEXT,payload BLOB NOT NULL,PRIMARY KEY(kind,key));
CREATE TABLE IF NOT EXISTS canonical_values(kind TEXT,canonical BLOB,key TEXT,PRIMARY KEY(kind,canonical),UNIQUE(kind,key));
CREATE TABLE IF NOT EXISTS factor_used(kind TEXT,id INTEGER,PRIMARY KEY(kind,id));
CREATE TABLE IF NOT EXISTS boundary_used(id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS boundary_owner(id INTEGER PRIMARY KEY,evidence INTEGER NOT NULL,phase TEXT NOT NULL,UNIQUE(evidence,phase));
CREATE TABLE IF NOT EXISTS records(kind TEXT,key TEXT,number INTEGER,text_key TEXT,digest TEXT NOT NULL,payload BLOB NOT NULL,PRIMARY KEY(kind,key));

CREATE UNIQUE INDEX IF NOT EXISTS record_number ON records(kind,number) WHERE number IS NOT NULL;

CREATE INDEX IF NOT EXISTS record_text ON records(kind,text_key);

CREATE TABLE IF NOT EXISTS blobs(kind TEXT,key TEXT,digest TEXT,path TEXT,bytes INTEGER,PRIMARY KEY(kind,key));

CREATE TABLE IF NOT EXISTS links(owner_kind TEXT,owner_key TEXT,target_kind TEXT,target_key TEXT,digest TEXT,PRIMARY KEY(owner_kind,owner_key,target_kind,target_key));

CREATE TABLE IF NOT EXISTS page_members(page TEXT,kind TEXT,key TEXT,PRIMARY KEY(page,kind,key));

CREATE TABLE IF NOT EXISTS page_deps(page TEXT,kind TEXT,key TEXT,digest TEXT,PRIMARY KEY(page,kind,key));

CREATE TABLE IF NOT EXISTS manifest_children(parent TEXT,child_kind TEXT,child_key TEXT,PRIMARY KEY(parent,child_kind,child_key));

CREATE TABLE IF NOT EXISTS loaded_members(path TEXT PRIMARY KEY,digest TEXT,reference BLOB);

CREATE TABLE IF NOT EXISTS authoritative(scope TEXT NOT NULL,kind TEXT,key TEXT,PRIMARY KEY(scope,kind,key));

CREATE TABLE IF NOT EXISTS external_roots(scope TEXT NOT NULL,kind TEXT,key TEXT,digest TEXT,PRIMARY KEY(scope,kind,key));

CREATE TABLE IF NOT EXISTS manifest_tally(scope TEXT NOT NULL,tally_manifest TEXT NOT NULL,kind TEXT,key TEXT,PRIMARY KEY(scope,tally_manifest,kind,key));
CREATE TABLE IF NOT EXISTS selected(scope TEXT NOT NULL,kind TEXT,key TEXT,PRIMARY KEY(scope,kind,key));

CREATE TABLE IF NOT EXISTS semantic_links(kind TEXT,key TEXT,relation TEXT,target INTEGER,PRIMARY KEY(kind,key,relation,target));

CREATE INDEX IF NOT EXISTS semantic_target ON semantic_links(relation,target,kind,key);

CREATE TABLE IF NOT EXISTS tlc_done(tag TEXT PRIMARY KEY,invocation_key TEXT NOT NULL,input_tuple BLOB NOT NULL,receipt BLOB,relative_path TEXT);

CREATE TABLE IF NOT EXISTS finite_failures(family TEXT PRIMARY KEY,product INTEGER,receipt BLOB);

CREATE TABLE IF NOT EXISTS finite_paths(scope TEXT,node INTEGER,parent INTEGER,edge INTEGER,PRIMARY KEY(scope,node));
CREATE TABLE IF NOT EXISTS finite_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,scope TEXT,node INTEGER,UNIQUE(scope,node));

CREATE TABLE IF NOT EXISTS v_stores(id INTEGER PRIMARY KEY,canonical TEXT UNIQUE,payload BLOB NOT NULL,parent INTEGER,checked INTEGER DEFAULT 0);

CREATE TABLE IF NOT EXISTS v_oracle(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,action TEXT,payload BLOB,used INTEGER DEFAULT 0,UNIQUE(src,action));

CREATE INDEX IF NOT EXISTS v_oracle_src ON v_oracle(src,id);

CREATE TABLE IF NOT EXISTS v_products(id INTEGER PRIMARY KEY,context TEXT,store INTEGER,digest TEXT,canonical TEXT UNIQUE,payload BLOB NOT NULL,goal INTEGER,capacity INTEGER,checked INTEGER DEFAULT 0,reachable INTEGER DEFAULT 0,parent INTEGER);

CREATE INDEX IF NOT EXISTS v_product_digest ON v_products(digest,canonical);

CREATE INDEX IF NOT EXISTS v_product_context ON v_products(context,id);

CREATE TABLE IF NOT EXISTS v_edges(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,branch BLOB,context TEXT);

CREATE INDEX IF NOT EXISTS v_edge_src ON v_edges(src,id);

CREATE INDEX IF NOT EXISTS v_edge_dst ON v_edges(dst,id);

CREATE TABLE IF NOT EXISTS v_roots(context TEXT PRIMARY KEY,initial INTEGER);

CREATE TABLE IF NOT EXISTS v_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,node INTEGER UNIQUE);

CREATE TABLE IF NOT EXISTS v_owned(kind TEXT,key TEXT,owner INTEGER,PRIMARY KEY(kind,key));

CREATE TABLE IF NOT EXISTS v_progress(phase TEXT PRIMARY KEY,after_id INTEGER NOT NULL,complete INTEGER DEFAULT 0);

CREATE TABLE IF NOT EXISTS v_fifo(ticket INTEGER PRIMARY KEY AUTOINCREMENT,product INTEGER UNIQUE,status INTEGER DEFAULT 0);

CREATE TABLE IF NOT EXISTS v_discovered(product INTEGER PRIMARY KEY);

CREATE TABLE IF NOT EXISTS store_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,store INTEGER UNIQUE);
CREATE TABLE IF NOT EXISTS rooted_products_checked(id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS rooted_stores(id INTEGER PRIMARY KEY);

CREATE TABLE IF NOT EXISTS restore_records(kind TEXT,key TEXT,PRIMARY KEY(kind,key));

CREATE TABLE IF NOT EXISTS tm_nodes(id INTEGER PRIMARY KEY,context TEXT,product INTEGER,history BLOB,discovered_by INTEGER,expanded INTEGER DEFAULT 0,UNIQUE(product,history));

CREATE INDEX IF NOT EXISTS tm_context ON tm_nodes(context,id);

CREATE TABLE IF NOT EXISTS tm_edges(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,product_edge INTEGER,UNIQUE(src,dst,product_edge));

CREATE INDEX IF NOT EXISTS tm_src ON tm_edges(src,id);

CREATE INDEX IF NOT EXISTS tm_dst ON tm_edges(dst,id);

CREATE TABLE IF NOT EXISTS tm_roots(context TEXT PRIMARY KEY,node INTEGER);

CREATE TABLE IF NOT EXISTS tm_views(view TEXT PRIMARY KEY,context TEXT,property TEXT,obligation BLOB);

CREATE TABLE IF NOT EXISTS tm_members(view TEXT,node INTEGER,component INTEGER,PRIMARY KEY(view,node));

CREATE INDEX IF NOT EXISTS tm_component ON tm_members(component,node);

CREATE TABLE IF NOT EXISTS graph_nodes(node INTEGER PRIMARY KEY);

CREATE TABLE IF NOT EXISTS graph_edges(edge INTEGER PRIMARY KEY,src INTEGER,dst INTEGER);

CREATE INDEX IF NOT EXISTS graph_src ON graph_edges(src,edge);

CREATE INDEX IF NOT EXISTS graph_dst ON graph_edges(dst,edge);

CREATE TABLE IF NOT EXISTS dfs_seen(node INTEGER PRIMARY KEY,finish INTEGER);

CREATE TABLE IF NOT EXISTS dfs_stack(depth INTEGER PRIMARY KEY,node INTEGER,last_edge INTEGER);

CREATE TABLE IF NOT EXISTS dfs_components(node INTEGER PRIMARY KEY,component INTEGER);

CREATE TABLE IF NOT EXISTS graph_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,node INTEGER UNIQUE);

CREATE TABLE IF NOT EXISTS tree_status(scope TEXT PRIMARY KEY,state BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS tree_certificates(scope TEXT,node INTEGER,parent_edge INTEGER,PRIMARY KEY(scope,node));
CREATE TABLE IF NOT EXISTS tree_seen(node INTEGER PRIMARY KEY,parent_edge INTEGER);

CREATE TABLE IF NOT EXISTS component_rank(component INTEGER PRIMARY KEY,rank INTEGER);

CREATE TABLE IF NOT EXISTS checked_monitors(id INTEGER PRIMARY KEY,product INTEGER,history BLOB,context TEXT,reachable INTEGER DEFAULT 0,UNIQUE(product,history));

CREATE TABLE IF NOT EXISTS checked_monitor_edges(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,pe INTEGER,UNIQUE(src,pe));

CREATE INDEX IF NOT EXISTS checked_me_src ON checked_monitor_edges(src,id);

CREATE TABLE IF NOT EXISTS monitor_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,node INTEGER UNIQUE);

CREATE TABLE IF NOT EXISTS completed_views(view TEXT PRIMARY KEY,last_id INTEGER);

CREATE TABLE IF NOT EXISTS dependency_work(kind TEXT,key TEXT,PRIMARY KEY(kind,key));

CREATE TABLE IF NOT EXISTS path_seen(node INTEGER PRIMARY KEY,parent INTEGER,edge INTEGER);

CREATE TABLE IF NOT EXISTS fair_groups(group_id INTEGER,node INTEGER,PRIMARY KEY(group_id,node));

CREATE TABLE IF NOT EXISTS fair_work(id INTEGER PRIMARY KEY AUTOINCREMENT,status INTEGER DEFAULT 0);

CREATE TABLE IF NOT EXISTS verified_scc_members(view TEXT,node INTEGER,component INTEGER,PRIMARY KEY(view,node));
CREATE TABLE IF NOT EXISTS verified_sccs(id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS verified_views(view TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS scc_check_state(id INTEGER PRIMARY KEY,state BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS view_check_state(view TEXT PRIMARY KEY,state BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS certificate_tree(src INTEGER,dst INTEGER PRIMARY KEY,edge INTEGER UNIQUE);

CREATE INDEX IF NOT EXISTS certificate_tree_src ON certificate_tree(src,edge);

-- Logical digest includes all columns of every table below in listed table/PK order.
-- Excluded disposable host paths/current external export bookkeeping only:
-- export_evidence,export_result_members,cache_bindings,loaded_members,blobs,export_members,authenticated_units.
-- Path-free input payloads, exact provenance and scoped preparation work are durable.
-- logical-order: {"authoritative":["scope","kind","key"],"blob_descriptors":["kind","key"],"boundary_owner":["id"],"boundary_used":["id"],"canonical_values":["kind","canonical"],"certificate_tree":["dst"],"check_results":["sequence"],"checked_monitor_edges":["id"],"checked_monitors":["id"],"completed_views":["view"],"component_rank":["component"],"credit_links":["table_name","row_key"],"dependency_work":["kind","key"],"dfs_components":["node"],"dfs_seen":["node"],"dfs_stack":["depth"],"execution_commits":["sequence"],"external_roots":["scope","kind","key"],"factor_used":["kind","id"],"fair_groups":["group_id","node"],"fair_work":["id"],"finite_failures":["family"],"finite_paths":["scope","node"],"finite_queue":["ticket"],"graph_edges":["edge"],"graph_nodes":["node"],"graph_queue":["ticket"],"input_members":["scope","kind","key"],"input_scopes":["scope"],"input_work":["scope","pass","owner_kind","owner_key","kind","key"],"inventory_entries":["result_sequence","purpose","ordinal"],"links":["owner_kind","owner_key","target_kind","target_key"],"manifest_children":["parent","child_kind","child_key"],"manifest_tally":["scope","tally_manifest","kind","key"],"meta":["key"],"monitor_queue":["ticket"],"operation_state":["operation"],"page_deps":["page","kind","key"],"page_members":["page","kind","key"],"path_seen":["node"],"proposed_records":["kind","key"],"record_members":["kind","key","member_kind","member_key"],"records":["kind","key"],"restore_records":["kind","key"],"rooted_products_checked":["id"],"rooted_stores":["id"],"scc_check_state":["id"],"selected":["scope","kind","key"],"semantic_links":["kind","key","relation","target"],"sqlite_sequence":["name"],"store_queue":["ticket"],"tlc_done":["tag"],"tm_edges":["id"],"tm_members":["view","node"],"tm_nodes":["id"],"tm_roots":["context"],"tm_views":["view"],"tree_certificates":["scope","node"],"tree_seen":["node"],"tree_status":["scope"],"unit_work":["unit"],"v_discovered":["product"],"v_edges":["id"],"v_fifo":["ticket"],"v_oracle":["id"],"v_owned":["kind","key"],"v_products":["id"],"v_progress":["phase"],"v_queue":["ticket"],"v_roots":["context"],"v_stores":["id"],"verified_scc_members":["view","node"],"verified_sccs":["id"],"verified_views":["view"],"verifier_meta":["singleton"],"view_check_state":["view"]}
