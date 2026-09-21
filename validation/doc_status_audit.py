"""Report docs/README.md status rows that disagree with document headers.

Free prose cannot be a hard gate: this audit narrows the review queue and
leaves every disposition to a reader. The JSON form exists so plans can record
and refresh counts without scraping terminal prose.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import json
from pathlib import Path
import re
from typing import Any

from validation.mobile_versions import REPO_ROOT


_ROW = re.compile(
    r"^\|\s*(.*?)\s*\|\s*(.*?)\s*\|\s*(live|open|built|done|superseded)\s*\|\s*$"
)
_LINK = re.compile(r"\[[^\]]+\]\(([^)#]+)")
_STATUS = re.compile(
    r"^[ \t]*>?[ \t]*\*\*Status(?:[ \t]*\([^\n]*?\))?:?\*\*"
    r"[ \t]*:?[ \t]*(.*)$",
    re.IGNORECASE | re.MULTILINE,
)
_TERMINAL = re.compile(
    r"\b(?:built|complete|done|executed|landed|merged|shipped|superseded)\b"
    r"|^live\b|^M\d+\s+accepted\b",
    re.IGNORECASE,
)
_OPEN_QUALIFIER = re.compile(
    r"\b(?:pending|open|in progress|in execution|unbuilt|not deployed|not shipped)\b"
    r"|\bnot\s+built\b|\bcannot\b",
    re.IGNORECASE,
)
_NONTERMINAL = re.compile(
    r"\b(?:ready to build|ready to run|decision needed|pending|open|awaiting)\b",
    re.IGNORECASE,
)
_BROAD_NONTERMINAL = re.compile(
    r"\b(?:awaiting|blocked|building|decision needed|draft|executing|"
    r"in progress|in execution|not started|open|pending|planned|proposed|"
    r"ready|review|under review|unbuilt|unfinished)\b",
    re.IGNORECASE,
)
_BROAD_TERMINAL = re.compile(
    r"\b(?:accepted|built|complete|done|executed|implemented|landed|merged|"
    r"reconciled|resolved|shipped|superseded)\b|^live\b",
    re.IGNORECASE,
)
_NEXT_FIELD = re.compile(r"\s*·\s*\*\*[^*]+:\*\*")
_IMPOSSIBLE_COMPOSITES = (
    re.compile(
        r"\b(?:built|complete|done|implemented|landed|merged|shipped|"
        r"superseded)\b.*\bready to (?:build|run)\b",
        re.IGNORECASE,
    ),
    re.compile(
        r"\bready to (?:build|run)\b.*\b(?:built|complete|done|implemented|"
        r"landed|merged|shipped|superseded)\b",
        re.IGNORECASE,
    ),
    re.compile(r"\bdone\b.*\bawaiting\b", re.IGNORECASE),
    re.compile(
        r"\bbuilt\b.*\b(?:validation and merge|merge|promotion) pending\b",
        re.IGNORECASE,
    ),
)


@dataclass(frozen=True)
class Finding:
    index_line: int
    index_status: str
    document: str
    header: str | None


def _status_clause(text: str) -> str | None:
    """Return the complete Status value, including wrapped continuation lines."""

    match = _STATUS.search(text)
    if match is None:
        return None

    lines = text.splitlines()
    line_number = text[: match.start()].count("\n")
    pieces = [match.group(1).strip()]
    for line in lines[line_number + 1 :]:
        stripped = line.strip().removeprefix(">").strip()
        if not stripped or re.match(r"^\*\*[^*]+:\*\*", stripped):
            break
        pieces.append(stripped)
        if _NEXT_FIELD.search(stripped):
            break

    value = " ".join(piece for piece in pieces if piece)
    return _NEXT_FIELD.split(value, maxsplit=1)[0].strip()


def _is_impossible_composite(header: str) -> bool:
    return any(pattern.search(header) for pattern in _IMPOSSIBLE_COMPOSITES)


def audit(root: Path = REPO_ROOT) -> dict[str, Any]:
    index_path = root / "docs" / "README.md"
    contradictions: list[Finding] = []
    missing_headers: list[Finding] = []
    unclear: list[Finding] = []
    rows = 0

    for line_number, line in enumerate(
        index_path.read_text(encoding="utf-8").splitlines(), 1
    ):
        row = _ROW.match(line)
        if row is None:
            continue
        link = _LINK.search(row.group(1))
        if link is None:
            continue
        rows += 1
        index_status = row.group(3)
        relative_path = link.group(1)
        if not relative_path.endswith(".md"):
            continue
        document = root / "docs" / relative_path
        header = _status_clause(document.read_text(encoding="utf-8"))
        if header is None:
            missing_headers.append(
                Finding(line_number, index_status, relative_path, None)
            )
            continue

        terminal = bool(_TERMINAL.search(header))
        nonterminal = bool(_NONTERMINAL.search(header))
        contradiction = (
            index_status == "open"
            and terminal
            and not _OPEN_QUALIFIER.search(header)
        ) or (
            index_status in {"built", "done", "superseded"}
            and nonterminal
            and not terminal
        ) or (
            index_status in {"built", "done", "superseded"}
            and _is_impossible_composite(header)
        )
        finding = Finding(line_number, index_status, relative_path, header)
        if contradiction:
            contradictions.append(finding)
            continue

        broadly_terminal = bool(_BROAD_TERMINAL.search(header))
        broadly_nonterminal = bool(_BROAD_NONTERMINAL.search(header))
        clear = (
            index_status == "live"
            or (index_status == "open" and broadly_nonterminal)
            or (
                index_status in {"built", "done", "superseded"}
                and broadly_terminal
            )
        )
        if not clear:
            unclear.append(finding)

    return {
        "rows": rows,
        "contradictions": [asdict(item) for item in contradictions],
        "missing_headers": [asdict(item) for item in missing_headers],
        "unclear": [asdict(item) for item in unclear],
    }


def _print_findings(label: str, findings: list[dict[str, Any]]) -> None:
    print(f"{label}: {len(findings)}")
    for finding in findings:
        header = finding["header"] or "<no status header>"
        print(
            f"  README:{finding['index_line']} {finding['document']}: "
            f"row={finding['index_status']}; header={header}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit JSON")
    arguments = parser.parse_args()
    report = audit()
    if arguments.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(f"indexed status rows: {report['rows']}")
        _print_findings("contradictions", report["contradictions"])
        _print_findings("missing status headers", report["missing_headers"])
        _print_findings("unclear", report["unclear"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
