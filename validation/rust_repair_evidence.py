"""Inventory-bound libtest accounting, including reviewed nested child protocols.

Nested output is admitted only for source files pinned in the repair manifest.
Every remaining native identity and footer must still agree exactly. Unknown
output structure is incomplete evidence, never permission to discard a result.
"""
from collections import Counter
import hashlib
import json
from pathlib import Path
import re

from validation.baseline_timing import ANSI, DOC_TESTS, RESULT, RUNNING

HEADER = re.compile(r"^running (\d+) tests?$")
OUTCOME = re.compile(r"^test (.+?) \.\.\. (ok|FAILED|ignored)(?:,.*)?$")
BASELINE_CASES_SHA256 = "22bdbf49112084882ac4b135287d28290257be96b274be04791695b741a2c5c2"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def protocol_sources(root, policy, prefix):
    paths = {name: digest for name, digest in policy["nested_source_sha256"].items()
             if name.startswith(prefix)}
    require(paths, "Nested protocol has no reviewed source owner")
    root = Path(root).resolve()
    for name, digest in paths.items():
        path = (root / name).resolve()
        require(path.is_relative_to(root) and path.is_file(), "Nested owner escaped source root")
        require(hashlib.sha256(path.read_bytes()).hexdigest() == digest,
                "Nested output owner changed; review its protocol before qualification: " + name)


def native_name(match):
    return match[1].removesuffix(" - should panic")


def reviewed_rows(lines, binary, full_cases, root, policy):
    """Remove only source-bound diagnostic replays/delegated native children."""
    excluded = set()
    blocks = []
    target = binary["target"]["name"]
    for index, text in enumerate(lines):
        if text == "--- restore child stdout.log tail ---":
            require(target == "plurx_core", "Restore replay outside its owning binary")
            protocol_sources(root, policy, "crates/plurx-core/src/cluster/")
            stop = next((n for n in range(index + 1, len(lines))
                         if lines[n] == "--- restore child stderr.log tail ---"), None)
            require(stop is not None, "Unclosed restore diagnostic replay")
            payload = lines[index + 1:stop]
            require(not any(RUNNING.fullmatch(row.strip()) or DOC_TESTS.fullmatch(row.strip())
                            for row in payload), "Cargo banner inside child replay")
            for row in payload:
                event = OUTCOME.fullmatch(row)
                if event:
                    require(native_name(event) in policy["restore_children"],
                            "Unrelated native outcome inside restore replay")
            excluded.update(range(index, stop))
            blocks.append({"kind": "restore_stdout_replay", "start": index + 1, "end": stop})
        if target == "plurx_tray" and HEADER.fullmatch(text) and text == "running 1 test" and len(full_cases) > 1:
            protocol_sources(root, policy, "crates/plurx-tray/src/")
            stop = next((n for n in range(index + 1, min(index + 9, len(lines)))
                         if RESULT.fullmatch(lines[n])), None)
            require(stop is not None, "Incomplete inherited Tray child")
            payload = lines[index + 1:stop]
            events = [OUTCOME.fullmatch(row) for row in payload if OUTCOME.fullmatch(row)]
            require(len(events) == 1 and native_name(events[0]) in policy["tray_children"],
                    "Unknown inherited Tray child identity")
            summary = RESULT.fullmatch(lines[stop])
            require(int(summary[2]) + int(summary[3]) == 1 and summary.group(4, 5) == ("0", "0")
                    and int(summary[6]) == len(full_cases) - 1,
                    "Inherited Tray child footer differs from its exact selection")
            require((events[0][2] == "ok") == (summary[2] == "1"),
                    "Inherited Tray child status differs from footer")
            excluded.update(range(index, stop + 1))
            blocks.append({"kind": "inherited_tray_child", "name": native_name(events[0]),
                           "start": index + 1, "end": stop + 1})
    return [line for n, line in enumerate(lines) if n not in excluded], blocks


