"""Audit corrective commits against current validation evidence."""

from __future__ import annotations

import argparse
from collections import Counter
import dataclasses
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from typing import Callable

from validation.runner import Catalog, REPO_ROOT, load_catalog, matches
from validation.test_markers import TEST_ADDITION_RE, defines_test


DEFAULT_COVERAGE = REPO_ROOT / "validation" / "regressions.d"
DEFAULT_CLIENT_FIXES = REPO_ROOT / "tests" / "client-fixes.toml"
DEFAULT_MERGE_LEDGER = REPO_ROOT / "validation" / "merge-errata.toml"
COVERAGE_FIELDS = frozenset({"commits", "points", "checks", "reason", "ignore"})
ISSUE_RE = re.compile(
    r"(^fix(?:\b|[(:])|^(?:perf(?:\([^)]*\))?:|address|hide|align|bind|delay|do not|fit|"
    r"harden|make .* (?:valid|playable)|map hls|normalize|put .* actually|"
    r"rebuild|refine|refresh|report (?:delivered|what)|reuse|right-align|route|"
    r"scope|signal|tighten|use latest)\b|"
    r"\b(?:bug|regression|crash|hang|freeze|broken|stale|race|wrong|failure|failed|"
    r"defect|bypass|lockout|clipped|clipping|fix(?:es|ed)?|stop(?:s|ped|ping)?|"
    r"avoid(?:s|ed|ing)?|correct(?:s|ed|ion)?|restore(?:s|d)?|prevent(?:s|ed|ing)?|"
    r"bound(?:s|ed|ing)?|keep(?:s|ing)?|remove(?:s|d|ing)?|withdraw(?:s|n)?|"
    r"omit(?:s|ted|ting)?|preserve(?:s|d|ing)?|repair(?:s|ed|ing)?|"
    r"refuse(?:s|d|ing)?|recover(?:y|ed|ing)?|survive(?:s|d|ing)?|fallback|"
    r"invalid|incompatible|compatible|truth|free each)\b|"
    r"^(?:roll back|revert unsupported|make .* fail|give .* back))",
    re.IGNORECASE,
)

# The narrow rule, from §3.2 of
# docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md: Conventional Commit
# corrective prefixes and nothing else.
#
# READ THIS BEFORE WIDENING IT. The set above is what this replaces, and it
# is wide on purpose-turned-accident: it matches body words, so a `docs:`
# commit whose subject contains "keeps" is corrective, and that is what
# produced the self-referential ledger fragments (a ledger-only commit mapped
# to the validation framework so that the check which demanded it is
# satisfied by it). Narrowing is not free. Measured over this repository's
# whole history at `fad591a4`, `ISSUE_RE` matches 2,028 subjects and this
# matches 1,241; 809 subjects stop being audited, and some of them really are
# corrective — `feat(scan): add bounded identity repair` carries a repair and
# `ci: restore complete watch integration preflight` restores a gate. A
# behaviour fix mislabelled `chore(` or `refactor(` now passes with no
# evidence. That escape is accepted, and AGENTS.md carries the rule that
# answers it: if a change alters behaviour a user could observe, its subject
# is `fix(` or `perf(`.
#
# The two rules are **not** nested. 22 subjects match this and not `ISSUE_RE`
# — every `perf(<scope>):` commit in the repository, because `ISSUE_RE`'s
# `perf` alternative is followed by a `\b` that a `:` and a space cannot
# satisfy. Applying this rule to the whole history would therefore make 22
# already-landed commits newly corrective and newly unevidenced. That is why
# the rule is chosen per commit by `_corrective_rule_for`, at the boundary,
# rather than swapped wholesale.
CORRECTIVE_RE = re.compile(r"^(fix|perf)(\([^)]*\))?[:!]")
# `Regression-Test: <path>::<name>` — the PR-level field of §3.1, carried onto
# the landing commit. Not a SHA, so a rebase cannot invalidate it. One parser
# reads both carriers, the pull request description before merge and the
# landing commit's message after it, so "the same line" means the same thing
# in both places: a line anywhere in the text, not only in a final trailer
# paragraph that a merge tool is free to reflow.
REGRESSION_TRAILER = "Regression-Test"
REGRESSION_FIELD_RE = re.compile(
    rf"^[ \t]*{REGRESSION_TRAILER}:[ \t]*(?P<value>\S.*?)[ \t]*$", re.MULTILINE
)
# The shapes a landing commit takes. The first two are
# `tests/operations/test_status_pr_claims.py`'s, so a squashed PR is covered
# as well as a Forgejo merge commit. The third is how an integration branch
# carries a plan PR — `Merge plan/C-08 (#461) at <sha>` — before the
# integration PR itself lands; its trailers count for the commits it brings.
MERGE_SUBJECT_RE = re.compile(r"^Merge pull request '(?P<title>.*)' \(#(?P<pull>\d+)\)")
SQUASH_SUBJECT_RE = re.compile(r"^(?P<title>.*) \(#(?P<pull>\d+)\)$")
INTEGRATION_SUBJECT_RE = re.compile(
    r"^Merge (?P<title>[^\s']+) \(#(?P<pull>\d+)\)(?: at [0-9a-f]{7,64})?$"
)


