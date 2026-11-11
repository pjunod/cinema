"""The runner roster and the label sets workflows request, parsed once.

`tests/operations/test_contracts.py` checks that every self-hosted `runs-on`
is satisfiable by `validation/runner-fleet.toml`. ci-fleet (its own repository,
noirr/ci-fleet) compares the same two things against the live fleet with a copy
of this module, `cifleet/runner_fleet.py`: when one changes, change the other,
so the contract test and the console cannot disagree about what a pool is.

Standard library only (Python 3.11+ for `tomllib`).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import re
import tomllib

# Existing GitHub-style rows inherit platform labels by default. Dedicated
# Forgejo rows set implicit_labels=false to preserve their exact API labels.
IMPLICIT = "self-hosted"
# These repository-scoped Forgejo workers advertise exact role labels, without
# GitHub's implicit generic labels. Only these reviewed local selector shapes
# are accepted; an unknown or mixed alias cannot disappear from fleet checks.
FORGEJO_ROLE_LABEL_SETS = frozenset({
    ("ci-nonvoter-heavy-amd64",),
    ("ci-nonvoter-store",),
    ("incus", "ci-nonvoter-heavy-amd64"),
})
FORGEJO_ROLE_LABELS = frozenset({"ci-nonvoter-heavy-amd64", "ci-nonvoter-store"})


def load_roster(source: str | Path) -> dict:
    """Parse runner-fleet.toml from a path or from its text."""
    if isinstance(source, Path):
        with source.open("rb") as handle:
            return tomllib.load(handle)
    return tomllib.loads(source)


def rostered_runners(roster: dict) -> tuple[tuple[str, frozenset[str]], ...]:
    """Each rostered runner with its provider's actual label projection."""
    result = []
    for runner in roster["runners"]:
        implicit = runner.get("implicit_labels", True)
        if type(implicit) is not bool:
            raise ValueError("implicit_labels must be a boolean")
        raw_labels = runner["labels"]
        if (not isinstance(raw_labels, list)
                or not all(isinstance(label, str) and label for label in raw_labels)
                or len(raw_labels) != len(set(raw_labels))):
            raise ValueError("runner labels must be unique nonempty strings")
        labels = frozenset(raw_labels)
        if any(label.startswith("ci-nonvoter-") and label not in FORGEJO_ROLE_LABELS
               for label in labels):
            raise ValueError("unknown dedicated Forgejo runner role")
        if implicit:
            labels |= {IMPLICIT, runner["os"], runner["arch"]}
        result.append((runner["name"], labels))
    return tuple(result)


def self_hosted_label_sets(block: str) -> list[tuple[str, ...]]:
    """Every self-hosted label set a job's `runs-on` can resolve to.

    A job either names the labels as a YAML flow sequence or reads the same
    sequence from a matrix key. Parse the arrays that are actually there.
    """
    direct = [
        tuple(part.strip().strip("\"'") for part in literal.split(","))
        for literal in re.findall(r"(?m)^ +runs-on: \[([^]]+)\]$", block)
    ]
    matrix = [
        tuple(json.loads(literal))
        for literal in re.findall(r"(?m)^ +runs_on: '(\[[^']*\])'$", block)
    ]
    result = []
    for labels in (*direct, *matrix):
        if not all(isinstance(label, str) and label for label in labels):
            raise ValueError("runner selector labels must be nonempty strings")
        dedicated = any(label.startswith("ci-nonvoter-") for label in labels)
        if dedicated:
            if labels not in FORGEJO_ROLE_LABEL_SETS:
                raise ValueError("unknown or ambiguous dedicated Forgejo selector")
            result.append(labels)
        elif IMPLICIT in labels:
            result.append(labels)
    return result


@dataclass(frozen=True)
class WorkflowJob:
    workflow: str
    key: str
    name: str
    label_sets: tuple[tuple[str, ...], ...]
    needs: tuple[str, ...]
    conditional: bool
    condition: str = ""
    uses: str = ""  # a reusable workflow this job calls, by file name

    @property
    def runs_after_failure(self) -> bool:
        """An `if:` that still runs when a need failed (always(), failure(), !cancelled())."""
        return bool(re.search(r"\b(always|failure)\(\)|!\s*cancelled\(\)", self.condition))

    def name_pattern(self) -> re.Pattern:
        """The display names this job's runs can carry (matrix expressions expand)."""
        parts = re.split(r"\$\{\{.*?\}\}", self.name)
        pattern = ".+".join(re.escape(part) for part in parts)
        if self.name == self.key:  # an unnamed matrix job is shown as "key (values)"
            pattern += r"(?: \(.+\))?"
        return re.compile(pattern + r"\Z")


