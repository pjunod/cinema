"""Exercise every grant family against the same installed SQLite triggers."""
import sqlite3
import unittest
from pathlib import Path

SCHEMA = Path(__file__).resolve().parents[2] / "crates/plurx-core/src/store/background_job_turns.sql"

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
            INSERT INTO analysis_requests VALUES('index','fragment_index','background','queued',0,1,0);
            INSERT INTO analysis_required_resources VALUES('index','source_io:a');
        """)
        self.db.executescript(SCHEMA.read_text())

    def manual(self, name, now):
        self.db.execute("INSERT INTO background_jobs VALUES(?,'queued',0,'library_scan','{\"library_id\":1}',0,?,?,0)", (name,now,now))
        self.db.execute("INSERT INTO background_library_requests VALUES(?,?,'{\"kind\":\"full\",\"trigger\":\"manual\"}',NULL)", (name,name))
        self.db.execute("INSERT INTO background_job_waiters VALUES(?,'library',?,'pending','library_work',NULL,?,NULL,NULL)", (name,name,now))
        self.db.execute("INSERT INTO background_job_required_resources VALUES(?,'source_io:a')", (name,))
        self.db.execute("INSERT INTO dispatch_turn_ready VALUES(?,'worker',0,?,'[\"/library\"]')", (name,now+7000))

    def analysis_claim(self, now):
        return self.db.execute("UPDATE analysis_requests SET state='running',fence=fence+1,updated_at_ms=? WHERE request_id='index' AND state='queued'", (now,)).rowcount

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
