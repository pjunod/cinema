"""Require real, unignored writer-cancellation bodies after Cargo exits zero."""
import json
from pathlib import Path
import re
import sys

from validation.rust_repair_evidence import one_suite, require

PREFIX = 'store::state_machine::sqlite::writer::snapshot_cut_tests::'
CASES = [PREFIX + 'dropped_transaction_' + outcome + '_reply_preserves_' + state + '_and_writer'
         for outcome, state in [('success', 'commit'), ('error', 'rollback')]]


def assess(listing, native):
    names = []
    summaries = []
    for line in listing.splitlines():
        if line.endswith(': test'):
            names.append(line.removesuffix(': test'))
        elif line.strip():
            summary = re.fullmatch(r'(\d+) tests?, 0 benchmarks?', line.strip())
            require(summary, 'Unknown writer inventory row')
            summaries.append(int(summary[1]))
    require(names and len(names) == len(set(names)), 'Empty/duplicate writer inventory')
    require(summaries == [len(names)], 'Writer inventory summary differs from its identities')
    require(set(CASES) <= set(names), 'Writer cancellation regression missing from inventory')
    result = one_suite(native.splitlines(), [{'name': name, 'ignored': False} for name in CASES],
                       filtered=len(names) - len(CASES))
    require(result['status'] == 'passed', 'A native writer cancellation regression failed')
    return result


if __name__ == '__main__':
    report = assess(Path(sys.argv[1]).read_text(), Path(sys.argv[2]).read_text())
    Path(sys.argv[3]).write_text(json.dumps(report, indent=2) + '\n')
