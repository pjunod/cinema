"""One truthful local selected-class capture, only for a schema-2 PR gate.

The original custody states direct checkout and post-execution byte inspection.
These observations are never relabelled as CI, an input archive, or a hash
measured during execution. Existing complete-cohort accounting remains required.
"""
import base64
import io
import json
import re
import tarfile
from validation import python_unit_receipts as r
from validation import promotion_unit_retention as retention

POLICY = 'selected-pr-android-local-v1'
# Reviewed original tool custody, not an asserted new execution.
CUSTODY_SHA256 = '3c093595d9189b47de70cf07d7b9c99406fd7b4ebc3937be1d0b1c99f841b3c8'


def source_methods(raw, klass):
    """Closed Kotlin JUnit4 shape; no runners, inheritance or parameterization."""
    text = raw.decode()
    name = klass.rsplit('.', 1)[-1]
    r.require(re.findall(r'(?m)^class\s+(\w+)\s*\{', text) == [name]
              and not re.search(r'(?m)^(?:open|abstract|sealed|data)\s+class\b|@(?:RunWith|Ignore|Parameterized)\b', text)
              and re.search(r'(?m)^package\s+' + re.escape(klass.rsplit('.', 1)[0]) + r'\s*$', text),
              'Selected class has an unsupported JUnit source shape')
    names = re.findall(r'@Test\s+fun\s+(\w+)\s*\(', text)
    r.require(names and len(names) == len(set(names)) == len(re.findall(r'@Test\b', text)),
              'Selected source has ambiguous or unsupported test declarations')
    return {klass + '/' + name for name in names}