def parse_regression_fields(text: str) -> tuple[str, ...]:
    """Every `Regression-Test:` value in a description or a commit message."""

    return tuple(match.group("value") for match in REGRESSION_FIELD_RE.finditer(text))


class HistoryError(ValueError):
    """The regression coverage ledger is malformed."""


@dataclasses.dataclass(frozen=True)
class IssueCommit:
    sha: str
    subject: str
    paths: tuple[str, ...]
    point_ids: tuple[str, ...]
    direct_test_evidence: bool


@dataclasses.dataclass(frozen=True)
class CoverageEntry:
    origin: str
    commits: tuple[str, ...]
    points: tuple[str, ...]
    checks: tuple[str, ...]
    reason: str
    ignore: bool


@dataclasses.dataclass(frozen=True)
class ClientFixEntry:
    id: str
    commits: tuple[str, ...]
    source: str
    source_anchor: str
    test: str
    test_anchor: str


@dataclasses.dataclass(frozen=True)
class ClientFixLedger:
    enforce_after: str | None
    fixes: tuple[ClientFixEntry, ...]


@dataclasses.dataclass(frozen=True)
class MergeErratum:
    """One landing commit whose trailer is wrong and can never be amended."""

    commit: str
    reason: str


@dataclasses.dataclass(frozen=True)
class MergeLedger:
    """The boundary the freeze is measured from, and the permanent errata.

    `enforce_after` is the boundary commit of §3.1's phase table. Unset means
    phase A: every commit keeps the legacy corrective rule, `regressions.d/`
    still accepts new fragments, and the merge-trailer audit reports nothing.
    Set means phase B for everything past it, and phase C for everything at or
    before it.
    """

    enforce_after: str | None
    errata: tuple[MergeErratum, ...]


@dataclasses.dataclass(frozen=True)
class MergeRegression:
    """A landing commit past the boundary, and what its trailers resolved to."""

    landing: str
    pull: str
    title: str
    corrective: bool
    tests: tuple[str, ...]
    resolved: bool


@dataclasses.dataclass(frozen=True)
class HistoryReport:
    issues: tuple[IssueCommit, ...]
    covered_by_entry: tuple[str, ...]
    covered_by_anchor: tuple[str, ...]
    ignored: tuple[str, ...]
    errors: tuple[str, ...]
    merges: tuple[MergeRegression, ...] = ()
    pending: tuple[str, ...] = ()
    covered_by_trailer: tuple[str, ...] = ()

    @property
    def direct_count(self) -> int:
        return sum(issue.direct_test_evidence for issue in self.issues)

    @property
    def mapped_count(self) -> int:
        return len(self.covered_by_entry)

    @property
    def ignored_count(self) -> int:
        return len(self.ignored)

    @property
    def anchored_count(self) -> int:
        return len(self.covered_by_anchor)


def _strings(value: object, field: str) -> tuple[str, ...]:
    if value is None:
        return ()
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise HistoryError(f"{field} must be an array of strings")
    return tuple(value)


def _commit_prefixes(value: object, field: str) -> tuple[str, ...]:
    prefixes = _strings(value, field)
    if not all(re.fullmatch(r"[0-9a-f]{7,40}", prefix) for prefix in prefixes):
        raise HistoryError(f"{field} must contain Git SHA prefixes")
    return prefixes


def _load_coverage_fragment(path: Path) -> CoverageEntry:
    name = path.name
    try:
        with path.open("rb") as handle:
            raw = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise HistoryError(f"cannot read {path}: {exc}") from exc
    if raw.get("version") != 1:
        raise HistoryError(f"{name} regression coverage version must be 1")
    stray = set(raw) - {"version", "coverage"}
    if stray:
        raise HistoryError(f"{name} has unknown keys {', '.join(sorted(stray))}")

    items = raw.get("coverage")
    if not isinstance(items, list) or len(items) != 1:
        raise HistoryError(f"{name} must hold exactly one [[coverage]] entry")
    item = items[0]
    if not isinstance(item, dict):
        raise HistoryError(f"{name} coverage entry must be a table")
    unknown = set(item) - COVERAGE_FIELDS
    if unknown:
        raise HistoryError(
            f"{name} coverage entry has unknown keys {', '.join(sorted(unknown))}"
        )

    entry = CoverageEntry(
        origin=name,
        commits=_commit_prefixes(item.get("commits"), f"{name} commits"),
        points=_strings(item.get("points"), f"{name} points"),
        checks=_strings(item.get("checks"), f"{name} checks"),
        reason=str(item.get("reason", "")).strip(),
        ignore=bool(item.get("ignore", False)),
    )
    # The file name carries the first mapped commit, so two branches can never
    # choose the same path for two different entries. That binding is what
    # makes the directory conflict-free rather than merely conflict-prone.
    if entry.commits:
        lead = entry.commits[0]
        if path.stem != lead and not path.stem.startswith(f"{lead}-"):
            raise HistoryError(
                f"{name} must be named {lead}.toml or {lead}-<slug>.toml "
                f"after its first mapped commit"
            )
    return entry


