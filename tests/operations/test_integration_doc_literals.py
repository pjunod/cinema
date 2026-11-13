"""`docs/INTEGRATION.md` states the scan seam the code implements.

Curator is written against INTEGRATION.md §1, not against Rust, so a limit, an
error code, a status or a `waiting_on` value that drifts there is a wire
contract broken in prose. This reads each literal from the source that owns it
and checks the page: the limits table row by row, every typed error code the
handler can return, the four request statuses, and every `waiting_on` prefix
plurxd writes. It also pins the documented Curator key mint to `scan:trigger`
alone, which is all Curator needs.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DOC = ROOT / "docs" / "INTEGRATION.md"
SCAN = ROOT / "crates" / "plurxd" / "src" / "http" / "scan.rs"
SIDECAR = ROOT / "crates" / "plurx-core" / "src" / "store" / "scan_request_v2.rs"
JOBS_SQL = ROOT / "crates" / "plurx-core" / "src" / "store" / "background_jobs.rs"
LIBRARY_WORK = ROOT / "crates" / "plurxd" / "src" / "library_work.rs"
WAIT_WRITERS = [
    ROOT / "crates" / "plurxd" / "src" / "library_work.rs",
    ROOT / "crates" / "plurxd" / "src" / "library_work" / "env.rs",
    ROOT / "crates" / "plurxd" / "src" / "background_jobs.rs",
]


def rust_const(path: Path, name: str) -> int:
    match = re.search(
        rf"pub(?:\(crate\))? const {name}: (?:u64|usize|i64) = ([0-9_]+)(?: \* ([0-9_]+))?;",
        path.read_text(),
    )
    if match is None:
        raise AssertionError(f"{name} is no longer an integer constant in {path.name}")
    value = int(match.group(1).replace("_", ""))
    return value * int(match.group(2).replace("_", "")) if match.group(2) else value


class IntegrationDocLiterals(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.doc = DOC.read_text()
        start = cls.doc.index("**Limits**")
        end = cls.doc.index("### 1.2")
        cls.limits = {
            code.strip("` "): value
            for value, code in re.findall(
                r"^\| [^|]+ \| ([^|]+) \| (`[A-Z_]+`)[^|]*\|$", cls.doc[start:end], re.M
            )
        }

    def limit(self, code: str) -> int:
        self.assertIn(code, self.limits, f"the INTEGRATION.md limits table has no `{code}` row")
        return int(re.match(r"\s*(\d+)", self.limits[code].split(" to ")[-1]).group(1))

    def test_limits_match_the_constants(self) -> None:
        for code, path in [
            ("MAX_WAIT_MS", SCAN),
            ("PATH_UNREADABLE_RETRY_S", SCAN),
            ("MAX_PROBE_PATHS", SCAN),
            ("MAX_REQUEST_KEY_BYTES", SIDECAR),
            ("MAX_EXPECTED_FILES", SIDECAR),
            ("MAX_REMOVED_PATHS", SIDECAR),
            ("MAX_WAITING_ON_BYTES", SIDECAR),
        ]:
            with self.subTest(code=code):
                self.assertEqual(self.limit(code), rust_const(path, code))

    def test_pending_request_bound_matches_the_enqueue_sql(self) -> None:
        bound = re.search(
            r"request\.library_id = json_extract\(body, '\$\.payload\.library_id'\) "
            r"AND waiter\.state = 'pending'\) >= (\d+) THEN 'queue_full'",
            JOBS_SQL.read_text(),
        )
        self.assertIsNotNone(bound, "the per-library pending bound moved out of ENQUEUE_SQL")
        self.assertEqual(self.limit("ENQUEUE_SQL"), int(bound.group(1)))

    def test_expected_file_bounds_and_the_recent_list_match_the_code(self) -> None:
        sidecar = SIDECAR.read_text()
        total = re.search(r"const MAX_EXPECTED_JSON_BYTES: usize = (\d+) \* 1024;", sidecar)
        path = re.search(r"file\.path\.len\(\) > (\d+)", sidecar)
        self.assertIsNotNone(total, "the expected_files total bound moved")
        self.assertIsNotNone(path, "the expected_files path bound moved")
        row = self.limits["MAX_EXPECTED_FILES"]
        self.assertIn(f"{total.group(1)} KiB of JSON", row)
        self.assertIn(f"each path at most {path.group(1)} bytes", row)
        self.assertIn(
            "MAX_EXPECTED_JSON_BYTES",
            re.search(r"fn validate\(.*?\n    \}\n", sidecar, re.S).group(0),
            "the 16 KiB bound is no longer what validation checks",
        )
        removed = self.limits["MAX_REMOVED_PATHS"]
        self.assertIn(f"each at most {path.group(1)} bytes", removed)
        self.assertIn(f"at most {total.group(1)} KiB of JSON together", removed)
        recent = rust_const(
            ROOT / "crates" / "plurx-core" / "src" / "store" / "background_jobs_library.rs",
            "MAX_LIBRARY_REQUESTS",
        )
        self.assertIn(f"The recent list is\nbounded to {recent} records", self.doc)

    def test_retry_after_is_stated_with_its_value(self) -> None:
        seconds = rust_const(SCAN, "PATH_UNREADABLE_RETRY_S")
        self.assertIn(f"`Retry-After: {seconds}`", self.doc)
        self.assertIn(f'"retry_after_s":{seconds}', self.doc)

    def test_every_typed_error_code_is_documented(self) -> None:
        source = SCAN.read_text()
        codes = set(re.findall(r'code: "([a-z_]+)"', source))
        codes |= set(re.findall(r'"code": "([a-z_]+)"', source))
        codes |= set(re.findall(r'const PATH_UNREADABLE: &str = "([a-z_]+)"', source))
        codes |= set(re.findall(r'\(\s*"([a-z_]+)",\s*"the scan request', source))
        # The record's own failure codes, which the GET read carries.
        record = re.search(
            r"fn request_record\(.*?\n\}\n", LIBRARY_WORK.read_text(), re.S
        )
        self.assertIsNotNone(record, "request_record moved")
        codes |= set(re.findall(r'(?:Some|then_some)\("([a-z_]+)"\)', record.group(0)))
        self.assertGreaterEqual(
            codes,
            {
                "path_required",
                "probe_paths_with_path",
                "scan_failed",
                "cancelled",
                "path_outside_roots",
                "path_unreadable",
            },
        )
        for code in sorted(codes):
            with self.subTest(code=code):
                self.assertIn(f'"code":"{code}"', self.doc)

    def test_the_four_statuses_are_the_ones_the_record_reports(self) -> None:
        mapping = re.search(r'let status = match record\.state\.as_str\(\) \{(.*?)\};', LIBRARY_WORK.read_text(), re.S)
        self.assertIsNotNone(mapping, "the request status mapping moved")
        statuses = set(re.findall(r'=> "([a-z]+)"', mapping.group(1)))
        self.assertEqual(statuses, {"done", "running", "queued", "failed"})
        self.assertIn("`status` is `queued` · `running` · `done` · `failed`", self.doc)

    def test_every_waiting_on_prefix_written_is_documented(self) -> None:
        # Every write goes through `note_targeted_wait`, or the two Store
        # setters it wraps; read the `format!` that builds each value.
        written = set()
        for path in WAIT_WRITERS:
            text = path.read_text()
            for call in re.finditer(r"(?:note_targeted_wait|set_scan_request_waiting_on|set_library_job_waiting_on)\(", text):
                argument = text[call.end() : call.end() + 400].split(")\n", 1)[0]
                written |= set(re.findall(r'format!\("([a-z_]+):\{', argument))
        self.assertGreaterEqual(written, {"full_scan", "root_unreadable", "queue_position"})
        for prefix in sorted(written):
            with self.subTest(prefix=prefix):
                self.assertRegex(self.doc, rf"\| `{prefix}:<[a-z]+>` \|")
        self.assertNotIn("heavy_lane", written, "the doc says plurx never writes heavy_lane")

    def test_the_documented_curator_key_holds_scan_trigger_only(self) -> None:
        mint = re.findall(r'"scopes":\[([^\]]*)\]', self.doc)
        self.assertTrue(mint, "the key mint example is gone")
        for scopes in mint:
            self.assertEqual(scopes, '"scan:trigger"')


if __name__ == "__main__":
    unittest.main()
