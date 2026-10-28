-- Normative M0 schema6 harness catalog; implementation/acceptance pending.
-- This is NOT the production Store database and must never be installed in it.
-- One SQLite writer; full canonical BLOB equality, never digest-only equality.
PRAGMA foreign_keys = ON;
CREATE TABLE catalog_commits (sequence INTEGER PRIMARY KEY CHECK(sequence>=1), phase TEXT NOT NULL CHECK(phase IN ('initial','source','temporal')), unit_ordinal INTEGER NOT NULL CHECK(unit_ordinal>=0));
CREATE TABLE catalog_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), current_commit INTEGER NOT NULL REFERENCES catalog_commits(sequence), schema_version INTEGER NOT NULL CHECK(schema_version=6), source_json BLOB NOT NULL, session_id TEXT NOT NULL UNIQUE, resource_plan_sha256 TEXT NOT NULL);
CREATE TABLE resource_plans (digest TEXT PRIMARY KEY, descriptor BLOB NOT NULL, admission TEXT NOT NULL CHECK(admission='admitted'));
CREATE TABLE sessions (id TEXT PRIMARY KEY, source_json BLOB NOT NULL, initial_image_sha256 TEXT NOT NULL, resource_plan_sha256 TEXT NOT NULL REFERENCES resource_plans(digest), phase TEXT NOT NULL CHECK(phase IN ('admitted','running','checkpointed_incomplete','resource_blocked','semantic_invalid','closed_unqualified','qualified','completion_rejected','complete')), checkpoint_digest TEXT, next_unit INTEGER NOT NULL CHECK(next_unit>=0), descriptor BLOB NOT NULL);
CREATE TABLE physical_rows (id INTEGER PRIMARY KEY AUTOINCREMENT, digest BLOB NOT NULL CHECK(length(digest)=32), canonical BLOB NOT NULL UNIQUE, utf8_bytes INTEGER NOT NULL CHECK(utf8_bytes>=0));
CREATE INDEX physical_rows_digest ON physical_rows(digest);
CREATE TABLE episodes (id INTEGER PRIMARY KEY AUTOINCREMENT, digest BLOB NOT NULL CHECK(length(digest)=32), canonical BLOB NOT NULL UNIQUE, utf8_bytes INTEGER NOT NULL CHECK(utf8_bytes>=0));
CREATE INDEX episodes_digest ON episodes(digest);
CREATE TABLE boundaries (id INTEGER PRIMARY KEY AUTOINCREMENT, physical_id INTEGER NOT NULL REFERENCES physical_rows(id), episode_id INTEGER NOT NULL REFERENCES episodes(id), phase TEXT NOT NULL CHECK(phase IN ('initial','pre_apply','post_apply','post_rollback')), canonical BLOB NOT NULL, omitted BLOB NOT NULL, witness BLOB NOT NULL);
CREATE TABLE stores (id INTEGER PRIMARY KEY AUTOINCREMENT, physical_id INTEGER NOT NULL REFERENCES physical_rows(id), episode_id INTEGER NOT NULL REFERENCES episodes(id), digest BLOB NOT NULL CHECK(length(digest)=32), canonical BLOB NOT NULL UNIQUE, initial_image_sha256 TEXT NOT NULL, sqlite_schema_sha256 TEXT NOT NULL, boundary_id INTEGER NOT NULL REFERENCES boundaries(id), replay_parent_edge INTEGER REFERENCES oracle_edges(id) DEFERRABLE INITIALLY DEFERRED, certification TEXT NOT NULL CHECK(certification IN ('actual_init_checked','novel_actual_state_checked')), UNIQUE(physical_id,episode_id));
CREATE INDEX stores_digest ON stores(digest);
CREATE TABLE oracle_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, from_store INTEGER NOT NULL REFERENCES stores(id), to_store INTEGER NOT NULL REFERENCES stores(id), input BLOB NOT NULL, applied BLOB NOT NULL, production_witness BLOB NOT NULL, before_boundary INTEGER NOT NULL REFERENCES boundaries(id), after_boundary INTEGER NOT NULL REFERENCES boundaries(id), rollback_boundary INTEGER NOT NULL REFERENCES boundaries(id), UNIQUE(from_store,input));
CREATE TABLE contexts (name TEXT PRIMARY KEY CHECK(name IN ('base','stable','weak_admission','no_claim_win','withheld_join','old_poll_policy','drop_before_join')), initial_product INTEGER REFERENCES products(id) DEFERRABLE INITIALLY DEFERRED, descriptor BLOB NOT NULL);
CREATE TABLE products (id INTEGER PRIMARY KEY AUTOINCREMENT, context TEXT NOT NULL REFERENCES contexts(name), store_id INTEGER NOT NULL REFERENCES stores(id), digest BLOB NOT NULL CHECK(length(digest)=32), canonical BLOB NOT NULL, predecessor_edge INTEGER REFERENCES product_edges(id) DEFERRABLE INITIALLY DEFERRED, expanded INTEGER NOT NULL DEFAULT 0 CHECK(expanded IN (0,1)), UNIQUE(context,canonical));
CREATE INDEX products_digest ON products(context,digest);
CREATE TABLE frontier (ticket INTEGER PRIMARY KEY AUTOINCREMENT, product_id INTEGER NOT NULL UNIQUE REFERENCES products(id), status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','expanded')));
CREATE INDEX frontier_fifo ON frontier(status,ticket);
CREATE TABLE expansions (id INTEGER PRIMARY KEY AUTOINCREMENT, predecessor INTEGER NOT NULL UNIQUE REFERENCES products(id), fifo_ticket INTEGER NOT NULL UNIQUE REFERENCES frontier(ticket), unit_ordinal INTEGER NOT NULL CHECK(unit_ordinal>=0), descriptor BLOB NOT NULL, complete INTEGER NOT NULL CHECK(complete=1));
CREATE TABLE candidates (id INTEGER PRIMARY KEY AUTOINCREMENT, expansion_id INTEGER NOT NULL REFERENCES expansions(id) DEFERRABLE INITIALLY DEFERRED, ordinal INTEGER NOT NULL CHECK(ordinal>=0), descriptor BLOB NOT NULL, classification TEXT NOT NULL CHECK(classification IN ('eligible','excluded_domain','excluded_sample')), reason TEXT, UNIQUE(expansion_id,ordinal), CHECK((classification='eligible')=(reason IS NULL)));
CREATE TABLE demands (id INTEGER PRIMARY KEY AUTOINCREMENT, candidate_id INTEGER NOT NULL REFERENCES candidates(id), ordinal INTEGER NOT NULL CHECK(ordinal>=0), from_store INTEGER NOT NULL REFERENCES stores(id), role TEXT NOT NULL CHECK(role IN ('apply','capacity','dependent_resolve','dependent_read','dependent_cancel')), descriptor BLOB NOT NULL, classification TEXT NOT NULL CHECK(classification IN ('eligible','excluded_domain','excluded_sample')), outcome_id INTEGER REFERENCES oracle_edges(id), reason TEXT, UNIQUE(candidate_id,ordinal), CHECK((classification='eligible' AND outcome_id IS NOT NULL AND reason IS NULL) OR (classification!='eligible' AND outcome_id IS NULL AND reason IS NOT NULL)));
CREATE TABLE demand_dependencies (demand_id INTEGER NOT NULL REFERENCES demands(id), depends_on INTEGER NOT NULL REFERENCES demands(id), PRIMARY KEY(demand_id,depends_on));
CREATE TABLE product_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, expansion_id INTEGER NOT NULL REFERENCES expansions(id) DEFERRABLE INITIALLY DEFERRED, candidate_id INTEGER NOT NULL REFERENCES candidates(id), from_product INTEGER NOT NULL REFERENCES products(id), to_product INTEGER NOT NULL REFERENCES products(id), descriptor BLOB NOT NULL, primary_edge INTEGER REFERENCES oracle_edges(id), resolution_edge INTEGER REFERENCES oracle_edges(id), read_edge INTEGER REFERENCES oracle_edges(id), cleanup_edge INTEGER REFERENCES oracle_edges(id));
CREATE TABLE fair_pairs (context TEXT NOT NULL REFERENCES contexts(name), from_product INTEGER NOT NULL REFERENCES products(id), to_product INTEGER NOT NULL REFERENCES products(id), nonstuttering INTEGER NOT NULL CHECK(nonstuttering IN (0,1)), PRIMARY KEY(context,from_product,to_product));
CREATE TABLE fair_labels (context TEXT NOT NULL, from_product INTEGER NOT NULL, to_product INTEGER NOT NULL, label TEXT NOT NULL, PRIMARY KEY(context,from_product,to_product,label), FOREIGN KEY(context,from_product,to_product) REFERENCES fair_pairs(context,from_product,to_product));
CREATE TABLE enabledness (product_id INTEGER PRIMARY KEY REFERENCES products(id), full_graph_labels BLOB NOT NULL);
CREATE TABLE monitor_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, product_id INTEGER NOT NULL REFERENCES products(id), monitor BLOB NOT NULL, UNIQUE(product_id,monitor));
CREATE TABLE monitor_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, from_monitor INTEGER NOT NULL REFERENCES monitor_nodes(id), to_monitor INTEGER NOT NULL REFERENCES monitor_nodes(id), product_edge INTEGER NOT NULL REFERENCES product_edges(id), UNIQUE(from_monitor,to_monitor,product_edge));
CREATE TABLE components (id INTEGER PRIMARY KEY AUTOINCREMENT, context TEXT NOT NULL REFERENCES contexts(name), property TEXT NOT NULL CHECK(property IN ('seen_goal_false','pending_true','capacity_ok_pending_true')), obligation BLOB NOT NULL, view_key BLOB NOT NULL, rank INTEGER NOT NULL CHECK(rank>=0), root INTEGER NOT NULL REFERENCES monitor_nodes(id), certificate BLOB NOT NULL, UNIQUE(id,view_key));
CREATE INDEX components_view ON components(view_key);
CREATE TABLE component_members (view_key BLOB NOT NULL, monitor_id INTEGER NOT NULL REFERENCES monitor_nodes(id), component_id INTEGER NOT NULL, PRIMARY KEY(view_key,monitor_id), FOREIGN KEY(component_id,view_key) REFERENCES components(id,view_key));
CREATE TABLE final_records (kind TEXT NOT NULL CHECK(kind IN ('Counts','ContextRoot','Outcome')), record_key BLOB NOT NULL, payload BLOB NOT NULL, sealed_commit INTEGER NOT NULL REFERENCES catalog_commits(sequence), PRIMARY KEY(kind,record_key));
CREATE TABLE pages (id TEXT PRIMARY KEY, kind TEXT NOT NULL, first_key BLOB NOT NULL, last_key BLOB NOT NULL, record_count INTEGER NOT NULL CHECK(record_count>=0), bytes INTEGER NOT NULL CHECK(bytes>=0 AND bytes<=268435456), descriptor BLOB NOT NULL, previous_checkpoint TEXT, transaction_high_water INTEGER NOT NULL);
CREATE TABLE page_dependencies (page_id TEXT NOT NULL REFERENCES pages(id), target_kind TEXT NOT NULL, target_id TEXT NOT NULL, sha256 TEXT NOT NULL, PRIMARY KEY(page_id,target_kind,target_id));
CREATE TABLE work_units (ordinal INTEGER PRIMARY KEY CHECK(ordinal>=0), expected_prior_checkpoint TEXT, stage TEXT NOT NULL CHECK(stage IN ('expand','semantic_verify','monitor_scc_verify','temporal_components','backend_qualification','complete')), progress BLOB NOT NULL, registered_cases BLOB NOT NULL, source_json BLOB NOT NULL, auth_receipt BLOB, status TEXT NOT NULL CHECK(status IN ('dispatched','running','checkpointed_incomplete','failed_incomplete','closed_unqualified','qualified','complete')));
CREATE TABLE checkpoints (sequence INTEGER PRIMARY KEY CHECK(sequence>=0), digest TEXT NOT NULL UNIQUE, unit_ordinal INTEGER NOT NULL REFERENCES work_units(ordinal), descriptor BLOB NOT NULL, previous_digest TEXT, page_root TEXT NOT NULL, transaction_high_water INTEGER NOT NULL, oldest_pending_ticket INTEGER, counts BLOB NOT NULL);
CREATE TRIGGER expanded_requires_complete BEFORE UPDATE OF expanded ON products WHEN NEW.expanded=1 BEGIN SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM expansions WHERE predecessor=NEW.id AND complete=1) THEN RAISE(ABORT,'expanded predecessor lacks sealed expansion') END; END;
-- Runtime code MUST deny UPDATE/DELETE of immutable identity/evidence/page rows.
-- Atomic complete expansion: BEGIN IMMEDIATE; intern actual checked states;
-- append candidates/demands/dependencies/outcomes/all successors/fair unions;
-- insert the complete expansion and enabledness; enqueue new discoveries in
-- source order; set expanded/frontier status; COMMIT. No checkpoint mid-txn.
-- A rollback publishes no IDs/records. Private provisional handles are invalid;
-- every committed global ID is append-stable and is never recycled/deleted.
-- Every atomic initialization/source/temporal data commit appends current+1
-- to catalog_commits and advances catalog_meta.current_commit in that SAME txn.
-- One writer shares this sequence across source and temporal phases. No per-table
-- ID is a commit sequence. Controller/page bookkeeping is outside the semantic
-- logical snapshot/digest and cannot mutate a committed semantic record.
-- A read/export snapshot begins a read transaction, reads current_commit first,
-- and must equal the requested high-water. The writer is paused until close;
-- all table reads then use that exact pinned snapshot. Stale/unavailable high-
-- water is refused, never reconstructed or silently promoted to current.
-- New noninitial stores/products may have NULL creating/discovery links only
-- inside their uncommitted expansion. First appended creating/discovery edge
-- fills that provisional link. Seal rejects missing or inconsistent links;
-- committed links never change. Boundary/enabledness records append in that txn.
-- components.obligation stores canonical JSON null or the exact closed triple;
-- view_key stores full canonical [context,property,obligation], NEVER its hash.
-- Membership covers each relevant fixed-obligation induced graph independently.
-- final_records publishes only frozen Counts/ContextRoot or sealed Outcome;
-- checkpoint/controller changing snapshots are inline, never these page keys.