def load_legacy_coverage(path: Path) -> tuple[CoverageEntry, ...]:
    """Load a pre-migration shared ledger for a one-time fidelity check."""

    try:
        with path.open("rb") as handle:
            raw = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise HistoryError(f"cannot read {path}: {exc}") from exc
    if raw.get("version") != 1:
        raise HistoryError(f"{path.name} regression coverage version must be 1")
    stray = set(raw) - {"version", "coverage"}
    if stray:
        raise HistoryError(f"{path.name} has unknown keys {', '.join(sorted(stray))}")

    entries: list[CoverageEntry] = []
    for index, item in enumerate(raw.get("coverage", [])):
        where = f"{path.name} coverage[{index}]"
        if not isinstance(item, dict):
            raise HistoryError(f"{where} must be a table")
        unknown = set(item) - COVERAGE_FIELDS
        if unknown:
            raise HistoryError(f"{where} has unknown keys {', '.join(sorted(unknown))}")
        entries.append(
            CoverageEntry(
                origin=where,
                commits=_commit_prefixes(item.get("commits"), f"{where} commits"),
                points=_strings(item.get("points"), f"{where} points"),
                checks=_strings(item.get("checks"), f"{where} checks"),
                reason=str(item.get("reason", "")).strip(),
                ignore=bool(item.get("ignore", False)),
            )
        )
    return tuple(entries)


def verify_migration_fidelity(legacy_path: Path, fragments_path: Path) -> None:
    """Refuse a migration that drops or changes any shared-ledger entry."""

    def meaning(entry: CoverageEntry) -> tuple[object, ...]:
        return (entry.commits, entry.points, entry.checks, entry.reason, entry.ignore)

    legacy = Counter(meaning(entry) for entry in load_legacy_coverage(legacy_path))
    if not fragments_path.is_dir():
        raise HistoryError(f"cannot read {fragments_path}: not a directory")
    fragment_entries = tuple(
        _load_coverage_fragment(fragment)
        for fragment in sorted(fragments_path.glob("*.toml"))
    )
    fragments = Counter(meaning(entry) for entry in fragment_entries)
    if legacy != fragments:
        missing = sum((legacy - fragments).values())
        extra = sum((fragments - legacy).values())
        raise HistoryError(
            "regression-ledger migration changed entry meaning: "
            f"{missing} missing and {extra} extra"
        )


def load_coverage(path: Path = DEFAULT_COVERAGE) -> tuple[CoverageEntry, ...]:
    """Load every `[[coverage]]` fragment from a regression-mapping directory.

    One entry per file means an author appends a new file instead of editing a
    shared tail, so concurrent corrective changes never collide.
    """

    legacy = path.with_suffix(".toml")
    if legacy.exists():
        raise HistoryError(
            f"{legacy.name} is no longer the regression ledger: move each "
            f"[[coverage]] entry into {path.name}/<commit>-<slug>.toml"
        )
    if not path.is_dir():
        raise HistoryError(f"cannot read {path}: not a directory")
    return tuple(
        _load_coverage_fragment(fragment)
        for fragment in sorted(path.glob("*.toml"))
    )


def load_client_fixes(path: Path) -> ClientFixLedger:
    if not path.exists():
        return ClientFixLedger(enforce_after=None, fixes=())
    try:
        with path.open("rb") as handle:
            raw = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise HistoryError(f"cannot read {path}: {exc}") from exc
    if raw.get("version") != 1:
        raise HistoryError("client fix ledger version must be 1")
    enforce_after = raw.get("enforce_after")
    if enforce_after is not None and (
        not isinstance(enforce_after, str)
        or not re.fullmatch(r"[0-9a-f]{7,40}", enforce_after)
    ):
        raise HistoryError("client fix enforce_after must be a Git SHA prefix")

    entries: list[ClientFixEntry] = []
    for index, item in enumerate(raw.get("fixes", [])):
        where = f"fixes[{index}]"
        if not isinstance(item, dict):
            raise HistoryError(f"{where} must be a table")
        required = {
            "id",
            "commits",
            "source",
            "source_anchor",
            "test",
            "test_anchor",
        }
        if set(item) != required:
            raise HistoryError(
                f"{where} must contain exactly {', '.join(sorted(required))}"
            )
        entries.append(
            ClientFixEntry(
                id=str(item["id"]),
                commits=_commit_prefixes(item["commits"], f"{where}.commits"),
                source=str(item["source"]),
                source_anchor=str(item["source_anchor"]),
                test=str(item["test"]),
                test_anchor=str(item["test_anchor"]),
            )
        )
    return ClientFixLedger(enforce_after=enforce_after, fixes=tuple(entries))


