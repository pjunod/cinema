"""Synthetic fresh-process CLI identity; no credentials or HTTP requests."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


class ReceiptCLIIdentityCase(unittest.TestCase):
    def test_module_cli_shares_identity_with_imports_and_owner_fallback(self):
        """`python3 -m` and `import` must agree on ReceiptError and the API."""
        program = textwrap.dedent('''
            import sys
            from unittest.mock import patch
            import urllib.request
            import runpy

            calls = []
            state = {}
            def configure(frame, event, arg):
                if event != "call" or frame.f_code.co_name != "main":
                    return
                sys.setprofile(None)
                module = frame.f_globals
                state["module"] = module
                class FakeAPI:
                    def __init__(self, *args): pass
                    def get(self, path):
                        calls.append(path)
                        raise module["ReceiptHTTPError"](403, path)
                def restore(api, scope, run, applicability=None):
                    from validation import python_unit_receipts as imported
                    assert imported is sys.modules["__main__"]
                    for _ in range(4):
                        imported.verify_attestor(api, scope, {"id": 1, "login": "pjunod"})
                    return {}
                module.update(API=FakeAPI, identity=lambda *a: {"repository": 1, "pr": 767},
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
            from validation import python_unit_receipts as receipts
            assert receipts.ReceiptHTTPError is state["module"]["ReceiptHTTPError"]
            assert receipts.ReceiptError is state["module"]["ReceiptError"]
            print("CLI owner403 identity controls passed")
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
        self.assertEqual(completed.stderr.count("checking explicit enrollment"), 4)
        self.assertIn("CLI owner403 identity controls passed", completed.stdout)


if __name__ == "__main__":
    unittest.main()
