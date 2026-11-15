"""The pull request's description and title, as the CI event delivered them.

Workflows used to pass the description to scripts as `PR_BODY:
${{ github.event.pull_request.body }}`. A process's environment strings are
capped (Linux MAX_ARG_STRLEN, 128 KiB), and a long description made the
runner fail to start the step at all — `fork/exec /usr/bin/bash: argument list
too long` — three paperwork failures in 2026-11-02..12 with no rule broken.
The same text is in the event payload file the runner writes for every job,
so read it from there. An explicit `PR_BODY` / `PR_TITLE` still wins, for
local use and tests.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

MAX_EVENT_BYTES = 16 * 1024 * 1024


def _event_pull_request() -> dict:
    path = os.environ.get("GITHUB_EVENT_PATH")
    if not path:
        return {}
    try:
        with Path(path).open("rb") as handle:
            data = handle.read(MAX_EVENT_BYTES + 1)
    except OSError:
        return {}
    if len(data) > MAX_EVENT_BYTES:
        raise ValueError("GITHUB_EVENT_PATH exceeds 16 MiB")
    event = json.loads(data)
    pull_request = event.get("pull_request") if isinstance(event, dict) else None
    return pull_request if isinstance(pull_request, dict) else {}


def _field(env: str, key: str) -> str | None:
    if env in os.environ:
        return os.environ[env]
    value = _event_pull_request().get(key)
    return value if isinstance(value, str) else None


def body() -> str | None:
    """`PR_BODY`, else the event's pull_request.body; None outside a PR."""
    return _field("PR_BODY", "body")


def title() -> str | None:
    """`PR_TITLE`, else the event's pull_request.title; None outside a PR."""
    return _field("PR_TITLE", "title")