def load_merge_ledger(path: Path = DEFAULT_MERGE_LEDGER) -> MergeLedger:
    """Read the boundary and the permanent errata.

    A missing file is phase A, not an error: the freeze is a policy someone
    turns on by writing a boundary down, and a checkout without the file is a
    checkout from before that happened.
    """

    if not path.exists():
        return MergeLedger(enforce_after=None, errata=())
    try:
        with path.open("rb") as handle:
            raw = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise HistoryError(f"cannot read {path}: {exc}") from exc
    if raw.get("version") != 1:
        raise HistoryError(f"{path.name} version must be 1")
    stray = set(raw) - {"version", "enforce_after", "errata"}
    if stray:
        raise HistoryError(f"{path.name} has unknown keys {', '.join(sorted(stray))}")
    enforce_after = raw.get("enforce_after")
    if enforce_after is not None and (
        not isinstance(enforce_after, str)
        or not re.fullmatch(r"[0-9a-f]{7,40}", enforce_after)
    ):
        raise HistoryError(f"{path.name} enforce_after must be a Git SHA prefix")

    errata: list[MergeErratum] = []
    for index, item in enumerate(raw.get("errata", [])):
        where = f"{path.name} errata[{index}]"
        if not isinstance(item, dict):
            raise HistoryError(f"{where} must be a table")
        if set(item) != {"commit", "reason"}:
            raise HistoryError(f"{where} must contain exactly commit, reason")
        commit = str(item["commit"])
        if not re.fullmatch(r"[0-9a-f]{7,40}", commit):
            raise HistoryError(f"{where}.commit must be a Git SHA prefix")
        reason = str(item["reason"]).strip()
        if not reason:
            raise HistoryError(f"{where} has no reason")
        errata.append(MergeErratum(commit=commit, reason=reason))
    return MergeLedger(enforce_after=enforce_after, errata=tuple(errata))


def landing_commit_title(subject: str) -> tuple[str, str] | None:
    """The PR title and number a landing commit's subject carries, if any.

    Every shape `main` carries: Forgejo's merge commit
    (`Merge pull request '<title>' (#N) from <branch> into main`), an
    integration branch's plan merge (`Merge plan/C-08 (#461) at <sha>`, whose
    "title" is the branch), and a squashed PR (`<title> (#N)`).
    """

    for shape in (MERGE_SUBJECT_RE, INTEGRATION_SUBJECT_RE, SQUASH_SUBJECT_RE):
        landed = shape.match(subject)
        if landed:
            return landed.group("title"), landed.group("pull")
    return None