@dataclass
class Workflow:
    file: str
    jobs: list[WorkflowJob] = field(default_factory=list)
    schedules: list[str] = field(default_factory=list)


def parse_workflow(file: str, text: str) -> Workflow:
    """Jobs (key, display name, label sets, needs, `if:`) and cron schedules.

    The repository writes workflows in one shape: two-space job keys under a
    top-level `jobs:`, four-space job attributes. That shape is what the
    contract tests already rely on, so this reads it with the same regexes
    rather than adding a YAML dependency to the collector host.
    """
    workflow = Workflow(file=file)
    workflow.schedules = re.findall(r"(?m)^\s*- cron: [\"']([^\"']+)[\"']", text)
    body = re.split(r"(?m)^jobs:\n", text, maxsplit=1)
    if len(body) != 2:
        return workflow
    starts = list(re.finditer(r"(?m)^  ([A-Za-z0-9_-]+):\s*$", body[1]))
    for index, start in enumerate(starts):
        end = starts[index + 1].start() if index + 1 < len(starts) else len(body[1])
        block = body[1][start.start():end]
        key = start.group(1)
        name = re.search(r"(?m)^    name: (.+)$", block)
        condition = re.search(r"(?m)^    if: ?(.*)\n((?:      .*\n)*)", block + "\n")
        uses = re.search(r"(?m)^    uses: \./\.github/workflows/([^\s@]+)", block)
        workflow.jobs.append(
            WorkflowJob(
                workflow=file,
                key=key,
                name=name.group(1).strip().strip("\"'") if name else key,
                label_sets=tuple(self_hosted_label_sets(block)),
                needs=_needs(block),
                conditional=condition is not None,
                condition=" ".join((condition.group(1) + " " + condition.group(2)).split()) if condition else "",
                uses=uses.group(1) if uses else "",
            )
        )
    return workflow


def _needs(block: str) -> tuple[str, ...]:
    inline = re.search(r"(?m)^    needs: (.+)$", block)
    if inline:
        value = inline.group(1).strip()
        if value.startswith("["):
            return tuple(part.strip() for part in value.strip("[]").split(",") if part.strip())
        return (value,)
    listed = re.search(r"(?m)^    needs:\n((?:      - .+\n)+)", block)
    if listed:
        return tuple(line.strip()[2:].strip() for line in listed.group(1).splitlines())
    return ()


def parse_workflows(files: dict[str, str]) -> list[Workflow]:
    return [parse_workflow(name, text) for name, text in sorted(files.items())]


def requested_pools(workflows: list[Workflow]) -> dict[frozenset[str], list[WorkflowJob]]:
    """Each distinct self-hosted label set, with the jobs that request it."""
    pools: dict[frozenset[str], list[WorkflowJob]] = {}
    for workflow in workflows:
        for job in workflow.jobs:
            for labels in job.label_sets:
                pools.setdefault(frozenset(labels), []).append(job)
    return pools


def _cron_field(text: str, low: int, high: int) -> set[int]:
    values: set[int] = set()
    for part in text.split(","):
        step = 1
        if "/" in part:
            part, step_text = part.split("/", 1)
            step = int(step_text)
        if part == "*":
            first, last = low, high
        elif "-" in part:
            first, last = (int(value) for value in part.split("-", 1))
        else:
            first = last = int(part)
        values.update(range(first, last + 1, step))
    return values


def next_cron(expression: str, after: datetime) -> datetime | None:
    """The next UTC minute a five-field cron fires strictly after `after`."""
    fields = expression.split()
    if len(fields) != 5:
        return None
    minutes = _cron_field(fields[0], 0, 59)
    hours = _cron_field(fields[1], 0, 23)
    days = _cron_field(fields[2], 1, 31)
    months = _cron_field(fields[3], 1, 12)
    weekdays = {value % 7 for value in _cron_field(fields[4], 0, 7)}
    day_any, weekday_any = fields[2] == "*", fields[4] == "*"
    moment = after.astimezone(timezone.utc).replace(second=0, microsecond=0) + timedelta(minutes=1)
    for _ in range(366 * 24 * 60):
        cron_weekday = (moment.weekday() + 1) % 7
        if day_any or weekday_any:
            day_ok = moment.day in days and cron_weekday in weekdays
        else:
            day_ok = moment.day in days or cron_weekday in weekdays
        if moment.month in months and day_ok and moment.hour in hours and moment.minute in minutes:
            return moment
        moment += timedelta(minutes=1)
    return None


def eligible(runner_labels: frozenset[str] | set[str], requested: frozenset[str] | set[str]) -> bool:
    """The forge's rule: a runner may take a job when it carries every label."""
    return set(requested) <= set(runner_labels)
