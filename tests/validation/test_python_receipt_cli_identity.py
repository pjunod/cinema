"""Synthetic fresh-process CLI identity; no credentials or HTTP requests."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


class ReceiptCLIIdentityCase(unittest.TestCase):
    def test_module_cli_owner_fallback_and_exact_zero_unit_refusal(self):
        program = textwrap.dedent('''
            import copy, hashlib, json, runpy, sys
            from pathlib import Path
            from unittest.mock import patch
            import urllib.request

            calls = []
            state = {}
            def configure(frame, event, arg):
                if event != "call" or frame.f_code.co_name != "main":
                    return
                sys.setprofile(None)
                module = frame.f_globals
                state["module"] = module
                state["restore"] = module["restore"]
                class FakeAPI:
                    def __init__(self, *args): pass
                    def get(self, path):
                        calls.append(path)
                        raise module["ReceiptHTTPError"](403, path)
                def restore(api, scope, run, applicability=None):
                    from validation import python_unit_interrupted_recovery as recovery
                    assert recovery.receipts is sys.modules["__main__"]
                    for _ in range(4):
                        recovery.receipts.verify_attestor(api, scope, {"id": 1, "login": "pjunod"})
                    return {}
                module.update(API=FakeAPI, identity=lambda *a: {"repository":1,"pr":767},
                              restore=restore, SourceApplicability=lambda commit: None,
                              atomic_json=lambda *a: None)
                module["subprocess"].check_output = lambda *a, **k: "a" * 40

            # This is CPython's actual -m entry path, in a clean subprocess.
            sys.argv = ["validation.python_unit_receipts", "prepare"]
            sys.setprofile(configure)
            with patch.object(urllib.request, "build_opener", side_effect=AssertionError("network forbidden")):
                try:
                    runpy._run_module_as_main("validation.python_unit_receipts", alter_argv=True)
                except SystemExit as exit:
                    assert exit.code == 0, exit.code
            assert len(calls) == 4
            from validation import python_unit_interrupted_recovery as recovery
            receipts = recovery.receipts
            assert receipts.ReceiptHTTPError is state["module"]["ReceiptHTTPError"]

            proof, digest = recovery.prepare_case()
            proof = copy.deepcopy(proof)
            scope = recovery.SCOPE
            source = {path: ("synthetic:" + path).encode() for path in proof["source_hashes"]}
            proof["source_hashes"] = {path: hashlib.sha256(raw).hexdigest() for path, raw in source.items()}
            refusal = ("Python receipt refusal: ReceiptHTTPError: HTTP 403 at "
                       "/repos/noirr/plurx/collaborators/pjunod/permission; evidence unavailable")
            lines = [proof["commit"], refusal,
                     "skipping post step for 'Publish Python attempt-start marker'; main step was skipped",
                     "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped",
                     "Job 'Python unit receipts' failed"]
            raw = ("\\n".join(lines) + "\\n").encode()
            proof["log_sha256"] = hashlib.sha256(raw).hexdigest()
            prior = {"id":3985,"commit_sha":proof["commit"]}
            jobs = [dict(row,run_id=3985,repo_id=1,attempt=1) for row in proof["jobs"]]
            class EvidenceAPI:
                def __init__(self): self.raw=raw; self.artifacts=[]; self.source=source
                def get(self, path):
                    if path == "/actions/runs/3985":
                        return dict(prior,repository={"id":1},prettyref=scope["branch"],workflow_id="effort-ci.yml",status="failure")
                    if path == "/actions/runs/3985/artifacts": return self.artifacts
                    if path.startswith("/collaborators/"): raise receipts.ReceiptHTTPError(403,path)
                    raise AssertionError(path)
                def bytes(self, path, params=None):
                    if path == "/actions/jobs/41443/logs": return self.raw
                    assert params == {"ref":proof["commit"]}
                    return self.source[path.removeprefix("/raw/")]
                def pages(self, path, params=None, field=None):
                    if path == "/issues/767/comments":
                        return [{"id":1,"user":{"id":1,"login":"pjunod"},"body":"Python-Journal-Recovery: " + json.dumps({"repository":1,"pr":767,"sha256":digest})}]
                    if path == "/actions/runs": return [prior]
                    if path == "/actions/runs/3985/jobs": return jobs
                    assert path == "/actions/artifacts"
                    return self.artifacts
            api = EvidenceAPI()
            with patch.object(recovery, "prepare_case", return_value=(proof,digest)):
                assert recovery.recover_interrupted_pr767(api,scope,prior,jobs,{},4000) == (True,None)
                inherited = {"validation:test_synthetic.C.test_retained": {"commit":None,"run":None,"local_receipt":"a"*64,"test_file_sha256":"b"*64,"comment":1}}
                with patch.object(receipts,"legacy_pr663",return_value={}), patch.object(receipts,"local_receipts",return_value=inherited):
                    assert state["restore"](api,scope,4000) == inherited
                for kind in ("metadata","rerun","source","log","truncated","duplicate","outcome","artifact"):
                    changed_jobs=copy.deepcopy(jobs); api=EvidenceAPI()
                    changed_prior=dict(prior)
                    if kind == "metadata": changed_prior["commit_sha"]="b"*40
                    if kind == "rerun": changed_jobs[1]["attempt"]=2
                    if kind == "source": api.source=dict(source); api.source[next(iter(source))]=b"altered"
                    if kind == "log": api.raw=b"altered"
                    if kind in ("truncated","duplicate","outcome"):
                        api.raw = raw[:-1] if kind == "truncated" else raw + (refusal if kind == "duplicate" else "test_x (test_x.C.test_x) ... ok").encode() + b"\\n"
                        proof["log_sha256"]=hashlib.sha256(api.raw).hexdigest()
                    if kind == "artifact": api.artifacts=[{"run_id":3985}]
                    try:
                        recovery.recover_interrupted_pr767(api,scope,changed_prior,changed_jobs,{},4000)
                    except receipts.ReceiptError: pass
                    else: raise AssertionError("accepted " + kind)
                    proof["log_sha256"]=hashlib.sha256(raw).hexdigest()
            print("CLI owner403 identity and exact zero-unit recovery controls passed")
        ''')
        with tempfile.TemporaryDirectory() as temporary:
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                   "PYTHONPATH": str(Path.cwd()), "GITHUB_API_URL": "http://synthetic.invalid/api/v1",
                   "GITHUB_SERVER_URL": "http://synthetic.invalid", "GITHUB_REPOSITORY": "noirr/plurx",
                   "GITHUB_SHA": "a" * 40, "GITHUB_RUN_ID": "4000",
                   "GITHUB_RUN_ATTEMPT": "1", "GITHUB_REF_NAME": "codex/ci-windows-baremetal-routing",
                   "GITHUB_OUTPUT": str(Path(temporary) / "output")}
            completed = subprocess.run([sys.executable, "-c", program], env=env,
                                       capture_output=True, text=True, timeout=15)
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        self.assertEqual(completed.stderr.count("checking explicit enrollment"), 4 + 10)
        self.assertIn("CLI owner403 identity and exact zero-unit recovery controls passed", completed.stdout)