def one_suite(lines, expected, *, execute_ignored=False, filtered=0):
    headers = [HEADER.fullmatch(line) for line in lines if HEADER.fullmatch(line)]
    footers = [RESULT.fullmatch(line) for line in lines if RESULT.fullmatch(line)]
    rows = [{"name": native_name(m), "status": m[2]} for line in lines
            if (m := OUTCOME.fullmatch(line))]
    require(len(headers) == len(footers) == 1, "Missing/duplicate native header or footer")
    header_position = next(n for n, line in enumerate(lines) if HEADER.fullmatch(line))
    footer_position = next(n for n, line in enumerate(lines) if RESULT.fullmatch(line))
    require(header_position < footer_position and all(
        header_position < n < footer_position for n, line in enumerate(lines)
        if OUTCOME.fullmatch(line)), "Native outcomes are outside their header/footer")
    footer = footers[0]
    counts = dict(zip(["passed", "failed", "ignored", "measured", "filtered_out"],
                      map(int, footer.groups()[1:6])))
    require(int(headers[0][1]) == len(rows), "Native declared/outcome count differs")
    require(len({row["name"] for row in rows}) == len(rows), "Duplicate native case outcome")
    if expected is not None:
        names = {case["name"]: case for case in expected}
        require(set(names) == {row["name"] for row in rows}, "Missing/unexpected exact native identity")
        for row in rows:
            ignored = names[row["name"]]["ignored"] and not execute_ignored
            require((row["status"] == "ignored") == ignored, "Native ignore/body disposition differs")
    observed = Counter(row["status"] for row in rows)
    require(all(observed[key] == counts[field] for key, field in
                [("ok", "passed"), ("FAILED", "failed"), ("ignored", "ignored")]),
            "Native footer counts differ from exact outcomes")
    require(counts["measured"] == 0 and counts["filtered_out"] == filtered,
            "Unexpected measured/filtered native cases")
    require((footer[1] == "ok") == (counts["failed"] == 0), "Native verdict differs from failed count")
    return dict(declared_tests=len(rows), outcomes=rows, result=dict(counts, reported_s=float(footer[7])),
                status="passed" if footer[1] == "ok" else "failed")


def parse_bound(text, inventory, *, root, policy, selected=None, execute_ignored=False,
                doctest_targets=(), direct_binary=None):
    """Return exact accounting; source/hash checks outside this parser remain required.

    selected maps executable basenames to exact case names for a filtered run.
    None means the complete prelisted binary inventory, including empty targets.
    """
    lines = [ANSI.sub("", row).strip() for row in text.splitlines()]
    all_binaries = {Path(b["executable"]).name: b for b in inventory["binaries"]}
    require(inventory.get("discovery_complete") is True and all_binaries,
            "Native pre-execution discovery is incomplete or empty")
    require(len(all_binaries) == len(inventory["binaries"]), "Duplicate inventoried executable")
    wanted = all_binaries if selected is None else {name: all_binaries[name] for name in selected}
    require(wanted, "Selected native binary universe is empty")
    sections = []
    if direct_binary:
        require(direct_binary in wanted and len(wanted) == 1, "Direct binary lacks exact selected binding")
        sections = [(direct_binary, lines)]
    else:
        for text_line in lines:
            native, doc = RUNNING.fullmatch(text_line), DOC_TESTS.fullmatch(text_line)
            if native or doc:
                sections.append((Path(native[2]).name if native else "doctest:" + doc[1], []))
            elif sections:
                sections[-1][1].append(text_line)
    expected_sections = set(wanted) | {"doctest:" + target for target in doctest_targets}
    gaps, suites, nested = [], [], []
    if Counter(name for name, _ in sections) != Counter({name: 1 for name in expected_sections}):
        gaps.append("Missing, extra or duplicate native binary/doctest sections")
    for name, payload in sections:
        try:
            require(name in expected_sections, "Unexpected native section: " + name)
            if name.startswith("doctest:"):
                suite = one_suite(payload, None)
            else:
                binary = wanted[name]
                expected = binary["cases"]
                if selected is not None:
                    requested = set(selected[name])
                    require(len(requested) == len(selected[name]), "Duplicate selected native case")
                    expected = [case for case in expected if case["name"] in requested]
                    require(len(expected) == len(requested), "Selected case missing from inventory")
                payload, removed = reviewed_rows(payload, binary, binary["cases"], root, policy)
                nested.extend(dict(binary=name, **row) for row in removed)
                suite = one_suite(payload, expected, execute_ignored=execute_ignored,
                                  filtered=len(binary["cases"]) - len(expected))
            suites.append(dict(binary=name, **suite))
        except (ValueError, OSError, KeyError) as error:
            gaps.append({"binary": name, "reason": str(error)})
    return dict(qualification_complete=not gaps, gaps=gaps, observed_suites=suites,
                native_passed=not gaps and all(suite["status"] == "passed" for suite in suites),
                nested_protocol_observations=nested)


def load_policy(path):
    policy = json.loads(Path(path).read_text())
    require(policy["required_repetitions"] == 10 and len(policy["cases"]) == 25,
            "Repair cohort must preserve the declared 25 failures and ten runs")
    require(len({(row["target"], row["name"]) for row in policy["cases"]}) == 25,
            "Repair cohort contains duplicate identities")
    identities = sorted((row["target"], row["name"]) for row in policy["cases"])
    require(hashlib.sha256(json.dumps(identities, separators=(",", ":")).encode()).hexdigest()
            == BASELINE_CASES_SHA256, "Original failing identities changed")
    return policy
