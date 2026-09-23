"""One definition of what a test looks like, for both audits that ask.

`validation.history` asks it of a *diff* — did this commit add something
test-shaped? `validation.regression_field` asks it of a *file* — does this
tree really define the test a `Regression-Test:` field names? Those are two
questions, but they have to agree about what a test is: a marker set that
drifts between them would let a field name something the history audit would
not have counted, or refuse something it would. So both live here and neither
module carries its own copy.
"""

from __future__ import annotations

import re


# Added lines that look like a test. Used against `git show --unified=0`
# output, so every line still carries its leading `+`.
TEST_ADDITION_RE = re.compile(
    r"^\+.*(?:#\[test\]|func test|@Test|assert(?:_eq|_ne)?!|XCTAssert|expect\(|"
    r"test\(|describe\(|it\(|\.golden)",
    re.MULTILINE,
)

# The attribute or annotation that marks the declaration below it as a test.
_TEST_ATTRIBUTE_RE = re.compile(
    r"^\s*(?:#\[(?:tokio::)?test\]|#\[test\]|@Test\b|#\[rstest\])"
)

# A declaration of a routine named `<name>`, in the five languages this
# repository's suites are written in: Rust (`fn`), Swift (`func`),
# Kotlin (`fun`), Java (`void`) and Python (`def`).
_DECLARATION_TEMPLATE = r"^\s*(?:pub\s+)?(?:async\s+)?(?:private\s+|public\s+)?(?:fn|func|fun|void|def)\s+{name}\s*[(<]"

# A Node suite names its test in a string literal: `it("…")`, `test("…")`,
# `describe("…")`.
_NODE_TEMPLATE = r"(?:^|\W)(?:it|test|describe)\s*\(\s*(['\"`]){name}\1"

# How far above a declaration the marker may sit. Doc comments and `#[cfg]`
# attributes routinely separate `#[test]` from `fn`, so this is not one line.
_MARKER_LOOKBACK = 6


def defines_test(source: str, name: str) -> bool:
    """Does `source` define a test called `name`?

    True when either the Node form names it in a string literal, or a
    declaration of `name` sits under a test attribute, or the declaration's
    own name begins with `test` (Swift's `func test…`, Python's `def test_…`,
    and the Rust convention this repository follows for `mod tests`).
    """

    escaped = re.escape(name)
    if re.search(_NODE_TEMPLATE.format(name=escaped), source):
        return True
    declaration = re.compile(
        _DECLARATION_TEMPLATE.format(name=escaped), re.MULTILINE
    )
    lines = source.splitlines()
    for match in declaration.finditer(source):
        index = source.count("\n", 0, match.start())
        if name.startswith("test"):
            return True
        start = max(0, index - _MARKER_LOOKBACK)
        if any(_TEST_ATTRIBUTE_RE.match(line) for line in lines[start:index]):
            return True
    return False
