#!/usr/bin/env python3
"""Summarize timestamped Cargo/Forgejo logs without executing tests or accessing hosts.

Only a completed libtest result proves completion. Running banners and slow-test
warnings are observations, not proof of outcome or per-test execution duration.
"""
import argparse
from collections import Counter
from datetime import datetime
import json
from pathlib import Path
import re

ANSI = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')
STAMP = re.compile(r'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z)\s?(.*)$')
RUNNING = re.compile(r'^Running\s+(.+?)\s+\((.+)\)$')
DOC_TESTS = re.compile(r'^Doc-tests\s+(\S+)$')
RESULT = re.compile(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out; finished in ([\d.]+)s$')
TEST = re.compile(r'^test (.+?) \.\.\. (ok|FAILED|ignored)(?:,.*)?$')
SLOW = re.compile(r'^test (.+?) has been running for over (\d+) seconds$')
COMMAND = re.compile(r'^cargo (fmt|clippy|test|llvm-cov)(?:\s|$)')
FINISHED = re.compile(r'^Finished `([^`]+)` profile .* in (.*)$')
DURATION = re.compile(r'([\d.]+)(h|m|s)')


def iso(value):
    return value.isoformat().replace('+00:00', 'Z') if value else None


def seconds(start, end):
    return round((end - start).total_seconds(), 6) if start and end else None


def duration(value):
    pieces = DURATION.findall(value)
    return round(sum(float(n) * {'h': 3600, 'm': 60, 's': 1}[unit] for n, unit in pieces), 6) if pieces else None


def failure_mapping(name):
    """Source-audited candidates, never proof that a historical failure passed."""
    specific = 'candidate_specific_repair_pending_execution'
    mitigation = 'candidate_fixture_mitigation_pending_execution'
    unresolved = 'unresolved_pending_reproduction'
    def mapped(commit, disposition, area, relation):
        return {'commit': commit, 'disposition': disposition, 'area': area,
                'relation': relation, 'fresh_outcome': 'pending'}
    if name == 'cluster::migration::tests::restore_archive_with_retired_purpose_keys_admits_new_node_before_key_census':
        return mapped('a0c2c6730', specific, 'restore endpoint acquisition',
                      'owned restore child and endpoint acquisition repair candidate; fresh outcome pending')
    if name == 'provider::tests::durable_revoke_in_oauth_other_connection_fences_provider_effect':
        return mapped('d282a429', specific, 'notification broker durable admission fixture',
                      'single captured clock for ticket and delivery expiry; fresh outcome pending')
    if name.startswith('transcode::source_actor::tests::') or name.startswith('http::shared_source_playback::'):
        return mapped('4eee020e4', mitigation, 'Source fixture admission',
                      'shared fixture concurrency mitigation; individual root cause and fresh outcome unproven')
    direct = {
        'http::hls::tests::cached_owner_delivery_tests::cached_caption_revision_reaches_real_remote_owner_without_ingress_cache': 'caption owner learner-admission fixture',
        'http::remote::tests::remote_poll_byte_budget_retains_oversized_command_overflow': 'deterministic poll queue-expiry clock',
        'http::tests::every_registered_literal_route_has_an_explicit_group': 'subtitle route classification',
        'startup_tests::the_assembled_state_carries_the_configured_identity_and_directories': 'startup Tokio runtime and telemetry drain',
        'subtitle_translation::runtime::tests::actual_timeout_and_cancellation_reap_owned_child_group_before_return': 'isolated owned child cleanup',
    }
    if name in direct:
        return mapped('dbb6f27f9', specific, direct[name], 'specific source repair candidate; fresh outcome pending')
    if name == 'store_result::tests::production_store_results_never_use_bare_let_underscore':
        return mapped(None, unresolved, 'workspace Store-result discard census',
                      'no specific repair identified; obtain current outcome and offending source location')
    if name == 'telemetry::tests::a_store_outliving_its_writer_runtime_cannot_reuse_a_stale_sink':
        return mapped(None, unresolved, 'telemetry Store and sink registry lifetime',
                      'startup drainage may reduce interference but is not a demonstrated sink-lifetime repair')
    return mapped(None, 'unmapped_pending_reproduction', 'unclassified', 'unmapped; reproduce and diagnose')


def parse(lines):
    suites = []
    commands = []
    compiler = []
    issues = []
    metrics = []
    current = None
    command = None
    first = last = None
    terminal = None
    deferred_banners = []

    def new_suite(label, binary, line_no, stamp):
        record = {
            'ordinal': len(suites) + 1, 'target': label, 'binary': binary,
            'kind': 'doctest' if binary and binary.startswith('doctest:') else 'libtest' if binary else 'unidentified',
            'binary_stem': (binary if binary.startswith('doctest:') else re.sub(r'-[0-9a-f]{8,}$', '', binary)) if binary else None,
            'command_ordinal': command['ordinal'] if command else None,
            'banner_line': line_no, 'banner_at': iso(stamp), 'declared_tests': None,
            'status': 'incomplete', 'result': None, 'outcomes': [], 'slow_warnings': [],
        }
        suites.append(record)
        return record

    for line_no, raw in enumerate(lines, 1):
        raw = ANSI.sub('', raw.rstrip('\n'))
        matched = STAMP.match(raw)
        stamp = None
        if matched:
            stamp = datetime.fromisoformat(matched[1].replace('Z', '+00:00'))
            first = first or stamp
            last = stamp
            text = matched[2].strip()
        else:
            text = raw.strip()
        matched = COMMAND.match(text)
        if matched:
            if command:
                command['next_command_at'] = iso(stamp)
                command['window_until_next_command_s'] = seconds(command.pop('_start'), stamp)
            command = {'ordinal': len(commands) + 1, 'kind': matched[1], 'command': text,
                       'line': line_no, 'started_at': iso(stamp), '_start': stamp}
            commands.append(command)
        matched = FINISHED.match(text)
        if matched:
            compiler.append({'profile': matched[1], 'reported_s': duration(matched[2]),
                             'finished_at': iso(stamp), 'line': line_no,
                             'command_ordinal': command['ordinal'] if command else None})
        matched = RUNNING.match(text)
        if matched:
            binary = Path(matched[2]).name
            # A result stream can lag Cargo's next stderr banner. Delay assigning
            # the next suite until its "running N tests" line appears.
            deferred_banners.append((matched[1], binary, line_no, stamp))
        matched = DOC_TESTS.match(text)
        if matched:
            # A doctest stream has no executable path. Give it an identity that
            # cannot accidentally satisfy the crate's ordinary libtest inventory.
            deferred_banners.append((text, 'doctest:' + matched[1], line_no, stamp))
        matched = re.fullmatch(r'running (\d+) tests?', text)
        if matched:
            if current and current['result'] is None:
                issues.append({'line': line_no, 'kind': 'new_test_count_before_previous_result', 'suite': current['ordinal']})
            if deferred_banners:
                label, binary, banner_line, banner_stamp = deferred_banners.pop(0)
                current = new_suite(label, binary, banner_line, banner_stamp)
            else:
                current = new_suite('unidentified libtest stream', None, line_no, stamp)
                issues.append({'line': line_no, 'kind': 'missing_running_banner', 'suite': current['ordinal']})
            current['declared_tests'] = int(matched[1])
            current['tests_started_at'] = iso(stamp)
            current['_tests_start'] = stamp
        matched = TEST.match(text)
        if matched:
            if current is None or current['result'] is not None:
                issues.append({'line': line_no, 'kind': 'orphan_test_outcome', 'test': matched[1]})
            else:
                current['outcomes'].append({'name': matched[1], 'status': matched[2], 'at': iso(stamp), 'line': line_no})
        matched = SLOW.match(text)
        if matched:
            warning = {'name': matched[1], 'threshold_s': int(matched[2]), 'at': iso(stamp), 'line': line_no}
            if current is None or current['result'] is not None:
                issues.append({'line': line_no, 'kind': 'orphan_slow_warning', 'test': matched[1]})
            else:
                current['slow_warnings'].append(warning)
        matched = RESULT.match(text)
        if matched:
            if current is None or current['result'] is not None:
                issues.append({'line': line_no, 'kind': 'orphan_libtest_result'})
            else:
                fields = ['passed', 'failed', 'ignored', 'measured', 'filtered_out']
                result = {k: int(v) for k, v in zip(fields, matched.groups()[1:6])}
                result.update({'reported_s': float(matched[7]), 'at': iso(stamp), 'line': line_no})
                current['status'] = 'passed' if matched[1] == 'ok' else 'failed'
                current['result'] = result
                current['observed_test_window_s'] = seconds(current.pop('_tests_start', None), stamp)
                observed = Counter(outcome['status'] for outcome in current['outcomes'])
                for status, key in [('ok', 'passed'), ('FAILED', 'failed'), ('ignored', 'ignored')]:
                    if observed[status] != result[key]:
                        issues.append({'line': line_no, 'kind': 'outcome_count_mismatch', 'suite': current['ordinal'],
                                       'outcome': status, 'observed': observed[status], 'reported': result[key]})
                declared = sum(result[key] for key in ['passed', 'failed', 'ignored', 'measured'])
                if current['declared_tests'] is not None and declared != current['declared_tests']:
                    issues.append({'line': line_no, 'kind': 'declared_count_mismatch', 'suite': current['ordinal']})
        if text.startswith('PLURX_TEST_METRIC '):
            try:
                value = json.loads(text.removeprefix('PLURX_TEST_METRIC '))
                metrics.append({'at': iso(stamp), 'line': line_no, 'value': value})
            except json.JSONDecodeError:
                issues.append({'line': line_no, 'kind': 'invalid_metric_json'})
        if re.search(r'Job (?:failed|succeeded|success)', text, re.I):
            terminal = 'failed' if 'failed' in text.lower() else 'passed'

    if command:
        command['observed_to_log_end_s'] = seconds(command.pop('_start'), last)
    for label, binary, line_no, stamp in deferred_banners:
        new_suite(label, binary, line_no, stamp)
    for suite in suites:
        suite.pop('_tests_start', None)
    completed = [suite for suite in suites if suite['result'] is not None]
    failures = []
    groups = Counter()
    for suite in suites:
        for warning in suite['slow_warnings']:
            groups['::'.join(warning['name'].split('::')[:3])] += 1
        for outcome in suite['outcomes']:
            if outcome['status'] == 'FAILED':
                failures.append({'suite': suite['ordinal'], 'binary': suite['binary'], **outcome,
                                 'repair_candidate': failure_mapping(outcome['name'])})
    totals = {key: sum(suite['result'][key] for suite in completed)
              for key in ['passed', 'failed', 'ignored', 'measured', 'filtered_out']}
    totals['reported_test_s'] = round(sum(suite['result']['reported_s'] for suite in completed), 6)
    totals['slow_warnings'] = sum(len(suite['slow_warnings']) for suite in suites)
    return {
        'schema_version': 1, 'first_at': iso(first), 'last_at': iso(last),
        'observed_log_span_s': seconds(first, last), 'job_terminal_status': terminal or 'not observed',
        'suite_count': len(suites), 'completed_suite_count': len(completed),
        'incomplete_suite_count': len(suites) - len(completed), 'totals': totals,
        'commands': commands, 'compiler': compiler, 'suites': suites, 'failures': failures,
        'slow_warning_groups': dict(groups.most_common()), 'metric_samples': metrics,
        'parser_issues': issues,
        'limitations': [
            'Command windows are log observations, not isolated command CPU time.',
            'libtest slow warnings include scheduling and waits; they are not per-test durations.',
            'Summed suite seconds need not equal wall time if binaries overlap.',
            'No result means incomplete or missing evidence, never zero tests or a pass.',
            'Repair candidates require exact-candidate regression and full-suite qualification.',
        ],
    }


def reconcile(report, expected=None, previous=None):
    """Missing/renamed cases require explicit evidence; absence is not a pass."""
    expected = expected or {}
    gaps = []
    for binary in expected.get('binaries', []):
        observed = [suite for suite in report['suites'] if suite['binary_stem'] == binary]
        if not observed:
            gaps.append({'binary': binary, 'status': 'not observed'})
        elif any(suite['result'] is None for suite in observed):
            gaps.append({'binary': binary, 'status': 'observed without complete result'})
    cases = {}
    for suite in report['suites']:
        for outcome in suite['outcomes']:
            cases.setdefault(outcome['name'], []).append(outcome['status'])
    for name in expected.get('required_test_cases', []):
        observed = cases.get(name, [])
        if 'ok' not in observed or 'FAILED' in observed:
            gaps.append({'test': name, 'status': 'not passed', 'observed': observed})
    report['inventory_gaps'] = gaps
    checks = []
    for failure in (previous or {}).get('failures', []):
        name = failure['name']
        observed = cases.get(name, [])
        status = ('failed' if 'FAILED' in observed else 'passed' if 'ok' in observed
                  else 'ignored' if 'ignored' in observed else 'missing')
        checks.append({'name': name, 'status': status, 'observed': observed,
                       'repair_candidate': failure.get('repair_candidate')})
    report['previous_failure_recheck'] = checks
    report['previous_failure_recheck_counts'] = dict(Counter(check['status'] for check in checks))
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--expected-inventory', type=Path,
                        help='JSON with binaries (hash-free stems) and required_test_cases lists')
    parser.add_argument('--previous-report', type=Path,
                        help='Reconcile every previously failed test ID against this log')
    args = parser.parse_args()
    with args.log.open(encoding='utf-8', errors='replace') as source:
        report = parse(source)
    reconcile(report,
              json.loads(args.expected_inventory.read_text()) if args.expected_inventory else None,
              json.loads(args.previous_report.read_text()) if args.previous_report else None)
    serialized = json.dumps(report, indent=2, sort_keys=True) + '\n'
    if args.output:
        args.output.write_text(serialized, encoding='utf-8')
    else:
        print(serialized, end='')


if __name__ == '__main__':
    main()
