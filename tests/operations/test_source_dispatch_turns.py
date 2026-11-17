"""Exercise every grant family against the same installed SQLite triggers."""
import sqlite3
import unittest
from pathlib import Path

SCHEMA = Path(__file__).resolve().parents[2] / "crates/plurx-core/src/store/background_job_turns.sql"
DISPATCH_FIXTURE = Path(__file__).resolve().parents[2] / "crates/plurx-core/tests/fixtures/source-lease-dispatch-current.sql"

class SourceDispatchTurns(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        self.addCleanup(self.db.close)
        self.db.executescript("""
            CREATE TABLE background_jobs(id TEXT PRIMARY KEY,state,revision,kind,payload_json,fence,created_at_ms,updated_at_ms,retry_deadline_ms);
            CREATE TABLE libraries(id INTEGER PRIMARY KEY,paths);
            CREATE TABLE settings(key TEXT PRIMARY KEY,value);
            CREATE TABLE job_leases(resource TEXT PRIMARY KEY,owner_node_id,fence,revision,expires_at_ms);
            CREATE TABLE background_storage_domains(library_id,root_path,domain_id);
            CREATE TABLE background_library_requests(request_id TEXT PRIMARY KEY,job_id,input_json,result_json);
            CREATE TABLE background_job_waiters(request_id,request_scope,job_id,state,consumer_kind,deadline_ms,created_at_ms,consumer_ref,target_node_id);
            CREATE TABLE background_job_required_resources(job_id,resource_key);
            CREATE TABLE analysis_requests(request_id TEXT PRIMARY KEY,component,trigger,state,fence,created_at_ms,updated_at_ms);
            CREATE TABLE analysis_required_resources(request_id,resource_key);
            CREATE TABLE source_io_occupancy(resource_key,active_occupancy,expires_at_ms,custody_id,owner_kind,work_id,fence);
            CREATE TABLE foreground_source_permissions(resource_key,lease_expires_at_ms,custody_held,permits_through_ms);
            CREATE TABLE background_job_reservations(resource_key,job_id,expires_at_ms);
            CREATE TABLE analysis_source_reservations(resource_key,request_id,expires_at_ms);
            CREATE TABLE files(id INTEGER PRIMARY KEY,probe_json,size,mtime);
            CREATE TABLE users(id INTEGER PRIMARY KEY,is_admin);
            CREATE TABLE subtitle_translation_operations(id TEXT PRIMARY KEY,file_id,operation_epoch,stopped,state,target_node_id,preparation_job_id,source_json,profile_json,asset_id);
            CREATE TABLE subtitle_translation_demands(id TEXT PRIMARY KEY,user_id,operation_id,operation_epoch,released,expires_at_ms,position_ms,playback_rate,continuity_id);
            CREATE TABLE subtitle_translation_range_continuities(job_id,fence,demand_id,continuity_id);
            CREATE TABLE subtitle_translation_assets(id TEXT PRIMARY KEY,retired_at_ms,source_json,profile_json);
            CREATE TABLE background_source_custody(owner_key TEXT PRIMARY KEY,owner_kind,work_id,resource_keys_json,started_at_ms);
            INSERT INTO libraries VALUES(1,'["/library"]');
            INSERT INTO background_storage_domains VALUES(1,'/library','a');
            INSERT INTO analysis_requests(request_id,component,trigger,state,fence,created_at_ms,updated_at_ms) VALUES('index','fragment_index','background','queued',0,1,0);
            INSERT INTO analysis_required_resources VALUES('index','source_io:a');
        """)
        self.db.executescript(SCHEMA.read_text())

    def manual(self, name, now):
        self.db.execute("INSERT INTO background_jobs(id,state,revision,kind,payload_json,fence,created_at_ms,updated_at_ms,retry_deadline_ms) VALUES(?,'queued',0,'library_scan','{\"library_id\":1}',0,?,?,0)", (name,now,now))
        self.db.execute("INSERT INTO background_library_requests VALUES(?,?,'{\"kind\":\"full\",\"trigger\":\"manual\"}',NULL)", (name,name))
        self.db.execute("INSERT INTO background_job_waiters VALUES(?,'library',?,'pending','library_work',NULL,?,NULL,NULL)", (name,name,now))
        if self.db.execute("SELECT type FROM sqlite_master WHERE name='background_job_required_resources'").fetchone()[0] == 'table':
            self.db.execute("INSERT INTO background_job_required_resources VALUES(?,'source_io:a')", (name,))
        self.db.execute("INSERT INTO dispatch_turn_ready VALUES(?,'worker',0,?,'[\"/library\"]')", (name,now+7000))

    def analysis_claim(self, now, name="index"):
        return self.db.execute("UPDATE analysis_requests SET state='running',fence=fence+1,updated_at_ms=? WHERE request_id=? AND state='queued'", (now,name)).rowcount

    def manual_claim(self, name, now):
        return self.db.execute("UPDATE background_jobs SET state='running',fence=fence+1,updated_at_ms=? WHERE id=? AND state='queued'", (now,name)).rowcount

    def custody(self, owner, key, now):
        return self.db.execute("INSERT INTO background_source_custody VALUES(?,?,?,json_array(?),?)", (owner,owner,owner,key,now)).rowcount

    def test_all_source_grants_respect_domain_and_real_waiter(self):
        self.manual("scan", 1000)
        self.assertEqual(self.analysis_claim(1001), 0)
        self.assertEqual(self.custody("offline", "source_io", 1001), 0)
        self.assertEqual(self.custody("lease", "source_io:a", 1001), 0)
        self.assertEqual(self.custody("lease", "source_io:b", 1002), 1)
        self.db.execute("UPDATE background_job_waiters SET state='cancelled' WHERE request_id='scan'")
        self.assertEqual(self.analysis_claim(1003), 1)
        self.assertEqual(self.custody("offline", "source_io", 1003), 1)

    def test_eight_actual_starts_give_aged_analysis_the_next_turn(self):
        for i in range(8):
            now = 70000 + 10*i
            name = f"manual-{i}"
            self.manual(name, now)
            for _ in range(3):
                self.assertEqual(self.analysis_claim(now), 0)
            self.assertEqual(self.manual_claim(name, now+1), 1)
        self.manual("next", 70100)
        self.assertEqual(self.manual_claim("next", 70101), 0)
        self.assertEqual(self.analysis_claim(70102), 1)
        self.assertEqual(self.manual_claim("next", 70103), 1)

    def test_backlogged_maintenance_gets_one_turn_per_manual_burst(self):
        waiting = ["index", "index2", "index3"]
        for name in waiting[1:]:
            self.db.execute("INSERT INTO analysis_requests(request_id,component,trigger,state,fence,created_at_ms,updated_at_ms) VALUES(?,'fragment_index','background','queued',0,1,0)", (name,))
            self.db.execute("INSERT INTO analysis_required_resources VALUES(?,'source_io:a')", (name,))
        now = 70000
        for round_number in range(3):
            for i in range(8):
                name = f"manual-{round_number}-{i}"
                self.manual(name, now)
                for contender in waiting:
                    self.assertEqual(self.analysis_claim(now, contender), 0)
                self.assertEqual(self.manual_claim(name, now), 1)
                now += 1
            self.manual("after-burst-" + str(round_number), now)
            self.assertEqual(self.analysis_claim(now, waiting.pop(0)), 1)
            for contender in waiting:
                self.assertEqual(self.analysis_claim(now, contender), 0)
            # Remove this still-ready request before the next eight-grant cycle.
            self.db.execute("UPDATE background_job_waiters SET state='cancelled' WHERE request_id=?", ("after-burst-" + str(round_number),))
            now += 1

    def test_playback_analysis_may_take_its_protected_turn(self):
        self.manual("scan", 1000)
        self.db.execute("INSERT INTO background_job_waiters VALUES('viewer','playback-analysis','index','pending','viewer',2000,1000,NULL,NULL)")
        self.assertEqual(self.analysis_claim(1001), 1)

    def test_wildcard_scan_on_blocked_disk_leaves_disjoint_job_free(self):
        self.manual("scan", 1000)
        self.db.execute("DELETE FROM background_storage_domains")
        self.db.execute("INSERT INTO background_job_reservations VALUES('source_io:b','reader',9000)")
        self.db.execute("INSERT INTO source_io_occupancy VALUES('source_io:b',1,9000,NULL,'job','reader',1)")
        self.assertEqual(self.custody("lease", "source_io:a", 1001), 1)
        self.db.execute("UPDATE background_job_reservations SET expires_at_ms=1002")
        self.db.execute("UPDATE source_io_occupancy SET expires_at_ms=1002")
        self.assertEqual(self.analysis_claim(1003), 0)

    def test_other_root_respects_playback_slot_and_foreground_permission(self):
        self.manual("scan", 1000)
        self.db.executescript("""
            UPDATE libraries SET paths='["/library","/other"]';
            UPDATE dispatch_turn_ready SET roots=(SELECT paths FROM libraries WHERE id=1);
            INSERT INTO background_storage_domains VALUES(1,'/other','b');
            INSERT INTO background_job_reservations VALUES('source_io:b','reader',9000);
            INSERT INTO source_io_occupancy VALUES('source_io:b',1,9000,NULL,'job','reader',1);
            INSERT INTO background_job_waiters VALUES('viewer','playback-artifact','reader','pending','playback_fragment',9000,1,NULL,NULL);
        """)
        self.assertEqual(self.analysis_claim(1100), 0, "viewer uses protected slot on the other disk")
        self.db.execute("INSERT INTO foreground_source_permissions VALUES('source_io:b',9000,1,0)")
        self.assertEqual(self.custody("lease", "source_io:a", 1100), 1, "viewer denies additional source service")
        self.db.execute("UPDATE foreground_source_permissions SET permits_through_ms=9000")
        self.assertEqual(self.analysis_claim(1101), 0)
        self.db.execute("UPDATE source_io_occupancy SET expires_at_ms=1000,custody_id='stale'")
        self.assertEqual(self.analysis_claim(1102), 1, "expired physical custody still prevents the scan")


    def test_held_library_publication_lease_defers_only_until_expiry(self):
        self.manual("scan", 1000)
        self.db.execute("INSERT INTO job_leases VALUES('scan:library:1','old-worker',1,1,1010)")
        self.db.execute("INSERT INTO settings VALUES('internal.cluster_job_owner_removed.old-worker','true')")
        self.assertEqual(self.custody("lease", "source_io:a", 1009), 1)
        self.assertEqual(self.analysis_claim(1010), 0)
        self.db.execute("UPDATE job_leases SET fence=9223372036854775807")
        self.assertEqual(self.analysis_claim(1011), 1)


EXPAND = SCHEMA.with_name("source_leases_expand.sql")


class SourceLeaseDispatchTurns(SourceDispatchTurns):
    """The same grants after the source-lease expand: a ready scan displaces
    other work only where its own claim would be admitted on its node."""

    def setUp(self):
        super().setUp()
        self.db.executescript("""
            CREATE TABLE items(id INTEGER PRIMARY KEY,library_id INTEGER NOT NULL);
            ALTER TABLE background_jobs ADD COLUMN target_node_id TEXT;
            ALTER TABLE files ADD COLUMN item_id INTEGER NOT NULL DEFAULT 1;
            ALTER TABLE files ADD COLUMN path TEXT NOT NULL DEFAULT '/library/fixture.mkv';
            ALTER TABLE analysis_requests ADD COLUMN file_id INTEGER NOT NULL DEFAULT 1;
            CREATE TABLE offline_packages(id TEXT PRIMARY KEY,file_id INTEGER NOT NULL);
            INSERT INTO items VALUES(1,1);
            INSERT INTO files(id,probe_json,size,mtime,item_id,path) VALUES(1,'{}',0,0,1,'/library/fixture.mkv');
            DROP TABLE background_job_required_resources;
            DROP TABLE analysis_required_resources;
            DROP VIEW dispatch_turn_library_resources;
        """)
        roots = Path(__file__).resolve().parents[2] / "crates/plurx-core/src/store/source_roots.sql"
        root_resources = Path(__file__).resolve().parents[2] / "crates/plurx-core/src/store/source_root_resources.sql"
        self.db.executescript(roots.read_text())
        # Install the exact production views this dispatch fixture exercises;
        # Rule A mapping-apply views belong to their separate fixture.
        required_views = ("background_job_required_resources", "analysis_required_resources",
                          "dispatch_turn_library_resources")
        installed = []
        for statement in root_resources.read_text().split("-- next statement"):
            for name in required_views:
                if f"CREATE VIEW {name} AS" in statement:
                    self.db.executescript(statement)
                    installed.append(name)
        self.assertCountEqual(installed, required_views)
        statements = [s for s in EXPAND.read_text().split("-- next statement")
                      if "CREATE TABLE source_leases" in s or "CREATE VIEW source_lease_owner_scopes(" in s or "source_lease_effective_resources AS" in s
                      or "source_lease_permissions AS" in s or "dispatch_turn" in s]
        for statement in statements:
            self.db.executescript(statement)
        self.db.executescript(DISPATCH_FIXTURE.read_text())

    def lease(self, name, node, key="source_io", expires=9000, kind="job", phase="active", projection=None):
        self.db.execute("INSERT INTO source_leases VALUES(?,?,?,1,?,'{}',json_array(?),?,?,1,1,?)",
                        (name, kind, name, node, key, phase, projection, expires))

    def custody(self, owner, key, now):
        before = self.db.execute("SELECT count(*) FROM source_leases").fetchone()[0]
        self.db.execute("INSERT INTO source_leases VALUES(?,?,?,1,'worker','{}',json_array(?),'active',NULL,?,?,?)",
                        (owner, owner, owner, key, now, now, now + 30000))
        return self.db.execute("SELECT count(*) FROM source_leases").fetchone()[0] - before

    # The v1 occupancy rows these inherited cases seed have no lease
    # successor here; the lease cases below replace them.
    def test_wildcard_scan_on_blocked_disk_leaves_disjoint_job_free(self):
        self.manual("scan", 1000)
        root_key = self.db.execute("SELECT root_key FROM source_lease_roots").fetchone()[0]
        root_id = self.db.execute("SELECT root_id FROM source_lease_roots").fetchone()[0]
        self.db.execute("INSERT INTO settings VALUES(?, '1')", (f"jobs.root_source_readers.{root_id}",))
        self.lease("reader", "worker", root_key)
        self.lease("reader-2", "elsewhere", root_key)
        self.assertEqual(self.analysis_claim(1001), 1, "the wildcard scan is blocked by its shared root budget")

    def test_other_root_respects_playback_slot_and_foreground_permission(self):
        self.manual("scan", 1000)
        self.lease("playback", "elsewhere", "source_io:a", 9000, "playback", "refill", '{"source_quiescent":false}')
        self.assertEqual(self.analysis_claim(1100), 1, "a refilling playback on any node blocks the scan's read")
        self.db.execute("UPDATE source_leases SET phase='source_idle',projection_json='{\"source_quiescent\":true}'")
        self.assertEqual(self.analysis_claim(1101, "index"), 0, "an idle playback permits it")

    def test_node_cap_is_the_scan_nodes(self):
        self.manual("scan", 1000)
        root_key = self.db.execute("SELECT root_key FROM source_lease_roots").fetchone()[0]
        root_id = self.db.execute("SELECT root_id FROM source_lease_roots").fetchone()[0]
        self.lease("a", "elsewhere", root_key)
        self.lease("b", "worker", root_key)
        self.assertEqual(self.analysis_claim(1001), 1, "root capacity is shared across nodes")
        self.db.execute("INSERT INTO settings VALUES(?, '3')", (f"jobs.root_source_readers.{root_id}",))
        self.db.execute("UPDATE analysis_requests SET state='queued' WHERE request_id='index'")
        self.assertEqual(self.analysis_claim(1002), 0, "the root override admits the scan")

    def test_domain_cap_and_grace(self):
        self.manual("scan", 1000)
        self.lease("other", "elsewhere", "source_io:a")
        self.db.execute("INSERT INTO settings VALUES('jobs.domain_source_readers.a','1')")
        self.assertEqual(self.analysis_claim(1001), 1, "an explicit domain cap blocks the scan")
        self.db.execute("UPDATE settings SET value='2' WHERE key='jobs.domain_source_readers.a'")
        self.db.execute("UPDATE analysis_requests SET state='queued' WHERE request_id='index'")
        self.assertEqual(self.analysis_claim(1002), 0, "raising the explicit cap admits the scan")
        self.db.execute("DELETE FROM settings")
        self.db.execute("UPDATE source_leases SET expires_at_ms=1003-60001")
        self.db.execute("UPDATE analysis_requests SET state='queued' WHERE request_id='index'")
        self.assertEqual(self.analysis_claim(1003), 0, "expired source leases no longer block the scan")

    def test_held_library_publication_lease_defers_only_until_expiry(self):
        self.manual("scan", 1000)
        self.db.execute("INSERT INTO job_leases VALUES('scan:library:1','old-worker',1,1,1010)")
        self.db.execute("INSERT INTO settings VALUES('internal.cluster_job_owner_removed.old-worker','true')")
        self.assertEqual(self.custody("lease", "source_io:a", 1009), 1)
        # That read attestation finished; its lease would otherwise fill domain a.
        self.db.execute("DELETE FROM source_leases")
        self.assertEqual(self.analysis_claim(1010), 0)
        self.db.execute("UPDATE job_leases SET fence=9223372036854775807")
        self.assertEqual(self.analysis_claim(1011), 1)
