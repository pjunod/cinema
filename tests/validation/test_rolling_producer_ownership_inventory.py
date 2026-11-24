from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path

from validation.rust_modules import module_segments, module_source, split_children


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tests/playback/rolling-producer-owners.toml"
MAX_LEADING_COMMENT_LINES = 15
RAW_STRING_START = re.compile(r'(?:br|rb|r)(#*)"')
TURBOFISH_START = re.compile(r"::\s*<")
CHAR_LITERAL = re.compile(
    r"'(?:\\(?:[nrt0\\'\"]|x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f_]{1,6}\})|[^\\'\r\n])'"
)
TRANSPARENT_CALLABLE_GROUP = re.compile(
    r"\(\s*(?P<reference>(?:(?:&\s*(?:mut\s+)?)|(?:\*\s*))*)"
    r"(?P<path>(?:::)?(?:[A-Za-z_]\w*::)*[A-Za-z_]\w*)\s*\)"
    r"(?=(?:\s*\))*\s*\()"
)
EXPRESSION_PREFIX_KEYWORDS = frozenset(
    {
        "become",
        "box",
        "break",
        "else",
        "if",
        "in",
        "let",
        "match",
        "move",
        "return",
        "while",
        "yield",
    }
)


def _is_raw_identifier_prefix(source: str, index_before_word: int) -> bool:
    return (
        index_before_word >= 1
        and source[index_before_word - 1 : index_before_word + 1] == "r#"
    )


def _blank_non_newlines(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] not in "\r\n":
            chars[index] = " "


def _preceded_by_callable(source: str, start: int) -> bool:
    index = start - 1
    while index >= 0 and source[index].isspace():
        index -= 1
    if index < 0:
        return False
    if source[index] in ")]}!?":
        return True
    if not (source[index].isalnum() or source[index] == "_"):
        return False
    end = index + 1
    while index >= 0 and (source[index].isalnum() or source[index] == "_"):
        index -= 1
    word = source[index + 1 : end]
    if word in EXPRESSION_PREFIX_KEYWORDS and not _is_raw_identifier_prefix(
        source, index
    ):
        return False
    if index >= 0 and source[index] == "'":
        index -= 1
        while index >= 0 and source[index].isspace():
            index -= 1
        label_owner_end = index + 1
        while index >= 0 and (source[index].isalnum() or source[index] == "_"):
            index -= 1
        if source[index + 1 : label_owner_end] in {
            "break",
            "continue",
        } and not _is_raw_identifier_prefix(source, index):
            return False
    return True


