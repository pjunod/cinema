"""Whole native unittest discovery from an authenticated promotion preflight."""

import re

from validation import promotion_unit_retention as retention
from validation.python_unit_receipts import require


# Reviewed environment and command bodies. Neither a run ID nor its log
# digest authorizes execution: the caller also authenticates both through CI.
PREFLIGHT = 'e1c1d63930937e64b70da4e9de434c00d6416cad0182f95d6ba58570f7094cd2'
CURRENT_PREFLIGHT = '8c31cb8cc8bc56c8625e5e1eff387801008521d1bbb696618efc421592840df5'
HISTORICAL_SCOPE = '0ee1ac7960c2417e5eba56611b80ece17b81c30d9913ccf9aa38386b94c98a75'
MAKEFILE = 'd74b5626264702988ac3d7bc9f64aac246a1656e8408d539c0241659375971d8'


def job_digest(workflow, name):
    jobs = re.findall(r'(?ms)^  ' + name + r':\n.*?(?=^  [A-Za-z0-9_-]+:\n|\Z)', workflow)
    require(len(jobs) == 1, 'Ambiguous Python preflight source')
    return retention.digest(jobs[0].encode())


def environment(source, workflow, expected=PREFLIGHT):
    globals_ = workflow.partition('\njobs:\n')[0]
    require(not re.search(r'(?m)^defaults:', globals_)
            and re.findall(r'(?ms)^env:\n(.*?)(?=^[^ \n]|\Z)', globals_)
            == ['  CARGO_TERM_COLOR: always\n\n']
            and job_digest(workflow, 'preflight') == expected
            and retention.digest(retention.git_bytes(source, 'Makefile') or b'') == MAKEFILE,
            'Python command, executor or workflow environment differs from reviewed preflight')


def native_event(source, workflow, inputs):
    environment(source, workflow)
    require(job_digest(workflow, 'scope') == HISTORICAL_SCOPE
            and all(not inputs.get(key) for key in (
                'retained_units_sha256', 'retained_units_attachment_id', 'retained_units_attachment_uuid')),
            'Historical Python discovery may have adopted retained results')


def outer_counts(text, kind, run_id, count):
    require(kind in {'validation', 'operations'} and count > 0, 'Unknown Python discovery')
    require(len(re.findall(r'^🏁  Job succeeded$', text, re.M)) == 1
            and not re.search(r'^Adopted authenticated complete (?:validation|operations) unit discovery$', text, re.M),
            'Native Python preflight success unavailable or adopted')
    if kind == 'validation':
        start = r'^catalog ok: [^\n]+$'
        end = r'^##\[group\]Environment details$'
    else:
        start = rf'^Artifact formal-model-{run_id}-1 has been successfully uploaded! [^\n]+$'
        end = r'^ok - every surface routes every state and every input to a defined outcome$'
    starts, ends = list(re.finditer(start, text, re.M)), list(re.finditer(end, text, re.M))
    require(len(starts) == len(ends) == 1 and starts[0].end() < ends[0].start(),
            'Native Python preflight step boundaries missing or ambiguous')
    block = text[starts[0].end():ends[0].start()]
    summaries = list(re.finditer(r'^-{70}\nRan (\d+) tests? in [0-9.]+s\n\n([^\n]+)$', block, re.M))
    require(summaries and int(summaries[-1][1]) == count
            and sum(int(row[1]) == count for row in summaries) == 1,
            'Native Python outer discovery count differs from source inventory')
    terminal = re.fullmatch(r'OK(?: \(skipped=([1-9]\d*)\))?', summaries[-1][2])
    require(terminal is not None, 'Native Python discovery did not complete successfully')
    skipped = int(terminal[1] or 0)
    require(skipped <= count, 'Impossible native Python skip count')
    return {'native_discovered': count, 'native_skipped': skipped, 'native_skipped_ids': None}


def source_cohort(record, text, kind, command, discovery, candidate):
    from validation.qualification_unit_continuation import authenticated_ids

    origin = record['origin']
    require(origin['execution'] == {'mode': 'source-cohort-v1', 'kind': kind,
                                    'native_discovery': True, 'job_status': 'success'}
            and origin['kind'] == 'authenticated-ci' and origin['command'] == command
            and type(record['exit_code']) is int and record['exit_code'] == 0,
            'Wrong native Python command or disposition')
    run_id, _ = authenticated_ids(origin['identity'])
    workflow = retention.git_bytes(candidate, '.github/workflows/ci.yml')
    require(workflow is not None, 'Candidate Python preflight unavailable')
    environment(candidate, workflow.decode(), CURRENT_PREFLIGHT)
    ids = list(discovery(record['source_commit'], kind))
    require(record['coverage'] == {'mode': 'python-discovery', 'ids': ids},
            'Native Python cohort differs from frozen source discovery')
    return ids, outer_counts(text, kind, run_id, len(ids))
