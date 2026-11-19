"""Read original Cargo workspace outcomes without assigning cross-target names."""
from __future__ import annotations

from collections import Counter, deque
import re

from validation import python_unit_receipts as receipts

COMMAND = 'cargo test --workspace --locked --exclude plurx-cluster-check --no-fail-fast'
HEADER = re.compile(
    r'^\s{3,}(?:Running (?:unittests )?(\S+) \((?:[^\n]+/)?'
    r'([a-zA-Z0-9_]+)-[0-9a-f]{16}\)|Doc-tests (\w+))$')
SUMMARY = re.compile(
    r'test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; '
    r'0 measured; 0 filtered out; finished in [\d.]+s')
BODY = re.compile(r'test (.+?) \.\.\. (ok|FAILED|ignored)(?:, .*|)')
OUTCOMES = {'ok': 'pass', 'FAILED': 'fail', 'ignored': 'ignored'}
# These two process helpers run their own libtest binary. The final outer
# harness row, checked against its full summary, is the applicable outcome.
NESTED_TRAY = {
    'linux::tray::tests::dbus_menu_clicks_reach_the_shell_after_refresh': ('pass', 'pass'),
    'tests::process::xdg_runner_child': ('pass', 'ignored'),
}


def clean_lines(raw):
    text = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', raw.decode(), flags=re.M)
    return re.sub(r'\x1b\[[0-9;]*[A-Za-z]', '', text).splitlines()


def workspace_targets(raw):
    """Return only observed literal outcomes, preserving partial target failure."""
    lines = clean_lines(raw)
    receipts.require(lines.count(COMMAND) == 1, 'Original workspace invocation is ambiguous')
    results, pending = {}, deque()
    active = None
    nested_restore = report = False
    start = lines.index(COMMAND) + 1
    for lineno, line in enumerate(lines[start:],start):
        # The failed restore test embeds the subprocess output in its own
        # diagnostic. It is not a second top-level test execution.
        if line == '--- restore child stdout.log tail ---':
            nested_restore = True
            continue
        if line == '--- restore child stderr.log tail ---':
            nested_restore = False
            continue
        if nested_restore:
            continue
        header = HEADER.fullmatch(line)
        if header:
            src, binary, docs = header.groups()
            pending.append('doc:' + docs if docs else f'{binary}@{src}')
            continue
        if active is None:
            start = re.fullmatch(r'running (\d+) tests?', line)
            if start:
                receipts.require(pending, 'Workspace target header missing')
                active = pending.popleft()
                receipts.require(active not in results, 'Duplicate workspace target')
                declared = int(start[1])
                results[active] = {'declared': declared, 'outcomes': {}, 'complete': False}
                report = False
            continue
        outcomes = results[active]['outcomes']
        if re.fullmatch(r'---- \S+ (?:stdout|stderr) ----', line):
            report = True
        summary = SUMMARY.fullmatch(line)
        if summary:
            polarity, passed, failed, ignored = summary.groups()
            expected = {'pass': int(passed), 'fail': int(failed), 'ignored': int(ignored)}
            receipts.require(Counter(outcomes.values()) == Counter(expected)
                             and len(outcomes) == declared
                             and (polarity == 'ok') == (int(failed) == 0),
                             'Workspace target summary contradicts literal bodies')
            results[active]['complete'] = True
            active = None
        elif line == 'fatal runtime error: stack overflow, aborting':
            thread = re.fullmatch(r"thread '([A-Za-z_0-9:]+)' \(\d+\) has overflowed its stack",lines[lineno - 1])
            receipts.require(not report and len(outcomes) < declared and thread and thread[1] not in outcomes,
                             'Interrupted workspace target lacks an exact failing thread identity')
            results[active]['interrupted_test'] = thread[1]
            active = None
        elif not report and (body := BODY.fullmatch(line)):
            name = body[1].removesuffix(' - should panic')
            outcome = OUTCOMES[body[2]]
            if name in outcomes:
                receipts.require(active == 'plurx_tray@src/lib.rs' and
                                 NESTED_TRAY.get(name) == (outcomes[name], outcome),
                                 'Duplicate native workspace outcome')
            outcomes[name] = outcome
    receipts.require(results and not pending and active is None and not nested_restore,
                     'Workspace log terminated without target framing')
    return results
