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


# These source-level controls parse supplied fixtures and do not invoke either
# platform's native binaries. Extending this set requires reviewing the inputs
# a body consumes before applying a Darwin result to the Linux preflight.
MEASURED_PORTABLE_CASES = {
    'test_native_wal_openraft.NativeWalOpenraftCase.test_native_source_recipe_keeps_actual_unprinted_outcome_scope',
    'test_native_wal_openraft.NativeWalOpenraftCase.test_rejects_unbound_origin_neighbor_target_or_partial_terminal',
    'test_native_wal_openraft.NativeWalOpenraftCase.test_preceding_command_cannot_absorb_the_unprinted_openraft_result',
    'test_native_wal_openraft.NativeWalOpenraftCase.test_echoed_next_command_keeps_the_existing_boundary_path',
    'test_retained_lane_commands.NativePythonTransitionCase.test_only_reviewed_whole_preflight_transition_preserves_node_scope',
    'test_native_python_discovery.NativeDiscoveryCase.test_native_skips_remain_unnamed_and_nested_fixtures_do_not_supply_outcomes',
    'test_native_python_discovery.NativeDiscoveryCase.test_refuses_misplaced_adopted_failed_or_ambiguous_discovery',
    'test_native_python_discovery.NativeDiscoveryCase.test_historical_retained_input_or_changed_scope_cannot_claim_execution',
    'test_native_python_discovery.NativeDiscoveryCase.test_native_continuation_runs_only_selected_exact_cases',
}


def measured_named(proof, record, raw, kind, universe, archive_digest):
    """Read a clean, measured archive capture on the reviewed local toolchain."""
    from validation.nonrust_unit_cohort import blob, python_rows
    from validation import python_unit_receipts as receipts

    origin = record['origin']
    execution = origin['execution']
    require(kind in {'operations', 'validation'} and origin['kind'] == 'attested-local'
            and set(execution) == {'mode', 'kind', 'measured_python_binding'}
            and record['coverage'] == {'mode': 'python-named'}
            and type(record['exit_code']) is int and record['exit_code'] in (0, 1),
            'Unexpected measured Python evidence')
    binding = receipts.bounded_json(blob(proof, execution['measured_python_binding']))
    argv = binding['argv']
    require(binding['source_commit'] == record['source_commit']
            and binding['source_archive_sha256'] == archive_digest(record['source_commit'])
            and all(binding[key] is True for key in ('source_clean_before', 'source_clean_after', 'source_stable'))
            and binding['log_sha256'] == retention.digest(raw) and binding['exit_code'] == record['exit_code']
            and binding['platform'] == 'darwin' and binding['python_version'] == '3.12.14'
            and binding['python_binary_sha256'] == '9d1335796350848c36dc2711a3298e096deebee18c85f1ab36edd8747fdf85eb'
            and binding['node_version'] == 'v24.22.1'
            and binding['node_binary_sha256'] == '8a7dd4c5cb35e6171dfbea7303650bc311f46971b06dd7e1c62e3f291cfd746c'
            and isinstance(argv, list) and argv[:4] == [binding['python_executable'], '-m', 'unittest', '-v']
            and origin['command'] == argv and len(argv) > 4,
            'Python archive, command, measured source or runtime differs')
    prefix = 'tests.' + kind + '.'
    require(all(token.startswith(prefix) for token in argv[4:]), 'Measured Python selector outside suite')
    ids = [token.removeprefix(prefix) for token in argv[4:]]
    actual = python_rows(raw.decode(), kind, universe)
    passed = all(outcome == 'pass' for outcome in actual.values())
    terminal = r'^OK\s*\Z' if passed else r'^FAILED \((?:errors|failures)=[1-9]\d*(?:, (?:errors|failures)=[1-9]\d*)?\)\s*\Z'
    require(len(ids) == len(set(ids)) and set(actual) == set(ids) and set(ids) <= MEASURED_PORTABLE_CASES
            and passed == (record['exit_code'] == 0) and re.search(terminal, raw.decode(), re.M),
            'Measured Python selectors, terminal or disposition differ')
    return actual
