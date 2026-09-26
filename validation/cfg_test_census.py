#!/usr/bin/env python3
"""Classify every literal ``#[cfg(test)]`` in Rust sources by its target."""

import collections
import pathlib
import re
import sys


ROOT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".")
CLASSES = [
    ("module", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*[;{]")),
    ("use", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?use\s")),
    (
        "type",
        re.compile(
            r"^\s*(pub(\([^)]*\))?\s+)?(struct|enum|type|trait|const|static)\s"
        ),
    ),
    ("impl", re.compile(r"^\s*impl\b")),
    ("fn", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s")),
    ("field", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?\w+\s*:\s*[^=]")),
    (
        "statement",
        re.compile(r"^\s*(let|if|match|for|while|loop|return|\w+[\.\(]|\{|\*)"),
    ),
]


def classify(line: str) -> str:
    """Return the first line-oriented class matching the gated item."""
    for name, pattern in CLASSES:
        if pattern.search(line):
            return name
    return "other"


total = collections.Counter()
per_file = collections.defaultdict(collections.Counter)
for path in sorted(ROOT.glob("crates/*/src/**/*.rs")):
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    for index, line in enumerate(lines):
        if not re.match(r"^\s*#\[cfg\(test\)\]\s*$", line):
            continue
        target = index + 1
        while target < len(lines) and (
            lines[target].strip().startswith(("#[", "//"))
            or not lines[target].strip()
        ):
            target += 1
        item_class = classify(lines[target] if target < len(lines) else "")
        total[item_class] += 1
        per_file[str(path.relative_to(ROOT))][item_class] += 1

print("class,count")
for item_class, count in total.most_common():
    print(f"{item_class},{count}")
print("file,field,statement")
ordered_files = sorted(
    per_file.items(),
    key=lambda item: -(item[1]["field"] + item[1]["statement"]),
)
for relative_path, counts in ordered_files[:12]:
    print(f"{relative_path},{counts['field']},{counts['statement']}")
