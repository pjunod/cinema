"""The `Changelog:` field: where a pull request's changelog entry lives.

`CHANGELOG.md` used to be edited by every pull request at the same insertion
point under `## [Unreleased]`, so any two open pull requests conflicted on it
and re-conflicted every time `main` moved. The entry now lives in the pull
request description instead:

    Changelog: Fixed — **Shared titles start again.** A shared copy of a
      Matroska title with reordered frames now starts from the beginning.
    Changelog: none

One line per entry: a Keep a Changelog section (`Added`, `Changed`,
`Deprecated`, `Removed`, `Fixed`, `Security`), a separator (` — `, ` -- `,
` - ` or `: `), and the entry's Markdown. Lines indented by at least two
spaces (or a tab) continue the entry above them. `Changelog: none` says the
change has nothing a person running the server would notice; it may carry a
reason after a separator and cannot be combined with entries.

Three places read it, all through `parse`:

* **Before merge**, `scripts/paperwork` (the fast lane's first job) refuses a
  description without a valid field. That is the check that matters: the
  description is still editable then.
* **At merge**, the description becomes the landing commit's message
  (`.forgejo/default_merge_message/MERGE_TEMPLATE.md`, `scripts/land` and the
  merge train all copy it), so every first-parent commit of `main` past the
  cutover carries its own entries. `main` accepts merge commits only, so there
  is always exactly one landing commit per pull request.
* **At release**, `scripts/release-cut` collects the entries of every landing
  commit since the previous cut (`collect`) into the dated section. Branch
  commits are never read, so nothing is counted twice.

HTML comments and fenced code blocks are ignored, so the pull request
template can show the syntax without the example counting as an entry.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys

FIELD = "Changelog"
SECTIONS = ("Added", "Changed", "Deprecated", "Removed", "Fixed", "Security")
NONE = "none"

_FIELD_RE = re.compile(rf"^{FIELD}:[ \t]*(?P<value>.*?)[ \t]*$")
_SEPARATOR_RE = re.compile(r"^(?P<head>[A-Za-z]+)(?:\s+(?:—|--|-)\s+|:\s+|\s*$)(?P<text>.*)$", re.DOTALL)
_COMMENT_RE = re.compile(r"<!--.*?-->", re.DOTALL)
_FENCE_RE = re.compile(r"^[ \t]*(```|~~~)")
_CONTINUATION_RE = re.compile(r"^(?:  |\t)\s*\S")
# The landing commit subjects `main` carries (validation/history.py).
_MERGE_SUBJECT_RE = re.compile(r"^Merge pull request '.*' \(#(?P<pull>\d+)\) from \S+ into \S+$")

HINT = (
    "add a `Changelog: <Section> — <entry>` line per user-visible change to the pull request "
    "description (Section is one of " + ", ".join(SECTIONS) + "), or `Changelog: none` "
    "when nothing a person running the server would notice changed. Indent continuation "
    "lines by two spaces. Never edit CHANGELOG.md: scripts/release-cut writes it."
)


class ChangelogError(ValueError):
    """A description's Changelog field is missing or malformed."""


@dataclass(frozen=True)
class Entry:
    section: str
    text: str

    def markdown(self) -> str:
        """The entry as a CHANGELOG.md bullet."""

        first, *rest = self.text.split("\n")
        return "\n".join([f"- {first}", *(f"  {line}" if line else "" for line in rest)])


@dataclass(frozen=True)
class Field:
    """What one description or landing message says about the changelog."""

    entries: tuple[Entry, ...]
    none: bool
    reason: str = ""


def _visible_lines(body: str) -> list[str]:
    lines = []
    fenced = False
    for line in _COMMENT_RE.sub("", body.replace("\r\n", "\n")).split("\n"):
        if _FENCE_RE.match(line):
            fenced = not fenced
            continue
        if not fenced:
            lines.append(line)
    return lines


def parse(body: str) -> Field | None:
    """The field `body` carries, None when it carries none.

    Raises ChangelogError for a field that is present but malformed.
    """

    lines = _visible_lines(body)
    entries: list[Entry] = []
    none_reason: str | None = None
    found = False
    index = 0
    while index < len(lines):
        match = _FIELD_RE.match(lines[index])
        index += 1
        if match is None:
            continue
        found = True
        value = match.group("value")
        continued = []
        while index < len(lines) and _CONTINUATION_RE.match(lines[index]):
            continued.append(lines[index].strip())
            index += 1
        if not value and not continued:
            raise ChangelogError(
                f"the `{FIELD}:` line is empty (the pull request template's blank); fill it in"
            )
        split = _SEPARATOR_RE.match(value)
        head = split.group("head") if split else ""
        text = "\n".join([split.group("text").strip(), *continued] if split else continued).strip()
        if head.lower() == NONE:
            none_reason = text
            continue
        section = next((name for name in SECTIONS if name.lower() == head.lower()), None)
        if section is None:
            raise ChangelogError(
                f"`{FIELD}: {value[:60]}` does not start with a section; use one of "
                + ", ".join(SECTIONS)
                + f", or `{FIELD}: {NONE}`"
            )
        if not text:
            raise ChangelogError(f"`{FIELD}: {value[:60]}` has a section but no entry text")
        entries.append(Entry(section, text))
    if not found:
        return None
    if none_reason is not None and entries:
        raise ChangelogError(
            f"`{FIELD}: {NONE}` cannot be combined with {len(entries)} changelog "
            f"entr{'y' if len(entries) == 1 else 'ies'}; keep one or the other"
        )
    return Field(tuple(entries), none_reason is not None, none_reason or "")


