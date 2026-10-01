"""Review35 regressions: controlled local processes only, never real host tools."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

if __package__:
    from .test_k06_owned_lab import LAB, manifest
else:
    # CI's discovery root loads operations modules without a package context.
    from test_k06_owned_lab import LAB, manifest


def executable(path, source):
    path.write_text("#!" + sys.executable + "\n" + source)
    path.chmod(0o700)


class OwnedClockLabReviewTests(unittest.TestCase):
    def test_threaded_remote_uses_exec_limiter_without_post_fork_callback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable(root / "ssh", """import json,resource,sys
source=json.loads(sys.stdin.readline())
request=json.loads(sys.stdin.readline())
if request['payload'].get('overflow'):
    sys.stdout.write('x' * (3 * 1024 * 1024))
else:
    print(json.dumps({'limit':resource.getrlimit(resource.RLIMIT_FSIZE)[0],
                      'private_stdin':request['payload']['private']=='fixture-private',
                      'argv_private':'fixture-private' in ' '.join(sys.argv)}))
""")
            actual_run = subprocess.run
            def launch(*args, **kwargs):
                self.assertTrue("preexec_fn" not in kwargs, "threaded launch must never execute Python after fork")
                return actual_run(*args, **kwargs)
            env = {"PATH": str(root) + os.pathsep + os.environ["PATH"]}
            m = manifest()
            with patch.dict(os.environ, env), patch.object(LAB.subprocess, "run", launch):
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                    results = list(pool.map(lambda n: LAB.remote(root / "fixture-key", m, n, "sample",
                                                                {"private": "fixture-private"}), m["nodes"]))
                self.assertEqual(len(results), 4)
                for result in results:
                    self.assertEqual(result["limit"], 2 * 1024 * 1024)
                    self.assertTrue(result["private_stdin"])
                    self.assertFalse(result["argv_private"])
                with self.assertRaises(ValueError):
                    LAB.remote(root / "fixture-key", m, m["nodes"][0], "sample", {"overflow": True})

    def test_cleanup_exports_both_daemon_channels_before_exact_remove(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable(root / "docker", """import json,os,sys
from pathlib import Path
root=Path(os.environ['K06_FIXTURE_ROOT'])
if sys.argv[1]=='logs':
    print('fixture stdout receipt',flush=True)
    print('fixture panic stderr receipt',file=sys.stderr,flush=True)
elif sys.argv[1]=='rm':
    with (root/'events.jsonl').open('a') as stream:
        stream.write(json.dumps({'id':sys.argv[2],'log_before_remove':(root/'daemon.log').read_text()})+'\\n')
""")
            m = manifest()
            n = m["nodes"][0]
            n.update({"container_id": "1" * 64, "network_id": "2" * 64,
                      "production": {"fixture": "unchanged"}})
            actual_command = LAB.command
            def controlled_command(args, **kwargs):
                if args[0] == "hostname":
                    return n["host"] + "\n"
                if args[0] == "ss":
                    return ""
                return actual_command(args, **kwargs)
            def controlled_inspect(kind, identity):
                if kind == "container":
                    return {"State": {"Running": False}}
                return {"Labels": {LAB.LABEL: m["owner"]}, "Containers": {}}
            env = {"PATH": str(root) + os.pathsep + os.environ["PATH"], "K06_FIXTURE_ROOT": str(root)}
            with patch.dict(os.environ, env), patch.object(LAB, "command", controlled_command), \
                 patch.object(LAB, "inspect", controlled_inspect), patch.object(LAB, "root_check", return_value=root), \
                 patch.object(LAB, "validate_container"), patch.object(LAB, "production", return_value=n["production"]):
                result = LAB.worker({"manifest": m, "node": n, "action": "cleanup", "payload": None})
            self.assertTrue(result["stopped"])
            event = json.loads((root / "events.jsonl").read_text())
            self.assertEqual(event["id"], n["container_id"])
            self.assertIn("fixture stdout receipt", event["log_before_remove"])
            self.assertIn("fixture panic stderr receipt", event["log_before_remove"])
            self.assertEqual((root / "daemon.log").stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