def outcomes(proof, record, raw, universe, candidate, selected_pr, clean_log):
    from validation import nonrust_unit_cohort as cohort
    from validation.retained_lane_commands import tree_id
    r.require(selected_pr is True and record['origin']['kind'] == 'attested-local',
              'Local selected capture is only ordinary-PR source-gate evidence')
    execution = record['origin']['execution']
    r.require(set(execution) == {'mode', 'kind', 'selected_pr_local'}
              and execution['mode'] == 'source-cohort-v1' and execution['kind'] == 'android_jvm',
              'Unknown selected local capture execution')
    refs = execution['selected_pr_local']
    r.require(set(refs) == {'version', 'custody', 'wrapper', 'daemon'} and refs['version'] == POLICY,
              'Unknown selected local custody contract')
    custody_raw = cohort.blob(proof, refs['custody'])
    r.require(retention.digest(custody_raw) == CUSTODY_SHA256,
              'Original selected local custody was changed or not reviewed')
    custody = r.bounded_json(custody_raw)
    r.require(custody['schema'] == 'android-focused-execution-custody-v1'
              and record['source_commit'] == custody['source_commit']
              and record['origin']['command'] == custody['argv']
              and record['exit_code'] == custody['exit_code'] == custody['wrapper']['tool_exit_code'] == 0
              and retention.digest(raw) == custody['raw_log_sha256']
              and retention.digest(cohort.blob(proof, refs['wrapper'])) == custody['wrapper']['sha256']
              and retention.digest(cohort.blob(proof, refs['daemon'])) == custody['gradle_daemon']['log_sha256'],
              'Original source, recipe, log or executor custody differs')
    r.require(custody['execution_source_mode'] == 'direct-clean-checkout'
              and custody['archive_sha256'] is None and custody['source_clean_start']['value'] is True
              and custody['source_head_end']['value'] == custody['source_commit']
              and custody['source_clean_end']['value'] is True
              and tree_id(custody['source_commit'], 'clients/android') == tree_id(candidate, 'clients/android'),
              'Selected Android source changed since actual execution')
    # The version/binary post-observation is explicit in the immutable custody;
    # its hash is not converted to a historical measured runtime assertion.
    r.require(custody['post_observations']['java_binary_sha256']
              == '814dc07211a49b57d8911d6e95fb8c8882ebbd1860dbee910b2f38390c2d4014'
              and custody['java_version_before'] == custody['post_observations']['java_version']
              and custody['gradle_daemon']['observed_java_home'] == custody['java_home'],
              'Reviewed actual Java observations differ')
    argv = custody['argv']
    prefix = ['./gradlew', '--no-daemon', '--offline', '--max-workers=2', ':app:testDebugUnitTest']
    r.require(argv[:5] == prefix and len(argv) > 5 and (len(argv) - 5) % 2 == 0
              and all(flag == '--tests' for flag in argv[5::2]), 'Selected Gradle recipe differs')
    classes = argv[6::2]
    r.require(len(classes) == len(set(classes)), 'Repeated selected class')
    expected = set()
    for klass in classes:
        path = 'clients/android/app/src/test/java/' + klass.replace('.', '/') + '.kt'
        source = retention.git_bytes(custody['source_commit'], path)
        r.require(source is not None, 'Selected original class source missing')
        expected.update(source_methods(source, klass))
    r.require(expected <= universe, 'Selected classes left current complete JVM inventory')
    coverage = record['coverage']
    r.require(set(coverage) == {'mode', 'xml_archive'} and coverage['mode'] == 'junit-xml',
              'Selected capture needs actual native JUnit XML')
    envelope = r.bounded_json(cohort.blob(proof, coverage['xml_archive']))
    archive = base64.b64decode(envelope['base64'], validate=True)
    r.require(0 < len(archive) <= r.MAX_BYTES and retention.digest(archive) == envelope['sha256'],
              'Selected XML archive digest differs')
    original_xml = custody['junit_xml']
    found = set()
    with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as tar:
        for member in tar:
            r.require(member.isfile() and member.name in original_xml and member.name not in found
                      and member.size == original_xml[member.name]['size'],
                      'Selected XML has a missing, duplicate or foreign member')
            found.add(member.name)
            r.require(retention.digest(tar.extractfile(member).read()) == original_xml[member.name]['sha256'],
                      'Original selected native XML bytes differ')
    r.require(found == set(original_xml), 'Original selected native XML member missing')
    actual = cohort.junit_archive(proof, coverage['xml_archive'], envelope['sha256'], universe)
    text = clean_log(raw.decode())
    r.require(set(actual) == expected and all(value == 'pass' for value in actual.values())
              and text.splitlines().count('> Task :app:testDebugUnitTest') == 1
              and len(re.findall(r'^BUILD SUCCESSFUL in .+$', text, re.M)) == 1
              and not re.search(r'^BUILD FAILED|^FAILURE:', text, re.M),
              'Selected native XML is partial, foreign or contradicts execution')
    return actual


MEASURED_POLICY = 'selected-pr-android-measured-v1'