def require(body: str) -> Field:
    """The field `body` carries; raises ChangelogError when it is missing."""

    field = parse(body)
    if field is None:
        raise ChangelogError(f"the description has no `{FIELD}:` field")
    return field


def render(entries: tuple[Entry, ...] | list[Entry]) -> dict[str, list[str]]:
    """Entries grouped by section, in Keep a Changelog order, as bullets."""

    grouped: dict[str, list[str]] = {}
    for section in SECTIONS:
        bullets = [entry.markdown() for entry in entries if entry.section == section]
        if bullets:
            grouped[section] = bullets
    return grouped


# -- release-time collection ---------------------------------------------------------


def _git(root: Path, *args: str) -> str:
    proc = subprocess.run(["git", *args], cwd=root, capture_output=True, text=True)
    if proc.returncode != 0:
        raise ChangelogError(f"git {' '.join(args[:2])} failed: {proc.stderr.strip()[:300]}")
    return proc.stdout


@dataclass(frozen=True)
class Landing:
    sha: str
    pull: int
    field: Field | None
    problem: str = ""


def landings(root: Path, since: str | None, until: str = "HEAD") -> list[Landing]:
    """Every pull request landing on `until`'s first-parent line after `since`, oldest first.

    `since` None reads the whole line: landings from before the field existed
    carry none and contribute nothing.
    """

    span = f"{since}..{until}" if since else until
    # NUL-separated records: a message can contain anything but NUL.
    out = _git(root, "log", "--first-parent", "--reverse", "--format=%H%n%B%x00", span, "--")
    found = []
    for record in out.split("\x00"):
        record = record.strip("\n")
        if not record:
            continue
        sha, _, message = record.partition("\n")
        subject = message.split("\n", 1)[0]
        merged = _MERGE_SUBJECT_RE.match(subject)
        if merged is None:
            continue
        try:
            field = parse(message)
            problem = ""
        except ChangelogError as error:
            field, problem = None, str(error)
        found.append(Landing(sha.strip(), int(merged.group("pull")), field, problem))
    return found


MARKER_RE = re.compile(r"(?m)^<!-- changelog-through: (?P<sha>[0-9a-f]{40}) -->$")


def marker(sha: str) -> str:
    """The line a dated section carries: the last landing commit its cut read."""

    return f"<!-- changelog-through: {sha} -->"


def cutover_base(root: Path, until: str = "HEAD") -> str | None:
    """The first-parent commit just before the field existed on `until`'s line.

    The first release cut after the cutover has no marker to start from; it
    starts at the parent of the landing that added this module, so the
    cutover's own landing (whose description carries its entries) is read and
    nothing older is. None when this module is not on `until`'s line at all.
    """

    out = _git(root, "log", "--first-parent", "--diff-merges=first-parent", "--no-patch", "--diff-filter=A",
               "--format=%H", until, "--", "validation/changelog_field.py").split()
    return f"{out[-1]}^1" if out else None


def since_for(root: Path, changelog: str, until: str = "HEAD") -> str | None:
    """Where the next cut starts: the newest dated section's marker, else the cutover."""

    found = MARKER_RE.search(changelog)
    return found.group("sha") if found else cutover_base(root, until)


def collect(root: Path, since: str | None, until: str = "HEAD") -> tuple[list[Entry], list[Landing]]:
    """(entries, landings whose field is missing or malformed) after `since`."""

    entries: list[Entry] = []
    lacking: list[Landing] = []
    for landing in landings(root, since, until):
        if landing.field is None:
            lacking.append(landing)
        else:
            entries.extend(landing.field.entries)
    return entries, lacking


def after_cutover(root: Path, landing: Landing) -> bool:
    """Whether the field already existed when `landing` merged, so it should carry one."""

    return subprocess.run(
        ["git", "cat-file", "-e", f"{landing.sha}^1:validation/changelog_field.py"],
        cwd=root, capture_output=True,
    ).returncode == 0


def landing_notices(root: Path, before: str | None, after: str) -> list[str]:
    """One line per landing in `before..after` that came in after the cutover without a valid field.

    Advisory: `validation.landing check` prints these as warnings and does not
    fail on them. The remedy needs no errata row -- the entry goes in the
    next pull request's description, and the next cut collects it from there.
    """

    notices = []
    for landing in landings(root, before or None, after):
        if landing.field is not None:
            continue
        if not after_cutover(root, landing):
            continue
        notices.append(
            f"#{landing.pull} landed as {landing.sha[:9]} without a valid `{FIELD}:` field"
            + (f" ({landing.problem})" if landing.problem else "")
            + "; if it changed something users notice, put its entry in the next pull "
            "request's description"
        )
    return notices


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check or show a pull request's Changelog field.")
    parser.add_argument("--body-file", type=Path, help="the pull request description to check")
    parser.add_argument("--body-env", action="store_true",
                        help="read the description from the runner's pull request event")
    parser.add_argument("--render", action="store_true", help="print the entries as CHANGELOG.md Markdown")
    args = parser.parse_args(argv)
    if args.body_file:
        body = args.body_file.read_text(encoding="utf-8")
    elif args.body_env:
        from validation import pr_event  # noqa: PLC0415

        body = pr_event.body() or ""
    else:
        parser.error("pass --body-file or --body-env")
    try:
        field = require(body)
    except ChangelogError as error:
        print(f"Changelog field: {error}", file=sys.stderr)
        print(f"fix: {HINT}", file=sys.stderr)
        return 1
    if args.render:
        for section, bullets in render(field.entries).items():
            print(f"### {section}")
            print("\n".join(bullets))
    elif field.none:
        print(f"Changelog: none{f' ({field.reason})' if field.reason else ''}")
    else:
        print(f"Changelog: {len(field.entries)} entr{'y' if len(field.entries) == 1 else 'ies'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
