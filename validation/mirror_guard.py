"""The public-mirror gate every Forgejo workflow job carries, and how to read past it.

The GitHub mirror is a verbatim push of every ref, so each workflow also exists
on github.com. Every job of a Forgejo workflow therefore starts its job-level
`if` with `github.server_url != 'https://github.com'`; Forgejo's server URL is
never github.com, so on Forgejo the gate is true and nothing changes.

Receipts and reviewed digests pin the bytes of a workflow's recipe. The gate is
not part of that recipe, so every read of a CI workflow for a digest, a
projection or a comparison goes through `strip`, which removes exactly the
four forms the gate takes and nothing else. A workflow read before the gate
existed passes through unchanged, so one reviewed digest names the same recipe
on both sides of the change.
"""

import re

GATE = b"github.server_url != 'https://github.com'"
GITHUB_RUN = frozenset({"github-release.yml", "desktop-qualification.yml", "media-runtime-public.yml"})
PREFIX = ".github/workflows/"

# `    if: <gate>` alone; `    if: <gate> && (<x>)`; `    if: ${{ <gate> && (<x>) }}`;
# and a folded block `if: >-` whose first line is `<gate> && (` and whose last ends ` )`.
_ALONE = re.compile(rb"(?m)^    if: " + re.escape(GATE) + rb"\n")
_PLAIN = re.compile(rb"(?m)^    if: " + re.escape(GATE) + rb" && \((.*)\)$")
_DOLLAR = re.compile(rb"(?m)^    if: \$\{\{ " + re.escape(GATE) + rb" && \((.*)\) \}\}$")
_FOLDED = re.compile(
    rb"(?m)^(    if: >-?\n)      " + re.escape(GATE) + rb" && \(\n((?:      .*\n)*?)(      .*?) \)\n"
)


def is_ci_workflow(path):
    if not isinstance(path, str) or not path.startswith(PREFIX):
        return False
    name = path[len(PREFIX):]
    return bool(name) and "/" not in name and name not in GITHUB_RUN


def strip(content):
    """`content` without the mirror gate; bytes in, bytes out (str works too)."""
    if isinstance(content, str):
        return strip(content.encode()).decode()
    content = _ALONE.sub(b"", content)
    content = _FOLDED.sub(rb"\1\2\3\n", content)
    content = _PLAIN.sub(rb"    if: \1", content)
    content = _DOLLAR.sub(rb"    if: ${{ \1 }}", content)
    return content


def strip_if_workflow(path, content):
    return strip(content) if content is not None and is_ci_workflow(path) else content


def read_file(path, binary=False):
    """A CI workflow from the working tree, gate removed (for tests and tools)."""
    with open(path, "rb") as handle:
        raw = strip(handle.read())
    return raw if binary else raw.decode("utf-8")