def measured_outcomes(proof, record, raw, universe, candidate, selected_pr, clean_log):
    """New captures measure archive and executable bytes before AND after."""
    from validation import nonrust_unit_cohort as cohort
    from validation.qualification_unit_continuation import source_archive_digest
    from validation.retained_lane_commands import tree_id
    r.require(selected_pr is True and record['origin']['kind'] == 'attested-local',
              'Measured local capture is only ordinary-PR source-gate evidence')
    execution = record['origin']['execution']
    r.require(set(execution) == {'mode', 'kind', 'selected_pr_local'}
              and execution['mode'] == 'source-cohort-v1' and execution['kind'] == 'android_jvm',
              'Unknown measured selected execution')
    refs = execution['selected_pr_local']
    r.require(set(refs) == {'version', 'binding'} and refs['version'] == MEASURED_POLICY,
              'Unknown measured selected binding')
    binding = r.bounded_json(cohort.blob(proof, refs['binding']))
    r.require(set(binding) == {'schema', 'source_commit', 'source_archive_sha256', 'argv',
              'source_head_start', 'source_head_end', 'source_clean_start', 'source_clean_end',
              'started_utc', 'finished_utc', 'exit_code', 'log_sha256', 'xml_archive_sha256',
              'java_version_before', 'java_version_after', 'java_binary_sha256_before',
              'java_binary_sha256_after', 'platform', 'gradle_semantic_environment',
              'gradle_init_scripts', 'ids_sha256'} and binding['schema'] == MEASURED_POLICY,
              'Measured selected custody fields differ')
    source = r.sha(record['source_commit'])
    r.require(binding['source_commit'] == binding['source_head_start'] == binding['source_head_end'] == source
              and binding['source_clean_start'] is True and binding['source_clean_end'] is True
              and binding['source_archive_sha256'] == source_archive_digest(source)
              and tree_id(source, 'clients/android') == tree_id(candidate, 'clients/android')
              and binding['exit_code'] == record['exit_code']
              and type(binding['exit_code']) is int and binding['exit_code'] in {0, 1}
              and binding['log_sha256'] == retention.digest(raw),
              'Measured source/archive/cleanliness or actual completion differs')
    r.require(binding['java_version_before'] == binding['java_version_after']
              and binding['java_version_before'].startswith('openjdk version "25.0.4"')
              and binding['java_binary_sha256_before'] == binding['java_binary_sha256_after']
              == '814dc07211a49b57d8911d6e95fb8c8882ebbd1860dbee910b2f38390c2d4014'
              and binding['platform'] == 'darwin-arm64'
              and binding['gradle_semantic_environment'] == {} and binding['gradle_init_scripts'] == [],
              'Measured Java or Gradle semantic environment differs')
    from datetime import datetime
    start = datetime.fromisoformat(binding['started_utc'].replace('Z', '+00:00'))
    finish = datetime.fromisoformat(binding['finished_utc'].replace('Z', '+00:00'))
    r.require(start.utcoffset() is not None and finish.utcoffset() is not None and start < finish,
              'Measured capture interval is invalid')
    r.require(binding['ids_sha256'] == retention.digest(
              json.dumps(sorted(universe), separators=(',', ':')).encode()),
              'Measured current compiled inventory differs')
    argv = binding['argv']
    prefix = ['./gradlew', '--no-daemon', '--offline', '--max-workers=2', ':app:testDebugUnitTest']
    r.require(isinstance(argv, list) and argv[:5] == prefix and len(argv) > 5
              and (len(argv) - 5) % 2 == 0 and argv == record['origin']['command']
              and all(flag == '--tests' for flag in argv[5::2]), 'Measured exact Gradle recipe differs')
    names = argv[6::2]
    r.require(len(names) == len(set(names)) and all(isinstance(name, str) for name in names),
              'Repeated measured selector')
    # Match full method IDs against the independently source-owned compiled
    # census. A class or prefix selector cannot stand for named executions.
    mapping = {test_id.replace('/', '.'): test_id for test_id in universe}
    r.require(set(names) <= set(mapping), 'Unknown, class or prefix Gradle selector')
    selected = {mapping[name] for name in names}
    coverage = record['coverage']
    r.require(set(coverage) == {'mode', 'xml_archive'} and coverage['mode'] == 'junit-xml',
              'Measured selection lacks native XML')
    actual = cohort.junit_archive(proof, coverage['xml_archive'], binding['xml_archive_sha256'], universe)
    text = clean_log(raw.decode())
    success = len(re.findall(r'^BUILD SUCCESSFUL in .+$', text, re.M))
    failed = len(re.findall(r'^BUILD FAILED in .+$', text, re.M))
    r.require(set(actual) == selected
              and len(re.findall(r'^> Task :app:testDebugUnitTest(?: FAILED)?$', text, re.M)) == 1
              and ((binding['exit_code'] == 0 and success == 1 and failed == 0
                    and all(value == 'pass' for value in actual.values()))
                   or (binding['exit_code'] == 1 and failed == 1 and success == 0
                       and any(value == 'fail' for value in actual.values()))),
              'Actual selected XML or command disposition contradicts execution')
    return actual