def _git(root: Path, *args: str) -> str:
    env = os.environ.copy()
    # Hooks export repository-local paths for the outer checkout. History may
    # intentionally inspect a linked worktree or a fixture repository, where
    # inheriting those paths makes Git read the outer index or object store.
    for name in (
        "GIT_COMMON_DIR",
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_WORK_TREE",
    ):
        env.pop(name, None)
    try:
        return subprocess.run(
            ("git", *args),
            cwd=root,
            check=True,
            text=True,
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        raise HistoryError(f"cannot inspect git history: {exc}") from exc


def _is_test_path(path: str) -> bool:
    return (
        path.startswith("tests/")
        or "/Tests/" in path
        or "/src/test/" in path
        or "/src/androidTest/" in path
    )


def _audit_tips(root: Path) -> tuple[str, ...]:
    """The commits whose combined history this audit should cover.

    Normally just HEAD, which the pre-commit hook sees as the parent of the
    commit being made. While a merge is in progress the commit will have two
    parents and its tree already carries both sides' coverage files, so the
    audit has to see both -- otherwise every entry arriving through MERGE_HEAD
    looks unmatched and a correct merge cannot be committed at all.

    MERGE_HEAD can name more than one commit for an octopus merge, so every
    line is taken.
    """
    # Asked of git rather than assembled from `root / ".git"`, because in a
    # worktree `.git` is a file and the real path lives elsewhere.
    merge_head = Path(_git(root, "rev-parse", "--git-path", "MERGE_HEAD").strip())
    if not merge_head.is_absolute():
        merge_head = root / merge_head
    try:
        tips = merge_head.read_text(encoding="ascii").splitlines()
    except FileNotFoundError:
        return ("HEAD",)
    except (OSError, UnicodeError) as exc:
        raise HistoryError(f"cannot read pending merge heads: {exc}") from exc
    if not tips or any(
        not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", tip) for tip in tips
    ):
        raise HistoryError("pending MERGE_HEAD must contain full Git commit hashes")
    return ("HEAD", *tips)


def _history_heads(root: Path) -> tuple[str, ...]:
    """[`_audit_tips`] as resolved commit hashes, with the pending merge checked.

    The same tips, and deliberately a second function rather than a stricter
    version of the first. `_audit_tips` answers in the symbolic form a `git log`
    argument list wants and is what the audit's own contract tests pin. This
    answers in resolved hashes, because these heads are also handed to
    `rev-list ... --not <boundary>`, where a symbolic `HEAD` would silently mean
    whatever the boundary comparison happened to be run against.

    Resolving is also where a malformed pending merge stops being someone
    else's problem: `MERGE_HEAD` that does not hold full hashes is a broken
    worktree, and reading it as an ordinary single-parent audit would let
    unreachable regression mappings pass. That is an error, not a fallback.
    """

    tips = _audit_tips(root)
    pending = tips[1:]
    # A `MERGE_HEAD` that exists and names nothing is the same broken worktree
    # as one that names a non-hash, and `_audit_tips` cannot tell them apart:
    # it drops blank lines, so an empty file and no file both come back as
    # `("HEAD",)`. Asked here, where the difference decides between an error and
    # an ordinary single-parent audit.
    merge_head = Path(_git(root, "rev-parse", "--git-path", "MERGE_HEAD").strip())
    if not merge_head.is_absolute():
        merge_head = root / merge_head
    if merge_head.is_file() and not pending:
        raise HistoryError("pending MERGE_HEAD must contain full Git commit hashes")
    if any(
        not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", head) for head in pending
    ):
        raise HistoryError("pending MERGE_HEAD must contain full Git commit hashes")
    heads = [
        _git(root, "rev-parse", "--verify", f"{head}^{{commit}}").strip()
        for head in tips
    ]
    return tuple(dict.fromkeys(heads))


def is_corrective(subject: str, sha: str, post_boundary: frozenset[str]) -> bool:
    """Which corrective rule judges this commit — the legacy one, or §3.2's.

    The boundary decides, per commit, and that is the whole of the freeze.
    A commit at or before the boundary keeps the requirement it was written
    under: its evidence is a `regressions.d/` fragment, those fragments are
    the only evidence that range will ever have, and re-judging it under a
    rule it never saw would either discard that evidence or demand new
    evidence for a commit nobody can amend. A commit past the boundary is
    judged by §3.2's prefix rule and carries its evidence as a
    `Regression-Test:` line on the commit that lands it instead.
    """

    rule = CORRECTIVE_RE if sha in post_boundary else ISSUE_RE
    return bool(rule.search(subject))


# Reads one repository-relative file out of some tree, or None when that tree
# does not carry it as a file.
TreeReader = Callable[[str], "str | None"]


def git_tree_reader(root: Path, treeish: str) -> TreeReader:
    """Files as they are in one commit or tree, never as they are on disk."""

    def read(path: str) -> str | None:
        try:
            kind = _git(root, "cat-file", "-t", f"{treeish}:{path}").strip()
        except HistoryError:
            return None
        if kind != "blob":
            return None
        return _git(root, "cat-file", "blob", f"{treeish}:{path}")

    return read


def worktree_reader(root: Path) -> TreeReader:
    """Files as they are in the checkout, for a local run with no revision."""

    base = root.resolve()

    def read(path: str) -> str | None:
        target = (base / path).resolve()
        if base not in target.parents or not target.is_file():
            return None
        try:
            return target.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return None

    return read


def resolve_regression_test(value: str, read: TreeReader, where: str) -> str | None:
    """Why one `<path>::<name>` value does not resolve in a tree, or None.

    The single resolver both carriers use: the pre-merge check hands it the
    pull request's merge candidate, the merge audit hands it a landing
    commit's own tree. `where` names that tree in the message.
    """

    path, separator, name = value.partition("::")
    path, name = path.strip(), name.strip()
    if not separator or not path or not name:
        return f"{REGRESSION_TRAILER} {value!r} is not <path>::<name>"
    parts = path.split("/")
    if path.startswith("/") or "\\" in path or any(part in ("", ".", "..") for part in parts):
        return (
            f"{REGRESSION_TRAILER} names {path}, which is not a normalised "
            f"repository-relative path"
        )
    source = read(path)
    if source is None:
        return f"{REGRESSION_TRAILER} names {path}, which {where} does not carry"
    if not defines_test(source, name):
        return (
            f"{REGRESSION_TRAILER} names {name}, which {path} does not define "
            f"as a test in {where} (searched {path} for a Node title, a "
            f"declaration carrying or under #[test]/#[tokio::test]/@Test, or "
            f"a Swift `func test…()` or Python `def test…`)"
        )
    return None


@dataclasses.dataclass(frozen=True)
class LandingAudit:
    """What the landing commits past the boundary say, and what they cover.

    `covers` is every commit brought in by a landing commit whose
    `Regression-Test:` lines all resolve in its own tree (or which an erratum
    excuses). `introduced` is every commit brought in by any landing commit at
    all, and `landed` every commit some landing commit has as an ancestor. A
    corrective commit past the boundary in none of them is still on its way to
    `main` and is the pre-merge check's to judge; one that is `landed` but not
    `introduced` reached `main` without going through a pull request.
    """

    rows: tuple[MergeRegression, ...] = ()
    errors: tuple[str, ...] = ()
    covers: frozenset[str] = frozenset()
    introduced: dict[str, tuple[str, ...]] = dataclasses.field(default_factory=dict)
    landed: frozenset[str] = frozenset()


def _excuse(ledger: MergeLedger, sha: str) -> str | None:
    return next(
        (erratum.reason for erratum in ledger.errata if sha.startswith(erratum.commit)),
        None,
    )


def audit_merge_regressions(
    root: Path,
    history_heads: tuple[str, ...],
    ledger: MergeLedger,
    boundary: str | None,
    post_boundary: frozenset[str],
) -> LandingAudit:
    """Audit the `Regression-Test:` lines on landing commits past the boundary.

    Each line is resolved against **the landing commit's own tree**, not the
    tree as it is today: a landing commit on `main` is immutable and so is its
    tree, and that exact-tree binding is the reason the field is a path and a
    name rather than a short SHA. With no boundary there is nothing to audit,
    which is phase A.
    """

    if boundary is None:
        return LandingAudit()
    rows: list[MergeRegression] = []
    errors: list[str] = []
    covers: set[str] = set()
    introduced: dict[str, list[str]] = {}
    landings: list[str] = []

    for erratum in ledger.errata:
        if not any(sha.startswith(erratum.commit) for sha in post_boundary):
            errors.append(
                f"merge errata row {erratum.commit} names no commit past the "
                f"boundary {ledger.enforce_after}"
            )

    log = _git(
        root, "log", "--format=%H%x1f%P%x1f%s%x1e", *history_heads, "--not", boundary, "--"
    )
    for record in log.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        sha, parents, subject = record.split("\x1f", 2)
        landed = landing_commit_title(subject)
        if landed is None:
            continue
        title, pull = landed
        landings.append(sha)
        parent_list = parents.split()
        brought = (
            _git(root, "rev-list", f"{parent_list[0]}..{sha}", "--").split()
            if len(parent_list) > 1
            else [sha]
        )
        for commit in brought:
            introduced.setdefault(commit, []).append(sha)

        excuse = _excuse(ledger, sha)
        message = _git(root, "show", "-s", "--format=%B", sha)
        fields = parse_regression_fields(message)
        read = git_tree_reader(root, sha)
        problems = [
            problem
            for problem in (
                resolve_regression_test(value, read, "its own tree") for value in fields
            )
            if problem is not None
        ]
        corrective = bool(CORRECTIVE_RE.match(title))
        resolved = bool(fields) and not problems
        if excuse is None:
            for problem in problems:
                errors.append(f"landing commit {sha[:8]} (#{pull}): {problem}")
            if corrective and not fields:
                errors.append(
                    f"corrective landing commit {sha[:8]} (#{pull}) carries no "
                    f"{REGRESSION_TRAILER}: line: {title}"
                )
        if resolved or excuse is not None:
            covers.update(brought)
        rows.append(
            MergeRegression(
                landing=sha,
                pull=pull,
                title=title,
                corrective=corrective,
                tests=fields,
                resolved=resolved,
            )
        )

    landed_set = (
        frozenset(_git(root, "rev-list", *landings, "--not", boundary, "--").split())
        if landings
        else frozenset()
    )
    return LandingAudit(
        rows=tuple(rows),
        errors=tuple(errors),
        covers=frozenset(covers),
        introduced={commit: tuple(by) for commit, by in introduced.items()},
        landed=landed_set,
    )


def discover_issues(
    root: Path,
    catalog: Catalog,
    explicit_prefixes: tuple[str, ...] = (),
    history_heads: tuple[str, ...] | None = None,
    post_boundary: frozenset[str] = frozenset(),
) -> tuple[IssueCommit, ...]:
    issues: list[IssueCommit] = []
    heads = history_heads if history_heads is not None else _history_heads(root)
    history = _git(root, "log", "--no-merges", "--format=%H%x09%s", *heads, "--")
    for line in history.splitlines():
        sha, subject = line.split("\t", 1)
        if not is_corrective(subject, sha, post_boundary) and not any(
            sha.startswith(prefix) for prefix in explicit_prefixes
        ):
            continue
        paths = tuple(
            path
            for path in _git(
                root,
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                sha,
            ).splitlines()
            if path
        )
        points = tuple(
            point.id
            for point in catalog.points
            if any(matches(path, point.paths) for path in paths)
        )
        patch = _git(root, "show", "--format=", "--unified=0", "--no-ext-diff", sha)
        current_evidence_file = any(
            (root / path).is_file() for path in paths if _is_test_path(path)
        )
        inline_evidence = bool(TEST_ADDITION_RE.search(patch)) and any(
            (root / path).is_file() for path in paths
        )
        issues.append(
            IssueCommit(
                sha=sha,
                subject=subject,
                paths=paths,
                point_ids=points,
                direct_test_evidence=current_evidence_file or inline_evidence,
            )
        )
    return tuple(issues)


def audit_history(
    root: Path = REPO_ROOT,
    catalog: Catalog | None = None,
    coverage_path: Path = DEFAULT_COVERAGE,
    client_fixes_path: Path | None = None,
    merge_ledger_path: Path | None = None,
) -> HistoryReport:
    entries = load_coverage(coverage_path)
    client_fixes = load_client_fixes(
        client_fixes_path or root / "tests" / "client-fixes.toml"
    )
    merge_ledger = load_merge_ledger(
        merge_ledger_path or root / "validation" / "merge-errata.toml"
    )
    catalog = catalog or load_catalog()
    history_heads = _history_heads(root)
    errors: list[str] = []
    post_boundary: frozenset[str] = frozenset()
    boundary: str | None = None
    if merge_ledger.enforce_after:
        try:
            boundary = _git(
                root, "rev-parse", "--verify", f"{merge_ledger.enforce_after}^{{commit}}"
            ).strip()
            post_boundary = frozenset(
                _git(root, "rev-list", *history_heads, "--not", boundary, "--").split()
            )
        except HistoryError:
            boundary = None
            errors.append(
                "merge ledger enforce_after does not resolve: "
                f"{merge_ledger.enforce_after}"
            )
    issues = discover_issues(
        root,
        catalog,
        tuple(prefix for entry in entries for prefix in entry.commits)
        + tuple(prefix for entry in client_fixes.fixes for prefix in entry.commits),
        history_heads,
        post_boundary,
    )
    landing = audit_merge_regressions(
        root, history_heads, merge_ledger, boundary, post_boundary
    )
    errors.extend(landing.errors)

    by_sha = {issue.sha: issue for issue in issues}
    resolved: dict[str, CoverageEntry] = {}
    anchored: set[str] = set()

    for entry in entries:
        where = entry.origin
        # The freeze, phase B. A fragment is evidence for the pre-boundary
        # range and nothing else: neither a new fragment nor a commit added to
        # an old one may name a commit past the boundary. Such a commit
        # carries its evidence on the commit that lands it, where the tree it
        # names can never change under it.
        for prefix in entry.commits:
            if any(sha.startswith(prefix) for sha in post_boundary):
                errors.append(
                    f"{where} maps {prefix}, which is past the frozen "
                    f"boundary {merge_ledger.enforce_after}: name its test in "
                    f"the pull request as `{REGRESSION_TRAILER}: <path>::<name>` "
                    f"so its landing commit carries it, instead of adding to "
                    f"validation/regressions.d"
                )
        if not entry.commits:
            errors.append(f"{where} has no commits")
        if not entry.reason:
            errors.append(f"{where} has no reason")
        if entry.ignore and (entry.points or entry.checks):
            errors.append(f"{where} ignore entries cannot claim points or checks")
        if not entry.ignore and not entry.checks:
            errors.append(f"{where} has no checks")
        for point in entry.points:
            if point not in catalog.point_map:
                errors.append(f"{where} references unknown point {point}")
        for check in entry.checks:
            if check not in catalog.check_map:
                errors.append(f"{where} references unknown check {check}")

        for prefix in entry.commits:
            matches_sha = tuple(sha for sha in by_sha if sha.startswith(prefix))
            if len(matches_sha) != 1:
                errors.append(
                    f"{where} commit {prefix} matches {len(matches_sha)} audited commits"
                )
                continue
            sha = matches_sha[0]
            issue = by_sha[sha]
            if sha in resolved:
                errors.append(f"corrective commit {prefix} is covered more than once")
                continue
            resolved[sha] = entry
            if (
                not entry.ignore
                and issue.point_ids
                and not set(issue.point_ids).intersection(entry.points)
            ):
                errors.append(
                    f"{where} commit {prefix} maps to {', '.join(issue.point_ids)}; "
                    f"entry claims {', '.join(entry.points) or 'no point'}"
                )

        allowed_checks = set(catalog.always_checks)
        for point in entry.points:
            if point in catalog.point_map:
                allowed_checks.update(catalog.point_map[point].checks)
        for check in entry.checks:
            if check in catalog.check_map and check not in allowed_checks:
                errors.append(
                    f"{where} check {check} is not evidence for its listed point(s)"
                )

    anchor_ids: set[str] = set()
    anchor_prefixes: set[str] = set()
    for index, entry in enumerate(client_fixes.fixes):
        where = f"fixes[{index}]"
        if not entry.id or entry.id in anchor_ids:
            errors.append(f"{where} has a missing or duplicate id {entry.id!r}")
        anchor_ids.add(entry.id)
        for path, anchor, kind in (
            (entry.source, entry.source_anchor, "source"),
            (entry.test, entry.test_anchor, "test"),
        ):
            target = root / path
            if not target.is_file():
                errors.append(f"{where} missing {kind} path {path}")
            elif anchor not in target.read_text(encoding="utf-8"):
                errors.append(f"{where} lost {kind} anchor {anchor!r} in {path}")
        for prefix in entry.commits:
            if prefix in anchor_prefixes:
                errors.append(f"client fix commit {prefix} is anchored more than once")
                continue
            anchor_prefixes.add(prefix)
            matches_sha = tuple(sha for sha in by_sha if sha.startswith(prefix))
            if len(matches_sha) != 1:
                errors.append(
                    f"{where} commit {prefix} matches {len(matches_sha)} corrective commits"
                )
                continue
            sha = matches_sha[0]
            if sha in resolved:
                errors.append(f"corrective commit {prefix} has both ledger and anchor evidence")
                continue
            if sha in anchored:
                errors.append(f"corrective commit {prefix} is anchored more than once")
                continue
            anchored.add(sha)

    explicit_commits: set[str] = set()
    if client_fixes.enforce_after:
        try:
            boundary = _git(
                root, "rev-parse", "--verify", f"{client_fixes.enforce_after}^{{commit}}"
            ).strip()
            explicit_commits.update(
                _git(root, "rev-list", *history_heads, "--not", boundary, "--").splitlines()
            )
        except HistoryError:
            errors.append(
                f"client fix enforce_after does not resolve: {client_fixes.enforce_after}"
            )

    pending: set[str] = set()
    trailer_covered: set[str] = set()
    for issue in issues:
        client_anchor_required = issue.sha in explicit_commits and any(
            path.startswith("clients/") for path in issue.paths
        )
        if issue.sha in post_boundary:
            # Phase B. The client anchor rule is not part of the ledger this
            # plan freezes, so it still applies; everything else a pre-boundary
            # commit could offer (a fragment, a direct test change) is replaced
            # by the `Regression-Test:` lines on the commit that lands it.
            if client_anchor_required and issue.sha not in anchored:
                errors.append(
                    f"corrective client commit {issue.sha[:8]} needs a "
                    f"tests/client-fixes.toml anchor row: {issue.subject}"
                )
            if issue.sha in landing.covers:
                trailer_covered.add(issue.sha)
            elif issue.sha in landing.introduced:
                by = ", ".join(sha[:8] for sha in landing.introduced[issue.sha])
                errors.append(
                    f"corrective commit {issue.sha[:8]} landed in {by} with no "
                    f"{REGRESSION_TRAILER}: line that resolves: {issue.subject}"
                )
            elif issue.sha in landing.landed and _excuse(merge_ledger, issue.sha) is None:
                errors.append(
                    f"corrective commit {issue.sha[:8]} reached main outside any "
                    f"landing commit, so nothing can carry its "
                    f"{REGRESSION_TRAILER}: line: {issue.subject}"
                )
            elif issue.sha not in landing.landed:
                # Not on `main` yet. Its pull request's description is where
                # the field lives until it lands, and the fast lane's
                # `validation.regression_field` step is what judges it.
                pending.add(issue.sha)
            continue
        if client_anchor_required:
            if issue.sha not in anchored:
                errors.append(
                    f"corrective client commit {issue.sha[:8]} needs a "
                    f"tests/client-fixes.toml anchor row: {issue.subject}"
                )
            continue
        runtime_mapping_required = issue.sha in explicit_commits and any(
            path.startswith("crates/") for path in issue.paths
        )
        if runtime_mapping_required:
            if issue.sha not in resolved and issue.sha not in anchored:
                errors.append(
                    f"corrective runtime commit {issue.sha[:8]} needs an explicit "
                    f"regressions.d mapping or client-fix anchor: {issue.subject}"
                )
            continue
        if issue.point_ids and issue.direct_test_evidence:
            continue
        if issue.sha not in resolved:
            missing = "functionality point" if not issue.point_ids else "regression evidence"
            errors.append(
                f"corrective commit {issue.sha[:8]} has no {missing}: {issue.subject}"
            )

    return HistoryReport(
        issues=issues,
        covered_by_entry=tuple(
            sorted(sha for sha, entry in resolved.items() if not entry.ignore)
        ),
        covered_by_anchor=tuple(sorted(anchored)),
        ignored=tuple(sorted(sha for sha, entry in resolved.items() if entry.ignore)),
        errors=tuple(errors),
        merges=landing.rows,
        pending=tuple(sorted(pending)),
        covered_by_trailer=tuple(sorted(trailer_covered)),
    )


def _write_report(path: Path, report: HistoryReport) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    mapped = set(report.covered_by_entry)
    anchored = set(report.covered_by_anchor)
    ignored = set(report.ignored)
    trailer_covered = set(report.covered_by_trailer)
    pending = set(report.pending)
    payload = {
        "summary": {
            "corrective_commits": len(report.issues),
            "direct_test_evidence": report.direct_count,
            "explicit_check_mapping": report.mapped_count,
            "client_fix_anchor": report.anchored_count,
            "non_runtime_correction": report.ignored_count,
            "landing_commits_past_boundary": len(report.merges),
            "landing_trailer": len(report.covered_by_trailer),
            "awaiting_landing": len(report.pending),
            "errors": len(report.errors),
        },
        # Generated, never tracked. The whole point of the merge trailer is
        # that landing a PR adds no file to the repository.
        "merges": [
            {
                "landing": merge.landing,
                "pull": merge.pull,
                "title": merge.title,
                "corrective": merge.corrective,
                "tests": list(merge.tests),
                "resolved": merge.resolved,
            }
            for merge in report.merges
        ],
        "commits": [
            {
                "commit": issue.sha,
                "subject": issue.subject,
                "points": list(issue.point_ids),
                "coverage": (
                    "landing-trailer"
                    if issue.sha in trailer_covered
                    else "awaiting-landing"
                    if issue.sha in pending
                    else "client-fix-anchor"
                    if issue.sha in anchored
                    else "direct-test"
                    if issue.direct_test_evidence
                    else "non-runtime"
                    if issue.sha in ignored
                    else "explicit-check"
                ),
                "explicitly_mapped": issue.sha in mapped,
                "explicitly_anchored": issue.sha in anchored,
            }
            for issue in report.issues
        ],
        "errors": list(report.errors),
    }
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Verify every historical corrective commit has validation evidence."
    )
    parser.add_argument("--report", type=Path, help="write the machine-readable audit JSON")
    parser.add_argument(
        "--compare-legacy",
        type=Path,
        help="prove the fragment ledger is semantically identical to a legacy ledger",
    )
    args = parser.parse_args(argv)
    try:
        if args.compare_legacy:
            verify_migration_fidelity(args.compare_legacy, DEFAULT_COVERAGE)
            print("regression-ledger migration preserves every legacy entry")
            return 0
        report = audit_history()
    except HistoryError as exc:
        print(f"history audit error: {exc}", file=sys.stderr)
        return 2
    if args.report:
        _write_report(args.report, report)
    if report.errors:
        print("historical regression coverage is incomplete:", file=sys.stderr)
        for error in report.errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print(
        "history ok: "
        f"{len(report.issues)} corrective commits · "
        f"{report.direct_count} direct test changes · "
        f"{report.mapped_count} explicit current-check mappings · "
        f"{report.anchored_count} client-fix anchors · "
        f"{report.ignored_count} non-runtime corrections · "
        f"{len(report.merges)} landing commits past the boundary · "
        f"{len(report.covered_by_trailer)} covered by a landing trailer · "
        f"{len(report.pending)} awaiting their landing commit"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
