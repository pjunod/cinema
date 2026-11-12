"""The pinned media runtime's DV helpers match the daemon built from this tree.

plurxd compiles the identity of tools/dv_processing into itself and refuses a
helper bundle with any other identity, declining every processed Dolby Vision
upgrade. While the bundle ships inside the digest-pinned runtime, the pin must
record the identity of the bundle it carries, and that identity must equal the
daemon's for this tree. A tools/dv_processing change without a republished
runtime fails here instead of in production.
"""
from pathlib import Path
import hashlib
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
DAEMON = ROOT / "crates/plurxd/src/dv_runtime.rs"


def daemon_identity() -> str:
    source = DAEMON.read_text(encoding="utf-8")
    body = source[source.index("fn renderer_source_identity() -> String {"):]
    body = body[:body.index("hex::encode(identity.finalize())")]
    declared = re.search(r"let inputs: \[\(&str, &\[u8\]\); (\d+)\] = \[", body)
    pairs = re.findall(r'\(\s*"([^"]+)",\s*include_bytes!\("([^"]+)"\),?\s*\)', body)
    if declared is None or int(declared.group(1)) != len(pairs) or not pairs:
        raise AssertionError("renderer_source_identity no longer has the shape this check reads")
    identity = hashlib.sha256()
    for name, path in pairs:
        data = (DAEMON.parent / path).resolve().read_bytes()
        identity.update(name.encode() + b"\0" + hashlib.sha256(data).digest())
    return identity.hexdigest()


class PinnedRuntimeDvIdentity(unittest.TestCase):
    def test_pinned_runtime_carries_the_dv_helpers_this_tree_expects(self):
        recorded = re.findall(r'^ARG PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256="([0-9a-f]{64})"$',
                              (ROOT / "Dockerfile").read_text(encoding="utf-8"), re.MULTILINE)
        self.assertEqual(len(recorded), 1, "Dockerfile must record the pinned runtime's DV helper identity")
        self.assertEqual(recorded[0], daemon_identity(),
                         "tools/dv_processing changed: rebuild and re-pin the media runtime")


if __name__ == "__main__":
    unittest.main()
