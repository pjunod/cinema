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


def _event_pull_request(*, required: bool = False) -> dict:
    path = os.environ.get("GITHUB_EVENT_PATH")
    if not path:
        if required:
            raise ValueError("PR_BODY_FROM_EVENT requires GITHUB_EVENT_PATH")
        return {}
    try:
        with Path(path).open("rb") as handle:
            data = handle.read(MAX_EVENT_BYTES + 1)
    except OSError:
        if required:
            raise
        return {}
    if len(data) > MAX_EVENT_BYTES:
        raise ValueError("GITHUB_EVENT_PATH exceeds 16 MiB")
    event = json.loads(data)
    pull_request = event.get("pull_request") if isinstance(event, dict) else None
    if isinstance(pull_request, dict):
        return pull_request
    if required:
        raise ValueError("PR event must contain pull_request")
    return {}


def _field(env: str, key: str, *, require_event: bool = False) -> str | None:
    if env in os.environ:
        return os.environ[env]
    request = _event_pull_request(required=require_event)
    value = request.get(key)
    if require_event and (key not in request or (value is not None and not isinstance(value, str))):
        raise ValueError(f"PR event must contain pull_request.{key} as a string or null")
    return value if isinstance(value, str) else None


def body(*, require_event: bool = False) -> str | None:
    """Read the full PR body, requiring a valid event when explicitly selected."""
    return _field("PR_BODY", "body", require_event=require_event)


def title() -> str | None:
    """`PR_TITLE`, else the event's pull_request.title; None outside a PR."""
    return _field("PR_TITLE", "title")
