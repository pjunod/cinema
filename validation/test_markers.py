"""One definition of what a test looks like, for both audits that ask.

`validation.history` asks it of a *diff* — did this commit add something
test-shaped? `validation.regression_field` and the merge-trailer audit ask it
of a *file* — does this tree really define the test a `Regression-Test:`
field names? Those are two questions, but they have to agree about what a
test is: a marker set that drifts between them would let a field name
something the history audit would not have counted, or refuse something it
would. So both live here and neither module carries its own copy.
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
# `#[tokio::test(flavor = "multi_thread")]` is how this repository spells a
# multi-threaded async test, and it is often wrapped over several lines, so
# the attribute is recognised by its opening alone.
_TEST_MARKER = r"(?:#\[(?:tokio::)?test(?:\]|\()|#\[(?:rstest|wasm_bindgen_test)\]|@Test\b)"
_TEST_ATTRIBUTE_RE = re.compile(r"^\s*" + _TEST_MARKER)
_TEST_MARKER_RE = re.compile(_TEST_MARKER)

# Any routine declaration at all. A line that carries one is another item,
# even when it opens with an annotation (`@Test fun other() { … }`), so the
# walk up from a declaration stops there rather than borrowing its marker.
_ANY_DECLARATION_RE = re.compile(r"\b(?:fn|func|fun|void|def)\s+\w")
_COMMENT_RE = re.compile(r"^\s*(?://|/?\*|#(?!\[))")

# A declaration of a routine named `<name>`, in the five languages this
# repository's suites are written in: Rust (`fn`), Swift (`func`),
# Kotlin (`fun`), Java (`void`) and Python (`def`). Attributes and
# annotations may sit on the declaration's own line (`@Test fun name()`,
# `#[test] fn name()`); they are captured so that their marker counts.
_DECLARATION_TEMPLATE = (
    r"^[ \t]*(?P<attributes>(?:(?:@[\w.]+(?:\([^)\n]*\))?|#\[[^\]\n]*\])[ \t]*)*)"
    r"(?P<modifiers>(?:(?:pub(?:\([^)]*\))?|async|unsafe|const|private|public|internal"
    r"|fileprivate|protected|static|final|open|override|suspend|mutating|nonisolated)[ \t]+)*)"
    r"(?P<keyword>fn|func|fun|void|def)[ \t]+{name}[ \t]*[(<]"
)

# Where the name alone is the marker: XCTest runs every parameterless
# instance `func test…()` of a test case, and unittest every `def test…`.
# (`static func testing(…)` is a production factory, not a test.) In Rust,
# Kotlin and Java a
# `test…` name is only a name -- this repository has dozens of Rust
# `fn test_…` fixture builders inside `mod tests` -- so there the attribute
# or annotation is required.
_NAME_IS_THE_MARKER = frozenset({"func", "def"})

# A Node suite names its test in a string literal: `it("…")`, `test("…")`,
# `describe("…")`.
_NODE_TEMPLATE = r"(?:^|\W)(?:it|test|describe)\s*\(\s*(['\"`]){name}\1"

# How far above a declaration its marker may sit. Doc comments and `#[cfg]`
# attributes routinely separate `#[test]` from `fn`, and a wrapped
# `#[tokio::test(...)]` takes several lines of its own.
_MARKER_LOOKBACK = 12

# A code line that ends an earlier item. The walk up from a declaration stops
# at the first one, so the `#[test]` of the function above cannot be borrowed
# by a helper declared below it.
_ITEM_END_RE = re.compile(r"[;{}]\s*$")
_ANNOTATION_OR_COMMENT = ("#", "@", "//", "*", "/*")


def _named_as_a_test(source: str, match: re.Match[str], name: str) -> bool:
    keyword = match.group("keyword")
    if keyword not in _NAME_IS_THE_MARKER or not name.startswith("test"):
        return False
    if keyword == "func":
        return "static" not in match.group("modifiers").split() and bool(
            re.match(r"\(\s*\)", source[match.end() - 1 :])
        )
    return True


def defines_test(source: str, name: str) -> bool:
    """Does `source` define a test called `name`?

    True when either the Node form names it in a string literal, or a
    declaration of `name` carries or sits under a test attribute, or the
    language makes the name itself the marker (Swift's `func test…`,
    Python's `def test_…`).
    """

    escaped = re.escape(name)
    if re.search(_NODE_TEMPLATE.format(name=escaped), source):
        return True
    declaration = re.compile(
        _DECLARATION_TEMPLATE.format(name=escaped), re.MULTILINE
    )
    lines = source.splitlines()
    for match in declaration.finditer(source):
        if _named_as_a_test(source, match, name):
            return True
        if _TEST_MARKER_RE.search(match.group("attributes")):
            return True
        index = source.count("\n", 0, match.start())
        for line in reversed(lines[max(0, index - _MARKER_LOOKBACK) : index]):
            if _ANY_DECLARATION_RE.search(line) and not _COMMENT_RE.match(line):
                break
            if _TEST_ATTRIBUTE_RE.match(line):
                return True
            stripped = line.strip()
            if _ITEM_END_RE.search(stripped) and not stripped.startswith(
                _ANNOTATION_OR_COMMENT
            ):
                break
    return False
