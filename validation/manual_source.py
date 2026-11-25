"""`Manual-Source-Gate:` — reuse an actual completed CI gate for this exact head.

A pull request description may carry exactly one `Manual-Source-Gate: <run
id>` line naming a completed `ci / Main promotion gate` run for the PR's
current head (docs/ci/RUST-UNIT-FANOUT.md). When `source_gate_landing`
authenticates that run, the fast lane's scope switches off every job that
would only repeat it, and the requested lane with them: the authenticated run
is the receipt `scripts/land` reads. A claim that does not authenticate fails
the scope job; it never waives anything by itself.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import sys

CLAIM = 'Manual-Source-Gate:'
# Every scope output that starts a job the authenticated run already proved.
DUPLICATED = ('rust_units', 'rust', 'web_layout', 'apple', 'desktop_macos', 'android_jvm',
              'preflight', 'worker_control', 'lane_requested')


def authenticated_manual_scope(lines, event):
    """Explicit claims require real completed CI evidence, never a label waiver."""
    pull = event.get('pull_request') or {}
    claims = [line for line in (pull.get('body') or '').splitlines() if line.startswith(CLAIM)]
    if not claims:
        return list(lines)
    from validation import python_unit_receipts as receipts
    receipts.require(len(claims) == 1, 'Ambiguous manual source gate claim')
    match = re.fullmatch(r'Manual-Source-Gate: ([1-9][0-9]*)', claims[0])
    receipts.require(match is not None, 'Malformed explicit manual source-gate claim')
    proof = authenticate_manual_source(event, int(match[1]))
    output = dict(line.split('=', 1) for line in lines if '=' in line)
    for surface in DUPLICATED:
        output[surface] = 'false'
    print(f"Reused actual ci / Main promotion gate run {proof['run']} job {proof['job']}: {proof['url']}", file=sys.stderr)
    return [key + '=' + value for key, value in output.items()]


def authenticate_manual_source(event, run_id):
    from validation import python_unit_receipts as receipts
    from validation import promotion_unit_retention as retention
    from validation import source_gate_landing
    pull = event['pull_request']
    number, head = receipts.positive(event['number']), receipts.sha(os.environ['GITHUB_SHA'])
    receipts.require(pull['number'] == number and pull['head']['sha'] == head
                     and event['repository']['full_name'] == os.environ['GITHUB_REPOSITORY']
                     and pull['head']['repo']['full_name'] == os.environ['GITHUB_REPOSITORY']
                     and pull['base']['ref'] == 'main', 'Manual source-gate event identity differs')
    api = retention.runner_api(os.environ)
    main_tip = api.get('/branches/main')['commit']['id']
    return source_gate_landing.verify(api, number, head, main_tip, Path.cwd(), run_id)
