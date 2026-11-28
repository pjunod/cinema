"""Whether this fast-lane run is the requested full lane (docs/ci/REQUESTED-LANE.md).

`python3 -m validation.lane_request scope` reads the scope lines that
`validation.ci_scope` prints (`key=value`) on stdin and writes them back for
`$GITHUB_OUTPUT`, adding:

* `preflight`, `worker_control` and `rust_units`: `true` (the workflow ANDs
  them with `lane_requested`; the Rust unit selector decides per crate);
* `lane_requested`: `true` only on a pull request label event of a ready PR
  whose labels include `lane`.

A push, open, reopen or ready event runs the cheap gate only. The heavy jobs
and the promotion gate run once per head, when a session (ci-fleet) adds the
`lane` label. Forgejo reports a PR label change with the raw payload action
`label_updated` (or `label_cleared` when the last label goes), matched by the
workflow's `types: [labeled]`, and its payload carries no `label` field, only
`pull_request.labels`; `labeled` is accepted too so a GitHub-shaped payload
means the same thing. A label event whose labels lack `lane` (an unrelated
label, or `lane` being removed) requests nothing.

A `Manual-Source-Gate:` claim is applied last (validation/manual_source.py):
an authenticated claim switches every duplicated job off, the request too.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys

LABEL = 'lane'
LABEL_ACTIONS = frozenset({'labeled', 'label_updated'})
ALWAYS_ON = ('preflight', 'worker_control', 'rust_units')


def label_names(pull: dict) -> list[str]:
    labels = pull.get('labels') or []
    return [label.get('name') for label in labels if isinstance(label, dict)]


def requested(event: dict) -> bool:
    """A label event on a ready PR that now carries `lane`."""
    if not isinstance(event, dict):
        return False
    pull = event.get('pull_request')
    if not isinstance(pull, dict):
        return False
    return (event.get('action') in LABEL_ACTIONS and pull.get('draft') is False
            and LABEL in label_names(pull))


def scope(lines: list[str], event: dict) -> list[str]:
    values = dict(line.split('=', 1) for line in lines if '=' in line)
    for output in ALWAYS_ON:
        # Rust tests can read files compile scope ignores; the unit selector
        # must see every requested run and no-ops on unchanged content keys.
        values[output] = 'true'
    values['lane_requested'] = 'true' if requested(event) else 'false'
    return [f'{key}={value}' for key, value in values.items()]


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv != ['scope']:
        raise SystemExit('usage: python3 -m validation.lane_request scope < ci_scope-output')
    path = os.environ.get('GITHUB_EVENT_PATH')
    event = json.loads(Path(path).read_text()) if path and Path(path).is_file() else {}
    from validation import manual_source
    lines = manual_source.authenticated_manual_scope(scope(sys.stdin.read().splitlines(), event), event)
    values = dict(line.split('=', 1) for line in lines)
    if values['lane_requested'] == 'true':
        print('requested lane: the `lane` label asks for the full lane on this head', file=sys.stderr)
    else:
        print('cheap gate only: no `lane` label request on this event', file=sys.stderr)
    print('\n'.join(lines))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
