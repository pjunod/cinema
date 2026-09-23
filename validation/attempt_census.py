"""Census of the hand-written epoch comparisons in `PlayerController.swift`.

`PlayerController` carries nine independent epoch counters (the table is in
`docs/clients/APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md` §2.1). A
continuation that crosses an `await` has to decide which of the nine
invalidate the work it was going to do, and for most of the controller's
history that decision was spelled as a conjunction written out by hand at the
continuation — `self.openGeneration == g && self.viewerActionEpoch == e && …`.
Nothing recorded which subset was intended, so nothing could tell a fence that
omits a counter on purpose from one that omits it by accident, and the next
`Task {}` had to rediscover the answer.

`Attempt.stillCurrent(_:scopes:)` gives that subset a name. This census is what
makes the migration to it finishable: it lists every remaining hand-written
comparison, checks the list against an allow-list committed beside it, and
fails when a comparison appears that is not on the list. Removing a row from
the allow-list is therefore the only way to shrink it, and re-introducing a
hand-written fence anywhere in the file fails the audit rather than passing
unnoticed.

The audit is deliberately textual. It reads Swift this environment cannot
compile, so it claims nothing about types — only about which identifiers are
compared with `==` or `!=`, and in which function.
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from pathlib import Path
import re
import tomllib


ROOT = Path(__file__).resolve().parents[1]
SOURCE = "clients/apple/Sources/PlayerController.swift"
ATTEMPT_SOURCE = "clients/apple/Sources/PlayerAttempt.swift"
ALLOWLIST = "validation/attempt-census.toml"

#: Every epoch counter a fence may compare, mapped to the `Attempt.Scope` that
#: names it. `createRetryExpiredEpoch` maps to the same scope as
#: `createRetryEpoch` because it is that counter's expiry latch, not a tenth
#: epoch — see the `Scope` doc comment in PlayerAttempt.swift.
EPOCH_SCOPES = {
    "lifecycleGeneration": "lifecycle",
    "openGeneration": "open",
    "viewerActionEpoch": "viewerAction",
    "initialDecisionGeneration": "initialDecision",
    "createRetryEpoch": "createRetry",
    "createRetryExpiredEpoch": "createRetry",
    "preparedAlignmentGeneration": "preparedAlignment",
    "seekState.generation": "seek",
    "pgsOverlaySelectionGeneration": "pgsSelection",
    "pgsOverlayItemGeneration": "pgsItem",
}

_OPERAND = r"[A-Za-z_][A-Za-z0-9_.?]*"
COMPARISON_RE = re.compile(rf"(?P<lhs>{_OPERAND})\s*(?P<op>==|!=)\s*(?P<rhs>{_OPERAND})")
FUNC_RE = re.compile(r"\bfunc\s+(?P<name>[A-Za-z_]\w*)")
SCOPE_CASE_RE = re.compile(r"^\s*case\s+(?P<name>[A-Za-z_]\w*)\s*$", re.MULTILINE)
STILL_CURRENT_RE = re.compile(r"stillCurrent\((?P<args>[^()]*(?:\([^()]*\)[^()]*)*)\)", re.DOTALL)
SCOPES_ARG_RE = re.compile(r"scopes:\s*\[(?P<scopes>[^\]]*)\]", re.DOTALL)


@dataclass(frozen=True)
class Comparison:
    """One hand-written epoch comparison, keyed by where and what it compares."""

    function: str
    expression: str

    @property
    def key(self) -> str:
        return f"{self.function}::{self.expression}"


def strip_comment(line: str) -> str:
    """The code on a line, with any `//` tail and whole-comment lines removed.

    Prose about a fence must not be counted as one: several of the comparisons
    this file is about are quoted in the comments that explain them, including
    in the comments added by the migration itself.
    """
    stripped = line.strip()
    if stripped.startswith(("//", "/*", "*/", "*")):
        return ""
    head = line.split("//", 1)[0]
    return head.split("/*", 1)[0]


def _is_epoch(operand: str) -> str | None:
    """The scope an operand names, or None if it names no epoch."""
    name = operand
    if name.startswith("self."):
        name = name[len("self."):]
    if name in EPOCH_SCOPES:
        return EPOCH_SCOPES[name]
    for epoch, scope in EPOCH_SCOPES.items():
        if name.endswith("." + epoch) or name.endswith("?." + epoch):
            return scope
    return None


def census(text: str) -> tuple[Comparison, ...]:
    """Every hand-written epoch comparison in `text`, in file order."""
    found: list[Comparison] = []
    function = "<file scope>"
    for raw in text.splitlines():
        match = FUNC_RE.search(strip_comment(raw))
        if match:
            function = match.group("name")
        code = strip_comment(raw)
        if not code:
            continue
        for comparison in COMPARISON_RE.finditer(code):
            lhs = comparison.group("lhs")
            rhs = comparison.group("rhs")
            if _is_epoch(lhs) is None and _is_epoch(rhs) is None:
                continue
            expression = f"{lhs} {comparison.group('op')} {rhs}"
            found.append(Comparison(function=function, expression=expression))
    return tuple(found)


def declared_scopes(attempt_source: str) -> tuple[str, ...]:
    """The case names of `Attempt.Scope`, in declaration order."""
    start = attempt_source.find("enum Scope")
    if start < 0:
        return ()
    end = attempt_source.find("\n    }", start)
    body = attempt_source[start:end if end > 0 else len(attempt_source)]
    return tuple(match.group("name") for match in SCOPE_CASE_RE.finditer(body))


def code_only(text: str) -> str:
    """`text` with every comment removed, so prose about a call is not a call."""
    return "\n".join(strip_comment(line) for line in text.splitlines())


def named_scope_sets(text: str) -> tuple[tuple[str, ...], ...]:
    """The scope list of every `stillCurrent(_:scopes:)` call site.

    Comments are removed first: this file documents the very API it calls, and
    a doc comment naming `stillCurrent(_:scopes:)` is not a call site.
    """
    sets: list[tuple[str, ...]] = []
    for call in STILL_CURRENT_RE.finditer(code_only(text)):
        arg = SCOPES_ARG_RE.search(call.group("args"))
        if arg is None:
            sets.append(())
            continue
        names = [piece.strip().lstrip(".") for piece in arg.group("scopes").split(",")]
        sets.append(tuple(name for name in names if name))
    return tuple(sets)


def load_allowlist(text: str) -> dict[str, int]:
    """The allowed fence keys mapped to how many times each may still appear.

    The count is what makes the list an inventory rather than a licence: a
    second copy of an already-excused conjunction in the same function is a new
    hand-written fence and is reported as one.
    """
    data = tomllib.loads(text)
    return dict(data.get("allowed", {}))


def audit(read) -> tuple[str, ...]:
    """Every way the census disagrees with what the repository claims.

    `read` takes a repository-relative path and returns its text, so a test can
    hand this function a source it has altered and watch the audit reject it.
    """
    errors: list[str] = []
    source = read(SOURCE)
    attempt_source = read(ATTEMPT_SOURCE)
    allowed = load_allowlist(read(ALLOWLIST))

    scopes = declared_scopes(attempt_source)
    expected = sorted(set(EPOCH_SCOPES.values()))
    if sorted(scopes) != expected:
        errors.append(
            f"{ATTEMPT_SOURCE} declares Attempt.Scope {sorted(scopes)}, but this census "
            f"maps the controller's epochs to {expected}. The nine counters and the nine "
            "scopes are the same nine; changing one without the other hides a fence."
        )

    counted = Counter(comparison.key for comparison in census(source))
    for key in sorted(set(counted) | set(allowed)):
        seen = counted.get(key, 0)
        budget = allowed.get(key, 0)
        if seen > budget:
            errors.append(
                f"{SOURCE}: `{key.split('::', 1)[1]}` appears {seen} time(s) in "
                f"`{key.split('::', 1)[0]}()` and {ALLOWLIST} allows {budget}. This fence "
                "compares an epoch by hand: name the scopes it depends on with "
                "`Attempt.stillCurrent(_:scopes:)`, or raise the row with the reason it "
                "cannot be migrated yet."
            )
        elif seen < budget:
            errors.append(
                f"{ALLOWLIST} allows `{key}` {budget} time(s) but {SOURCE} has {seen}. "
                "Lower the row to what is left: an allow-list that outlives what it excused "
                "stops being a record of what is still to do, and leaves room for a new "
                "hand-written fence to take the retired one's place unnoticed."
            )

    for index, named in enumerate(named_scope_sets(source)):
        if not named:
            errors.append(
                f"{SOURCE}: the stillCurrent call #{index + 1} names no scopes. A fence that "
                "depends on none of the nine epochs is not a fence."
            )
            continue
        for name in named:
            if name not in scopes:
                errors.append(
                    f"{SOURCE}: the stillCurrent call #{index + 1} names scope `.{name}`, "
                    f"which Attempt.Scope does not declare ({list(scopes)})."
                )
    return tuple(errors)


def repository_read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


if __name__ == "__main__":  # pragma: no cover - a reporting entry point
    for comparison in census(repository_read(SOURCE)):
        print(comparison.key)