def rust_structural_source(source: str) -> str:
    """Retain Rust syntax while removing comments, literals, and turbofish payloads."""

    chars = list(source)
    index = 0
    length = len(chars)
    while index < length:
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = length if end < 0 else end
            _blank_non_newlines(chars, index, end)
            index = end
            continue
        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < length and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            _blank_non_newlines(chars, index, end)
            index = end
            continue

        raw = RAW_STRING_START.match(source, index)
        if raw is not None:
            terminator = '"' + raw.group(1)
            body = raw.end()
            close = source.find(terminator, body)
            end = length if close < 0 else close + len(terminator)
            _blank_non_newlines(chars, index, end)
            index = end
            continue

        quote = index + 1 if source.startswith('b"', index) else index
        if quote < length and source[quote] == '"':
            end = quote + 1
            escaped = False
            while end < length:
                char = source[end]
                end += 1
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    break
            _blank_non_newlines(chars, index, end)
            index = end
            continue

        if source[index] == "'":
            literal = CHAR_LITERAL.match(source, index)
            if literal is None:
                index += 1
            else:
                _blank_non_newlines(chars, index, literal.end())
                index = literal.end()
            continue

        index += 1

    code = "".join(chars)
    chars = list(code)
    index = 0
    while index < len(chars):
        opener = TURBOFISH_START.match(code, index)
        if opener is None:
            index += 1
            continue
        depth = 1
        delimiter_depth = 0
        end = opener.end()
        while end < len(chars) and depth:
            char = code[end]
            if char in "([{":
                delimiter_depth += 1
            elif char in ")]}" and delimiter_depth:
                delimiter_depth -= 1
            elif char == "<" and delimiter_depth == 0:
                depth += 1
            elif (
                char == ">"
                and delimiter_depth == 0
                and (end == 0 or code[end - 1] not in "-=")
            ):
                depth -= 1
            end += 1
        if depth == 0:
            _blank_non_newlines(chars, index, end)
            index = end
        else:
            index += 1
    code = "".join(chars)

    while True:
        changed = False

        def unwrap(match: re.Match[str]) -> str:
            nonlocal changed
            if _preceded_by_callable(code, match.start()):
                return match.group(0)
            value = match.group("reference") + match.group("path")
            padding = len(match.group(0)) - len(value)
            changed = True
            return " " * (padding // 2) + value + " " * (padding - padding // 2)

        code = TRANSPARENT_CALLABLE_GROUP.sub(unwrap, code)
        if not changed:
            return code


class RollingProducerOwnershipInventoryTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.catalog = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
        cls.source_path = ROOT / cls.catalog["source"]
        cls.source = module_source(cls.source_path)
        cls.source_children = split_children(cls.source_path)
        cls.module_roots = tuple(cls.catalog["module_roots"])
        cls.module_paths = tuple(
            path
            for module_root in cls.module_roots
            for path in sorted((ROOT / module_root).rglob("*.rs"))
        )
        cls.module_file_text = {
            cls.relative(path): path.read_text(encoding="utf-8")
            for path in cls.module_paths
        }
        cls.module_file_structural = {
            relative: rust_structural_source(text)
            for relative, text in cls.module_file_text.items()
        }
        cls.scope_cache: dict[str, list[tuple[str, str]]] = {}

    @staticmethod
    def relative(path: Path) -> str:
        return path.resolve().relative_to(ROOT.resolve()).as_posix()

    @classmethod
    def read_scope(cls, relative_path: str) -> list[tuple[str, str]]:
        """Read one scope path as `(repository-relative file, text)` segments.

        A file is read with its S-14 split children expanded
        (`validation.rust_modules`), and each child's text is attributed to the
        child's own path, not to the parent. A directory is every `*.rs` under
        it, skipping the files `source` already expanded, so a row whose
        `also_in` names the directory those children live in counts each
        occurrence once.
        """

        cached = cls.scope_cache.get(relative_path)
        if cached is not None:
            return cached
        path = ROOT / relative_path
        if path.is_dir():
            segments = [
                (cls.relative(child), child.read_text(encoding="utf-8"))
                for child in sorted(path.rglob("*.rs"))
                if child.resolve() not in cls.source_children
            ]
        else:
            segments = [
                (cls.relative(file), text) for file, text in module_segments(path)
            ]
        cls.scope_cache[relative_path] = segments
        return segments

    @classmethod
    def symbol_scope(cls, row: dict) -> tuple[list[tuple[str, str]], str]:
        """Return the segments a `[[symbols]]` row counts and the name of that scope.

        A row counts `source`. A row that also carries `also_in` counts the paths
        it names alongside `source`, so an occurrence that moved out of `source`
        into an extracted module is still counted by the same entry.
        """

        paths = [cls.catalog["source"], *row.get("also_in", ())]
        segments = [seg for path in paths for seg in cls.read_scope(path)]
        return segments, " and ".join(paths)

    @staticmethod
    def count_by_file(
        pattern: re.Pattern[str], texts: "list[tuple[str, str]] | dict[str, str]"
    ) -> dict[str, int]:
        """Nonzero per-file match counts, keyed by repository-relative path."""

        items = texts.items() if isinstance(texts, dict) else texts
        counts: dict[str, int] = {}
        for relative, text in items:
            found = len(pattern.findall(text))
            if found:
                counts[relative] = counts.get(relative, 0) + found
        return counts

    def assert_file_counts(
        self, row: dict, actual: dict[str, int], scope_name: str, hint: str
    ) -> None:
        """Compare a row's reviewed `occurrences` table with the scan, per file."""

        expected = row["occurrences"]
        problems = []
        for relative in sorted(set(expected) | set(actual)):
            want = expected.get(relative, 0)
            got = actual.get(relative, 0)
            if want == got:
                continue
            if want == 0:
                action = (
                    f"add the line `\"{relative}\" = {got}` under "
                    f"[{row['id']}] occurrences"
                )
            elif got == 0:
                action = f"remove the `\"{relative}\"` line (it no longer matches)"
            else:
                action = f"update the `\"{relative}\"` line to {got}"
            problems.append(
                f"{relative}: expected {want}, actual {got}; {action}"
            )
        self.assertFalse(
            problems,
            f"{row['id']} changed in {scope_name}; {hint}. Update only the "
            "named file's line in tests/playback/rolling-producer-owners.toml "
            "and put the reason in the pull request description:\n  "
            + "\n  ".join(problems),
        )

    def assert_occurrence_table(self, row: dict) -> None:
        """An `occurrences` table is sorted, nonzero, and names existing files."""

        table = row["occurrences"]
        self.assertIsInstance(table, dict)
        self.assertEqual(
            list(table),
            sorted(table),
            f"{row['id']} occurrences keys must be sorted",
        )
        for relative, count in table.items():
            self.assertIsInstance(count, int)
            self.assertGreater(
                count,
                0,
                f"{row['id']} lists {relative} with no occurrences; remove the line",
            )
            self.assertTrue(
                (ROOT / relative).is_file(),
                f"{row['id']} names a missing file {relative}",
            )

    def test_manifest_leading_comment_block_stays_short(self) -> None:
        lines = MANIFEST.read_text(encoding="utf-8").splitlines()
        leading = 0
        for line in lines:
            if line.startswith("#") or not line.strip():
                leading += 1
            else:
                break
        self.assertLessEqual(
            leading,
            MAX_LEADING_COMMENT_LINES,
            "the manifest's leading comment block must stay a short header; "
            "put the reason for a count change in the pull request description, "
            "not in a new paragraph at the top of the file",
        )

    def test_catalog_has_unique_complete_rows(self) -> None:
        self.assertEqual(self.catalog.get("version"), 1)
        symbols = self.catalog["symbols"]
        module_symbols = self.catalog["module_symbols"]
        module_structures = self.catalog["module_structures"]
        contract_cases = self.catalog["contract_cases"]
        negative_cases = self.catalog["negative_cases"]
        entries = self.catalog["entrypoints"]
        symbol_ids = [entry["id"] for entry in symbols]
        module_symbol_ids = [entry["id"] for entry in module_symbols]
        module_structure_ids = [entry["id"] for entry in module_structures]
        contract_case_ids = [entry["id"] for entry in contract_cases]
        negative_case_ids = [entry["id"] for entry in negative_cases]
        entry_ids = [entry["id"] for entry in entries]
        self.assertEqual(len(symbol_ids), len(set(symbol_ids)))
        self.assertEqual(len(module_symbol_ids), len(set(module_symbol_ids)))
        self.assertEqual(len(module_structure_ids), len(set(module_structure_ids)))
        self.assertEqual(len(contract_case_ids), len(set(contract_case_ids)))
        self.assertEqual(len(negative_case_ids), len(set(negative_case_ids)))
        self.assertEqual(len(entry_ids), len(set(entry_ids)))
        self.assertEqual(
            {
                "namespaced-task-turbofish",
                "method-task-spawn-local",
                "bare-task-spawn",
                "parenthesized-task-call",
                "nested-parenthesized-task-call",
                "referenced-nested-task-call",
                "wrapped-task-call-inside-argument",
                "for-in-wrapped-task-call",
                "namespaced-time-turbofish",
                "bare-time-constructor",
                "parenthesized-local-timer-alias",
                "turbofished-local-task-alias",
                "referenced-local-task-alias",
                "interleaved-local-task-alias",
                "command-construction",
                "command-method-launch",
                "bare-command-ufcs",
                "namespaced-command-ufcs",
                "parenthesized-command-alias",
                "interleaved-command-alias",
                "grouped-command-import-alias",
                "absolute-command-import-alias",
                "absolute-command-type-alias",
                "labeled-break-command-call",
                "namespaced-process-clone",
                "bare-imported-process-start",
                "grouped-process-namespace-alias",
                "absolute-process-namespace-alias",
                "extern-process-namespace-alias",
                "process-lifecycle-method",
                "free-process-signal",
                "local-low-level-alias",
                "interleaved-low-level-alias",
            },
            set(contract_case_ids),
        )
        self.assertEqual(
            {
                "ordinary-call-argument",
                "referenced-ordinary-call-argument",
                "raw-keyword-call-argument",
            },
            set(negative_case_ids),
        )
        self.assertEqual(
            {
                "namespaced-task-spawn",
                "method-spawn",
                "bare-task-spawn",
                "namespaced-time-constructor",
                "bare-time-constructor",
                "tokio-time-import",
                "process-command-construction",
                "process-capable-launch-method",
                "process-command-ufcs-launch",
                "low-level-process-start",
                "process-lifecycle-method",
                "free-or-ufcs-process-lifecycle",
                "kill-on-drop-construction",
                "rolling-supervisor-construction",
                "forbidden-timer-or-task-alias",
                "forbidden-timer-or-task-callable-alias",
                "forbidden-command-alias",
                "forbidden-low-level-function-alias",
                "forbidden-process-namespace-alias",
                "forbidden-command-type-alias",
                "forbidden-command-callable-alias",
                "forbidden-low-level-callable-alias",
            },
            set(module_structure_ids),
        )
        self.assertGreaterEqual(len(symbols), 28)
        self.assertGreaterEqual(len(module_symbols), 13)
        self.assertGreaterEqual(len(module_structures), 22)
        self.assertGreaterEqual(len(contract_cases), 33)
        self.assertGreaterEqual(len(negative_cases), 3)
        self.assertGreaterEqual(len(entries), 7)
        self.assertGreaterEqual(len(self.module_paths), 20)
        self.assertEqual(
            self.module_roots,
            (
                "crates/plurxd/src",
                "crates/plurx-core/src/process",
                "crates/plurx-core/src/scan",
            ),
        )

        symbol_keys = {
            "id",
            "pattern",
            "occurrences",
            "kind",
            "replacement",
        }
        self.assertEqual(symbol_keys, set(symbols[0]) - {"also_in"})
        for row in symbols:
            with self.subTest(symbol=row["id"]):
                self.assertEqual(symbol_keys, set(row) - {"also_in"})
                self.assertGreater(len(row["occurrences"]), 0)
                self.assert_occurrence_table(row)
                self.assertTrue(row["replacement"].strip())
                if "also_in" in row:
                    self.assertTrue(
                        row["also_in"],
                        f"{row['id']} declares an empty also_in scope",
                    )
                    for extra in row["also_in"]:
                        self.assertIsInstance(extra, str)
                        self.assertNotEqual(
                            extra,
                            self.catalog["source"],
                            f"{row['id']} counts {extra} twice",
                        )
                        self.assertTrue(
                            (ROOT / extra).exists(),
                            f"{row['id']} names a missing also_in path {extra}",
                        )

        for row in module_symbols:
            with self.subTest(module_symbol=row["id"]):
                self.assertEqual({"id", "pattern", "occurrences"}, set(row))
                self.assert_occurrence_table(row)

        for row in module_structures:
            with self.subTest(module_structure=row["id"]):
                self.assertEqual({"id", "pattern", "occurrences"}, set(row))
                self.assert_occurrence_table(row)

        for row in contract_cases:
            with self.subTest(contract_case=row["id"]):
                self.assertEqual({"id", "source", "must_trigger"}, set(row))
                self.assertTrue(row["source"].strip())
                self.assertTrue(row["must_trigger"])
                self.assertLessEqual(
                    set(row["must_trigger"]), set(module_structure_ids)
                )

        for row in negative_cases:
            with self.subTest(negative_case=row["id"]):
                self.assertEqual({"id", "source", "must_not_trigger"}, set(row))
                self.assertTrue(row["source"].strip())
                self.assertTrue(row["must_not_trigger"])
                self.assertLessEqual(
                    set(row["must_not_trigger"]), set(module_structure_ids)
                )

        for row in entries:
            with self.subTest(entrypoint=row["id"]):
                self.assertEqual(
                    {"id", "anchor", "owns", "replacement"}, set(row)
                )
                self.assertTrue(row["owns"])
                self.assertTrue(row["replacement"].strip())

    def test_every_tracked_symbol_has_the_reviewed_source_count(self) -> None:
        for row in self.catalog["symbols"]:
            with self.subTest(symbol=row["id"]):
                try:
                    pattern = re.compile(row["pattern"])
                except re.error as error:
                    self.fail(f"invalid pattern for {row['id']}: {error}")
                segments, scope_name = self.symbol_scope(row)
                self.assert_file_counts(
                    row,
                    self.count_by_file(pattern, segments),
                    scope_name,
                    "update the source and ownership ledger together",
                )

    def test_module_wide_legacy_and_deadline_sentinels_have_reviewed_counts(self) -> None:
        for row in self.catalog["module_symbols"]:
            with self.subTest(module_symbol=row["id"]):
                try:
                    pattern = re.compile(row["pattern"])
                except re.error as error:
                    self.fail(f"invalid module pattern for {row['id']}: {error}")
                self.assert_file_counts(
                    row,
                    self.count_by_file(pattern, self.module_file_text),
                    f"the files under {', '.join(self.module_roots)}",
                    "update the module-wide owner allowlist in the same reviewed change",
                )

    def test_module_wide_task_timer_and_process_shapes_have_reviewed_counts(self) -> None:
        for row in self.catalog["module_structures"]:
            with self.subTest(module_structure=row["id"]):
                try:
                    pattern = re.compile(row["pattern"])
                except re.error as error:
                    self.fail(f"invalid structural pattern for {row['id']}: {error}")
                counts = self.count_by_file(pattern, self.module_file_structural)
                if row["id"] == "low-level-process-start":
                    # Each pinned call site is reviewed; only unowned starts count.
                    for relative, reviewed in self.reviewed_containment_starts(
                        pattern
                    ).items():
                        remaining = counts.get(relative, 0) - reviewed
                        if remaining:
                            counts[relative] = remaining
                        else:
                            counts.pop(relative, None)
                self.assert_file_counts(
                    row,
                    counts,
                    f"the files under {', '.join(self.module_roots)}",
                    "new task, timer, process, or alias shapes require an explicit "
                    "ownership review and allowlist update",
                )

    def reviewed_containment_starts(self, pattern: re.Pattern) -> dict[str, int]:
        """Reviewed fixture and launch starts, per repository-relative file."""
        path = ROOT / "crates/plurx-core/src/process/contained_launch.rs"
        header = (
            '    #[test]\n'
            '    #[ignore = "private child executable entry; invoked only by delegated cgroup test"]\n'
            '    fn private_contained_child_entry() {\n'
        )
        source = path.read_text(encoding="utf-8")
        self.assertIn(path, self.module_paths)
        self.assertEqual(source.count(header), 1)
        fixture = source.split(header, 1)[1].split("\n    #[test]", 1)[0]
        limit = fixture.split('} else if mode == "fork-limit" {', 1)[1].split(
            '} else if mode == "memory-limit" {', 1
        )[0]
        orphan = fixture.split('} else if mode == "orphan" {', 1)[1]
        self.assertIn("let pid = unsafe { libc::fork() };", limit)
        self.assertIn("let result = unsafe { libc::fork() };", orphan)
        for branch in (limit, orphan):
            self.assertEqual(pattern.findall(rust_structural_source(branch)), ["libc::fork("])
        self.assertEqual(len(pattern.findall(rust_structural_source(fixture))), 2)

        restricted = source.split("    fn private_restricted_child_entry() {", 1)[1]
        restricted = restricted.split("\n    #[test]", 1)[0]
        self.assertRegex(
            restricted,
            r"libc::syscall\(\s*libc::SYS_clone3,\s*args.as_ptr\(\),"
            r"\s*std::mem::size_of_val\(&args\),\s*\)",
        )
        self.assertEqual(len(pattern.findall(rust_structural_source(restricted))), 1)

        startup = source.split("pub(crate) fn finite_native_startup_available() -> bool {", 1)[1]
        startup = startup.split("\n}\n", 1)[0]
        self.assertIn(
            "libc::syscall(libc::SYS_clone3, std::ptr::null::<libc::c_void>(), 0usize)",
            startup,
        )
        self.assertEqual(len(pattern.findall(rust_structural_source(startup))), 1)

        launch_path = path.with_suffix("") / "linux.rs"
        self.assertIn(launch_path, self.module_paths)
        launch = launch_path.read_text(encoding="utf-8").split(
            "pub(super) fn spawn(&self, domain: BorrowedFd<'_>)", 1
        )[1]
        launch = launch.split("\n    }\n", 1)[0]
        self.assertIn("flags: CLONE_PIDFD | CLONE_INTO_CGROUP,", launch)
        self.assertIn("cgroup: domain.as_raw_fd() as u64,", launch)
        self.assertIn(
            "libc::syscall(libc::SYS_clone3, &args, std::mem::size_of::<CloneArgs>())",
            launch,
        )
        self.assertEqual(len(pattern.findall(rust_structural_source(launch))), 1)
        return {self.relative(path): 4, self.relative(launch_path): 1}

    def test_promised_syntax_forms_hit_structural_sentinels(self) -> None:
        patterns = {
            row["id"]: re.compile(row["pattern"])
            for row in self.catalog["module_structures"]
        }
        for row in self.catalog["contract_cases"]:
            source = rust_structural_source(row["source"])
            for structure_id in row["must_trigger"]:
                with self.subTest(contract_case=row["id"], structure=structure_id):
                    self.assertIsNotNone(
                        patterns[structure_id].search(source),
                        f"{row['id']} no longer triggers {structure_id}",
                    )

    def test_ordinary_argument_groups_do_not_manufacture_calls(self) -> None:
        patterns = {
            row["id"]: re.compile(row["pattern"])
            for row in self.catalog["module_structures"]
        }
        for row in self.catalog["negative_cases"]:
            source = rust_structural_source(row["source"])
            for structure_id in row["must_not_trigger"]:
                with self.subTest(negative_case=row["id"], structure=structure_id):
                    self.assertIsNone(
                        patterns[structure_id].search(source),
                        f"{row['id']} manufactured {structure_id}",
                    )

    def test_every_named_entrypoint_is_live_and_unique(self) -> None:
        for row in self.catalog["entrypoints"]:
            with self.subTest(entrypoint=row["id"]):
                self.assertEqual(
                    self.source.count(row["anchor"]),
                    1,
                    f"{row['id']} lost or duplicated {row['anchor']!r}",
                )


if __name__ == "__main__":
    unittest.main()
