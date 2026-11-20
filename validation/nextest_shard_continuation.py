"""Authenticated selected-PR Rust continuation; never full-workspace evidence.

The source adapter retains original nextest outcomes under original content
keys, then applies a writer-reviewed source manifest. Current execution keeps
nextest process isolation; no nextest result is relabelled as Cargo/libtest.
"""
from __future__ import annotations

import base64
import io
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import sys
import time
import subprocess
import tarfile
import urllib.request
import zipfile

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r
from validation import rust_units as units

POLICY = 'workspace-nextest-shards-v1'
COMPATIBILITY = 'linux-x64-nextest-0.9.151-isolated-two-8m-v1'
NEXTEST_SHA256 = '1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4'
IMAGE_DIGEST = '6a989b4d92f55b29befa9eae08f9cd59940b3067d771629549ebf5ab1f4d433d'
FFMPEG_VERSION = '8.1.3-Jellyfin'
TEST_INPUTS = {
    'crates/plurxd/src/http/applied_route_inventory.rs',
    'crates/plurxd/src/http/stream/tests.rs',
    'crates/plurxd/src/transcode/tests/chunk_05.rs',
}
# Reviewed clock/quality inputs still require exact hashes and affected-body witnesses.
RUNTIME_INPUTS = {'clients/android/app/build.gradle.kts',
 'clients/android/README.md',
 'clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt',
 'clients/android/app/src/test/java/tv/plurx/app/player/PlaybackIntentTest.kt',
 'clients/android/app/src/test/java/tv/plurx/app/player/PreparedSwitchMeasurementTest.kt',
 'crates/plurx-core/src/segplan.rs',
 'crates/plurx-core/src/segplan/copy_cursor.rs',
 'crates/plurx-core/src/segplan/ranges.rs',
 'crates/plurx-core/src/store/fragindex.rs',
 'crates/plurx-core/src/store/fragment_index_cluster.rs',
 'crates/plurx-core/src/store/telemetry.rs',
 'crates/plurx-core/tests/store_contract.rs',
 'crates/plurxd/src/copy_analysis/quarantine_tests.rs',
 'crates/plurxd/src/copy_index_v3/tests.rs',
 'crates/plurxd/src/fragindex.rs',
 'crates/plurxd/src/fragindex/refit_acceptance_tests.rs',
 'crates/plurxd/src/fragment_index_cluster.rs',
 'crates/plurxd/src/http/browse.rs',
 'crates/plurxd/src/prodsched.rs',
 'crates/plurxd/src/renditiondir.rs',
 'crates/plurxd/src/subtitle_source.rs',
 'crates/plurxd/src/titlestore.rs',
 'crates/plurxd/src/vod/generation.rs',
 'crates/plurxd/src/vod/serve/construct.rs',
 'crates/plurxd/src/vod/session.rs',
 'crates/plurxd/src/vod/tests/chunk_01.rs',
 'crates/plurxd/src/vod/tests/chunk_03.rs',
 'crates/plurxd/src/vodgen.rs',
 'crates/plurx-core/src/store/background_jobs.rs',
 'crates/plurx-core/src/store/dv_title_jobs.rs',
 'crates/plurx-core/tests/store_contract/dv_title_jobs.rs',
 'crates/plurxd/src/copyseg.rs',
 'crates/plurxd/src/dv_runtime.rs',
 'crates/plurxd/src/dv_segment.rs',
 'crates/plurxd/src/http/hls/preparation.rs',
 'crates/plurxd/src/http/hls/processed_upgrade.rs',
 'crates/plurxd/src/source_probe/fel_qualification.rs',
 'crates/plurxd/src/source_probe/title_work.rs',
 'crates/plurxd/src/source_probe/title_work_tests.rs',
 'crates/plurxd/src/transcode.rs',
 'crates/plurxd/src/transcode/manager/construct.rs',
 'crates/plurxd/src/transcode/manager/create.rs',
 'crates/plurxd/src/transcode/manager/native_fel.rs',
 'crates/plurxd/src/transcode/manager/processed_reuse_tests.rs',
 'crates/plurxd/src/transcode/manager/publication.rs',
 'crates/plurxd/src/transcode/manager/retained_recovery_tests.rs',
 'crates/plurxd/src/transcode/manager/start.rs',
 'crates/plurxd/src/transcode/pretranscode/source.rs',
 'crates/plurxd/src/transcode/producer/prepublication_executor.rs',
 'crates/plurxd/src/transcode/rolling/retention.rs',
 'crates/plurxd/src/transcode/rolling/session.rs',
 'crates/plurxd/src/transcode/tests/chunk_03.rs',
}

# Reviewed recipes, not a source-commit or pull-request exception. Different
# workflow, selector, feature, image or nextest contracts need a new review.
HARNESS = {'.config/nextest.toml': 'b68e6d071b1e441727237de221cb60ac32e78f0ddff3d9636d9004f1e8d38c81',
 '.github/workflows/main-fast-lane.yml': '3ce991087d99bc508988d674d37ca48eb99cec34ce0d18af4fc36ec7c0878e22',
 '.github/workflows/rust-units.yml': 'e9ea37562c132dcf815336a46fa954fe7e0db841527dcff16f5e98d0b0ef93c2',
 'rust-toolchain.toml': '03cbfb27fcdc8253524b3e4c71c832457a504f7e9b1da5e6220178404b77f4e4',
 'validation/rust-unit-inputs.toml': '009d3107a038f79ed3986d87c96d247717bb6328a89ec179a5a2cbf0255a2372',
 'validation/rust_units.py': 'c97c1469068209b7b9b2eb0413d8984dd338051a9e4c996843d063683bb3d683'}


MAIN_SCOPE_ENV = '        env:\n          VALIDATION_BASE: ${{ github.event.pull_request.base.sha }}\n          MERGE_TRAIN_BOT: ${{ vars.MERGE_TRAIN_BOT }}\n          MERGE_TRAIN: ${{ vars.MERGE_TRAIN }}\n'
MANUAL_SCOPE_ENV = '        env:\n          GITHUB_TOKEN: ${{ github.token }}\n          VALIDATION_BASE: ${{ github.event.pull_request.base.sha }}\n          MERGE_TRAIN_BOT: ${{ vars.MERGE_TRAIN_BOT }}\n          MERGE_TRAIN: ${{ vars.MERGE_TRAIN }}\n'


def projected_main_workflow(raw):
    r.require(isinstance(raw, bytes), 'Missing main-lane workflow source')
    projected = raw.replace(MANUAL_SCOPE_ENV.encode(), MAIN_SCOPE_ENV.encode()) if raw.count(MANUAL_SCOPE_ENV.encode()) == 1 else raw
    plumbing = b"          PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}\n          PLURX_PAPERWORK_CONTINUATION: preliminary\n"
    if b'PLURX_PAPERWORK_CONTINUATION' in projected:
        start, end = projected.find(b'  paperwork:\n'), projected.find(b'\n  scope:\n')
        r.require(0 <= start < end and projected.count(plumbing) == 1
                  and projected[start:end].count(plumbing) == 1,
                  'Paperwork continuation plumbing escaped its original job')
        projected = projected.replace(plumbing, plumbing.splitlines(keepends=True)[0], 1)
    r.require(retention.digest(projected) == HARNESS['.github/workflows/main-fast-lane.yml'],
              'Main-lane compile/test recipe changed beyond exact authenticated-scope credential plumbing')
    return projected


def source_selection(candidate, base, repository='.'):
    """Unchanged Keys over one closed metadata-only workflow projection.

    Standard rust_units selection/cache keys are never altered or published.
    This explicitly typed source gate separates the actual workflow hash from
    its reviewed byte-identical compiler/test recipe before classifying bodies.
    """
    trees, keys, witness = [], [], {}
    for label, revision in (('head', candidate), ('base', base)):
        tree = units.Tree(revision, repository)
        path = '.github/workflows/main-fast-lane.yml'
        actual = tree.text(path).encode()
        projected = projected_main_workflow(actual)
        mode, _ = tree.entries[path]
        blob = hashlib.sha1(b'blob ' + str(len(projected)).encode() + b'\0' + projected).hexdigest()
        tree.entries[path] = (mode, blob)
        tree._blobs[path] = projected
        config = units.Config.load(tree.text(str(units.CONFIG)))
        trees.append(tree)
        keys.append(units.Keys(tree, config))
        witness[label] = {'actual_sha256': retention.digest(actual), 'recipe_sha256': retention.digest(projected)}
    head, baseline = keys
    rows = {}
    for name in head.units:
        key = head.key(name)
        base_key = baseline.key(name) if name in baseline.crates else None
        only = (head.dynamic_tests(name) if base_key is not None and key != base_key
                and head.key(name, False) == baseline.key(name, False) else None)
        rows[name] = {'key': key, 'base_key': base_key, 'due': key != base_key, 'only': only}
    return {'head': trees[0].commit, 'base': trees[1].commit, 'crates': rows,
            'projection': {'policy': 'authenticated-manual-scope-env-only-v1', 'workflow': witness}}


def strict_outcomes(raw):
    result = {}
    for line in raw.decode('utf-8', 'strict').splitlines():
        line = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line)
        match = units.STATUS_LINE.fullmatch(line)
        if match:
            status, binary, name = match.groups()
            identity = binary + ' ' + name
            r.require(identity not in result, 'Duplicate nextest body outcome')
            result[identity] = 'PASS' if status in {'PASS', 'LEAK'} else 'FAIL'
    r.require(result, 'No original nextest outcomes')
    return result


def environment_witness(raw, *, archive=False):
    text = raw.decode('utf-8', 'strict')
    lines = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', text, flags=re.M).splitlines()
    r.require('@sha256:' + IMAGE_DIGEST in text and 'cargo-nextest 0.9.151 ' in text,
              'Original nextest image or executable witness differs')
    if archive:
        r.require(all(value in lines for value in ('release: 1.97.1',
                  'commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452',
                  'host: x86_64-unknown-linux-gnu')), 'Original compiler witness differs')
    else:
        r.require('expected major=8 resolved=ffmpeg version ' + FFMPEG_VERSION + ' ' in text,
                  'Original FFmpeg runtime witness differs')


class ForgeView:
    """Reuse the original exact-key authenticator with its bounded API."""
    def __init__(self, api):
        self.api = api

    def get(self, path, query=None):
        return self.api.get(path, query)

    def bytes(self, path, query=None, limit=units.MAX_RECEIPT_BYTES):
        data = self.api.bytes(path, query)
        r.require(len(data) <= limit, 'Nextest evidence exceeds original bound')
        return data


def artifact(api, metadata, member):
    r.require(metadata.get('expired') is False
              and 0 < metadata['size_in_bytes'] <= units.MAX_RECEIPT_BYTES,
              'Unavailable nextest artifact')
    raw = api.bytes(f"/actions/artifacts/{metadata['id']}/zip")
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        r.require(len(entries) == 1 and entries[0].filename == member
                  and not entries[0].is_dir() and entries[0].file_size <= units.MAX_RECEIPT_BYTES,
                  'Ambiguous nextest artifact members')
        return r.bounded_json(archive.read(entries[0]))


COMPILE_ONLY_NONE_PATHS = {
    'crates/plurx-core/src/segplan/copy_cursor.rs',
    'crates/plurx-core/src/segplan/ranges.rs',
    'crates/plurxd/src/prodsched.rs',
    'crates/plurxd/src/renditiondir.rs',
    'crates/plurxd/src/titlestore.rs',
    'crates/plurxd/src/vod/tests/chunk_01.rs',
    'crates/plurxd/src/vod/serve/construct.rs',
}


def compile_only_none(path, witness, before, after):
    """A literal absent row field changes compilation, with no runtime value."""
    r.require(path in COMPILE_ONLY_NONE_PATHS and witness['affected'] == []
              and witness.get('compile_only') == {'kind': 'none-row-field-initializer'}
              and before is not None and after is not None,
              'Unknown empty Rust impact witness')
    insertions = list(re.finditer(rb'(?m)^[ \t]+first_cto: None,\r?\n', after))
    r.require(any(after[:m.start()] + after[m.end():] == before for m in insertions),
              'Compile-only witness changes more than one literal None initializer')


IGNORED_COMPILE_PATH = 'crates/plurxd/src/fragindex/refit_acceptance_tests.rs'
IGNORED_COMPILE_ID = 'plurxd::bin/plurxd fragindex::refit_acceptance_tests::real_fitted_index_and_seek_publish_stored_curves'
# Actual metadata-only list of authenticated original archive12386, pinned
# nextest0.9.151/image/Rust1.97.1 on qualified Linux5.15, zero test bodies.
PRIOR_LISTING_SOURCE = '9ab3590df2d9513fa73997dbdad6887038b97f84'
PRIOR_LISTING_RUN = 8687
PRIOR_LISTING_SHA256 = 'f6caa6f7b81d1ab9500358bae8b4867a86f4273664a1017aea0ac3301bbe8abd'


def ignored_source_projection(raw):
    r.require(raw is not None, 'Missing original ignored-only source')
    text = raw.decode()
    r.require(re.search(r'(?m)^#\[ignore = "[^"\n]+"\]\nasync fn real_fitted_index_and_seek_publish_stored_curves\(', text),
              'Physical acceptance test is no longer explicitly ignored')
    for name in ('measured_landing', 'real_fitted_index_and_seek_publish_stored_curves'):
        pattern = r'(?ms)^(?:async )?fn ' + name + r'\b.*?^}\r?\n'
        matches = list(re.finditer(pattern, text))
        r.require(len(matches) == 1, 'Ambiguous ignored-only top-level function')
        text = re.sub(pattern, '/* isolated ignored-only function: ' + name + ' */\n', text)
    r.require(not re.search(r'\bmeasured_landing\b', text.replace('/* isolated ignored-only function: measured_landing */', '')),
              'Ignored helper has a caller outside the ignored test')
    return text


def compile_only_ignored(path, witness, before, after, universe, *, prior, run):
    r.require(prior == PRIOR_LISTING_SOURCE and run == PRIOR_LISTING_RUN,
              'Sealed original ignored discovery belongs to another source/run')
    item = witness.get('compile_only')
    r.require(path == IGNORED_COMPILE_PATH and witness['affected'] == []
              and isinstance(item, dict) and set(item) == {'kind', 'test', 'prior_listing'}
              and item['kind'] == 'ignored-test-only' and item['test'] == IGNORED_COMPILE_ID
              and IGNORED_COMPILE_ID not in universe,
              'Unknown ignored-only empty impact witness')
    envelope = item['prior_listing']
    r.require(isinstance(envelope, dict) and set(envelope) == {'sha256', 'base64'}
              and envelope['sha256'] == PRIOR_LISTING_SHA256
              and isinstance(envelope['base64'], str) and len(envelope['base64']) <= r.MAX_BYTES,
              'Missing sealed original full nextest discovery')
    raw = base64.b64decode(envelope['base64'], validate=True)
    r.require(retention.digest(raw) == PRIOR_LISTING_SHA256, 'Original full nextest discovery changed')
    listing = r.bounded_json(raw)
    binary, name = IGNORED_COMPILE_ID.split(' ', 1)
    case = listing['rust-suites'][binary]['testcases'][name]
    r.require(case.get('ignored') is True, 'Original physical test was not ignored')
    suites = units.nextest_suites(listing)
    ordinary = {b + ' ' + n for b, cases in suites.items() for n, ignored in cases.items() if not ignored}
    all_cases = {b + ' ' + n for b, cases in suites.items() for n in cases}
    r.require(ordinary <= universe and universe <= all_cases,
              'Original compiled listing contradicts selected body census')
    r.require(ignored_source_projection(before) == ignored_source_projection(after),
              'Empty ignored witness changes selected source outside its isolated functions')




# Finite reviewed add-only test regions; existing body/helper bytes stay intact.
# The retained-recovery region also adds one helper used only by its new tests.
REVIEWED_TEST_ADDITIONS = {'crates/plurx-core/tests/store_contract/dv_title_jobs.rs': ('9b2c9464592471fe80110c70c12ca34da03c6aaebcf7c67fc87ae78ad9e413a2',
                                                             '5bcf53c4f3bd8a98b21c6940a102b7c80e8ace99524c07f8d47780ea3bc79bcc',
                                                             ['plurx-core::store_contract '
                                                              'dv_title_jobs::fel_head_rebuild_reopens_exact_settled_title_without_remeasurement']),
 'crates/plurxd/src/transcode/manager/processed_reuse_tests.rs': ('a06ff694f6045a785ab7cd1d69c2ab24d8573cfd7f99d00b1957244ffe03448a',
                                                                  '3aad281fcdf6ce36710e84d886b1ec0670c8ba38e3c72d902177737b2453c409',
                                                                  ['plurxd::bin/plurxd '
                                                                   'transcode::manager_create::processed_reuse_tests::native_fel_window_requires_reaped_success_and_keeps_verified_evidence',
                                                                   'plurxd::bin/plurxd '
                                                                   'transcode::manager_create::processed_reuse_tests::prepared_fel_batch_is_bound_to_first_snapshot_and_attempt']),
 'crates/plurxd/src/transcode/manager/retained_recovery_tests.rs': ('ce2e4b93fb318ecb6d8365e84ffbed47b41305c9a98f544fa81aac71fafbd42e',
                                                                    'c6e06af4c6cc17ef167feda2ad17b9a4106eb618de8eb309935405cb4983610a',
                                                                    ['plurxd::bin/plurxd '
                                                                     'transcode::manager_create::retained_recovery_tests::ready_native_fel_admission_is_limited_to_the_original_auto_copy',
                                                                     'plurxd::bin/plurxd '
                                                                     'transcode::manager_create::retained_recovery_tests::clustered_initial_vod_start_primes_a_cold_native_fel_head']),
 'crates/plurxd/src/transcode/tests/chunk_03.rs': ('6327189e8151f4a38d4ae10174d641a0f8fb834a0f89dc332a9b712850f0783a',
                                                   'ba4ed0fee02bed2ea5f7534d311648074ef2b9ef11c553b1e18595960c6b4fa7',
                                                   ['plurxd::bin/plurxd '
                                                    'transcode::tests::pending_fel_head_only_defers_the_first_playable_snapshot'])}


# Independent source-domain review: no original selected body reaches these
# new native-FEL branches. Exact bytes close the scope; new cases remain due.
REVIEWED_NATIVE_FEL_EMPTY = {'crates/plurxd/src/copyseg.rs': ('a0b804b7bdc425920e9eb1dbd7513436a66c4b9782c7bc56004d9e72149efd32',
                                  '63c528924ef200b9d8a713f5c3b87181cc960c6c8d6dfa988a6ead7dac91ef16'),
 'crates/plurxd/src/dv_runtime.rs': ('fe044d8501cc2b914efc38e4fd5258ed6388540c9faba0a1cb77f796900320cc',
                                     'efcb84a9e6c24e6272f89675d652b5c547119d0478d50c72a5332e67db8a31b0'),
 'crates/plurxd/src/http/hls/preparation.rs': ('bd13aa74afef1b5bafdd0e8f59b9deb7eb19477d7dc8f486295c89af26da30ac',
                                               'b9c1a1ca020674a99bf57ce87f789a7dfea6126d5a274a49fd923bbb582dc0f2'),
 'crates/plurxd/src/http/hls/processed_upgrade.rs': ('f38da86d8fa5e3016dccf8328925f769062c5f6f83761cb7715f81d4eee57507',
                                                     'e70eb713b507365a5084d3490ec6e2b6c41517b84c4c4f9428600a80eb08f553'),
 'crates/plurxd/src/transcode/manager/publication.rs': ('06129e18d5c55e0a2876a0cb2cd9bfcab8e3ebc4725052fb28f7ae50d51d6509',
                                                        '2f70ab1e324eedda8ffcfa68a94d2c356d13827a7541084169bafbd5e5deeba0'),
 'crates/plurxd/src/transcode/manager/start.rs': ('0ca9229fe85db24ce05c0cfede2919b1f0613fa97b668c2eaa85c2be4820afa8',
                                                  '9d5c2895b9fdaf1879afd7d8d4bbbc0275b55a781f4f78bb882717b9751d6c05'),
 'crates/plurxd/src/transcode/producer/prepublication_executor.rs': ('d9908acc06e2d99dda60c7beb9f1f68ddf268b7551612a52b94148eb820c5c35',
                                                                     '12608d59f6bb8a88c5052e87e292c342f7fbf4ea3f582db7003306357e5a2937'),
 'crates/plurxd/src/transcode/rolling/retention.rs': ('3319d4ce21002613774a3d0344dabc3908ca5e914a84fe94f798fd1792627ba1',
                                                      'e07aaae8857271dfd69d44483670c454849b3b49b45b442d8d8883bc48ca206e'),
 'crates/plurxd/src/transcode/rolling/session.rs': ('3ebaef95422f3499106a2de248a90f85e87d7b75b4764b369ec27d479b82ad00',
                                                    '62b2d2a085afb011a66a03eda00956fbb821370f4669e8af759aa09e56f09580')}
NATIVE_FEL_CURRENT_ADDITIONS = ['plurx-core::store_contract '
 'dv_title_jobs::fel_head_rebuild_reopens_exact_settled_title_without_remeasurement',
 'plurxd::bin/plurxd copyseg::tests::prepared_fel_defers_first_snapshot_once_and_falls_back',
 'plurxd::bin/plurxd '
 'copyseg::tests::prepared_native_fel_keeps_the_single_session_when_split_activation_fails',
 'plurxd::bin/plurxd '
 'source_probe::fel_qualification::tests::foreign_pending_recipe_does_not_block_current_background_qualification',
 'plurxd::bin/plurxd '
 'transcode::manager_create::processed_reuse_tests::native_fel_window_requires_reaped_success_and_keeps_verified_evidence',
 'plurxd::bin/plurxd '
 'transcode::manager_create::processed_reuse_tests::prepared_fel_batch_is_bound_to_first_snapshot_and_attempt',
 'plurxd::bin/plurxd '
 'transcode::manager_create::retained_recovery_tests::clustered_initial_vod_start_primes_a_cold_native_fel_head',
 'plurxd::bin/plurxd '
 'transcode::manager_create::retained_recovery_tests::ready_native_fel_admission_is_limited_to_the_original_auto_copy',
 'plurxd::bin/plurxd '
 'transcode::manager_start::prepared_native_fel_requires_the_full_startup_scratch_grant',
 'plurxd::bin/plurxd transcode::tests::pending_fel_head_only_defers_the_first_playable_snapshot']


def compile_only_native_fel(path, witness, before, after, universe):
    pair = REVIEWED_NATIVE_FEL_EMPTY.get(path)
    r.require(pair is not None and not witness['affected']
              and witness['compile_only'] == {'kind': 'reviewed-native-fel-domain',
                                            'new_ids': NATIVE_FEL_CURRENT_ADDITIONS}
              and (retention.digest(before), retention.digest(after)) == pair
              and not set(NATIVE_FEL_CURRENT_ADDITIONS) & universe,
              'Unreviewed native FEL empty historical source domain')


DEFAULT_NONE_TARGET_PATH = 'crates/plurxd/src/source_probe/title_work_tests.rs'
DEFAULT_NONE_TARGET_PAIR = ('31941684250ecddad9e46c29d7f4394723a45c896692870b6d0d5fd985ca18ee',
                            'f4953d7e3de3aa4ab6adcf9d16c36e0f3283500afecec5b36e5e17acd1bf4409')

def compile_only_test_additions(path, witness, before, after, universe):
    contract = witness['compile_only']
    if contract.get('kind') == 'default-none-title-target':
        r.require(path == DEFAULT_NONE_TARGET_PATH and contract == {'kind': 'default-none-title-target'}
                  and not witness['affected']
                  and (retention.digest(before), retention.digest(after)) == DEFAULT_NONE_TARGET_PAIR
                  and after.count(b'        fel_head_target: None,\n') == 1
                  and after.replace(b'        fel_head_target: None,\n', b'', 1) == before,
                  'Unreviewed default title target fixture')
        return
    pair = REVIEWED_TEST_ADDITIONS.get(path)
    r.require(pair is not None and not witness['affected']
              and contract == {'kind': 'reviewed-test-additions', 'new_ids': pair[2]}
              and (retention.digest(before), retention.digest(after)) == pair[:2]
              and not set(pair[2]) & universe
              and all(identity.split('::')[-1].encode() not in before for identity in pair[2]),
              'Unreviewed new-only test source or historical body overlap')


RETIRED_FEL_PATH = 'crates/plurxd/src/source_probe/fel_qualification.rs'
RETIRED_FEL_ID = 'plurxd::bin/plurxd source_probe::fel_qualification::tests::a_foreign_backend_recipe_request_stays_pending_without_measurement'
FEL_SUCCESSOR_ID = 'plurxd::bin/plurxd source_probe::fel_qualification::tests::foreign_pending_recipe_does_not_block_current_background_qualification'
RETIRED_FEL_SOURCE = ('d4446cbf718abf6da23cb7d00df89483124e8a51736f0b789afb5e18dc205f28',
                      '02c3c5b4542cff5c5fa3172a9a27f48d47712d6374237dbac473f962b6149129')


def reviewed_retirements(record, outcomes):
    """An old PASS remains historical; its reviewed replacement must execute."""
    retired = {}
    for path, witness in record['changes'].items():
        if 'retirement' not in witness:
            continue
        r.require(path == RETIRED_FEL_PATH
                  and witness['retirement'] == {'retired': RETIRED_FEL_ID, 'successor': FEL_SUCCESSOR_ID}
                  and (witness['before'], witness['after']) == RETIRED_FEL_SOURCE
                  and RETIRED_FEL_ID in witness['affected']
                  and outcomes.get(RETIRED_FEL_ID) == 'PASS'
                  and FEL_SUCCESSOR_ID not in outcomes,
                  'Unreviewed source retirement, failed origin, or existing successor')
        retired[RETIRED_FEL_ID] = FEL_SUCCESSOR_ID
    return retired


def reviewed_changes(record, candidate, universe):
    prior = r.sha(record['prior'])
    changes = record['changes']
    r.require(isinstance(changes, dict) and set(changes) == retention.changed(prior, candidate),
              'Every nextest source change needs a reviewed witness')
    invalidated = set()
    for path, witness in changes.items():
        r.require((path in TEST_INPUTS or path in RUNTIME_INPUTS or path in retention.PROTOCOL_PATHS)
                  and (not path.startswith('.github/') or path in {'.github/workflows/ci.yml', '.github/workflows/main-fast-lane.yml'}),
                  'Changed protected or unreviewed nextest input')
        r.require(isinstance(witness, dict)
                  and set(witness) in ({'before', 'after', 'affected', 'reason'},
                                       {'before', 'after', 'affected', 'reason', 'compile_only'},
                                       {'before', 'after', 'affected', 'reason', 'retirement'})
                  and isinstance(witness['reason'], str) and 0 < len(witness['reason'].strip()) <= 2048,
                  'Missing nextest source applicability reason')
        before, after = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        r.require(witness['before'] == (retention.digest(before) if before is not None else None)
                  and witness['after'] == (retention.digest(after) if after is not None else None),
                  'Nextest applicability bytes differ')
        if path == '.github/workflows/ci.yml':
            from validation.workspace_rust_continuation import metadata_only_workflow
            r.require(before is not None and after is not None and metadata_only_workflow(before, after),
                      'Changed protected workflow execution, beyond exact source-gate metadata')
        if path == '.github/workflows/main-fast-lane.yml':
            r.require(projected_main_workflow(before) == projected_main_workflow(after),
                      'Unreviewed automatic source-gate routing workflow')
        affected = witness['affected']
        r.require(isinstance(affected, list) and len(affected) == len(set(affected))
                  and set(affected) <= universe, 'Unknown nextest affected body')
        r.require(path not in TEST_INPUTS or bool(affected), 'Changed test input has no affected bodies')
        if 'compile_only' in witness:
            if witness['compile_only'].get('kind') == 'ignored-test-only':
                compile_only_ignored(path, witness, before, after, universe, prior=prior, run=record['run'])
            elif witness['compile_only'].get('kind') == 'reviewed-native-fel-domain':
                compile_only_native_fel(path, witness, before, after, universe)
            elif witness['compile_only'].get('kind') in {'reviewed-test-additions', 'default-none-title-target'}:
                compile_only_test_additions(path, witness, before, after, universe)
            else:
                compile_only_none(path, witness, before, after)
        else:
            r.require(path not in RUNTIME_INPUTS or bool(affected) or not path.startswith('crates/'),
                      'Changed Rust runtime input has no affected bodies')
        invalidated.update(affected)
    return invalidated


def authenticate(api, record, binding, candidate, repository, *, current_run=None):
    fields = {'version', 'repository', 'pr', 'branch', 'base_sha', 'source_commit',
              'prior', 'run', 'plan_artifact', 'crate_artifacts', 'shard_artifacts', 'changes', 'compatibility'}
    r.require(set(record) == fields and record['version'] == POLICY
              and record['source_commit'] == candidate and record['compatibility'] == COMPATIBILITY
              and binding.get('schema') == 2
              and record['repository'] == repository and record['pr'] == binding['pull_request']
              and record['branch'] == binding['head_ref'] and record['base_sha'] == binding['base_sha'],
              'Unknown nextest continuation contract')
    prior, run_id = r.sha(record['prior']), r.positive(record['run'])
    from validation import workspace_rust_continuation as workspace
    for revision in (prior, candidate):
        workspace.check_workflow_environment(retention.git_bytes(revision, '.github/workflows/ci.yml').decode(),
                                             workspace.CURRENT_JOB)
    for path, digest in HARNESS.items():
        old, new = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        if path == '.github/workflows/main-fast-lane.yml':
            old, new = projected_main_workflow(old), projected_main_workflow(new)
        r.require(old is not None and retention.digest(old) == digest and old == new,
                  'Unreviewed nextest compiler/workflow/runtime recipe')
    run = api.get(f'/actions/runs/{run_id}')
    r.require(run['repository']['id'] == repository and run['commit_sha'] == prior
              and run['workflow_id'] == 'main-fast-lane.yml' and run['trigger_event'] == 'pull_request'
              and run['prettyref'] == '#' + str(binding['pull_request']),
              'Original nextest PR execution differs')
    current = source_selection(candidate, binding['base_sha'])
    selected = {name for name, row in current['crates'].items() if row['due']}
    r.require(all(not current['crates'][name].get('only') for name in selected),
              'Dynamic-only Rust selection needs its own continuation contract')
    artifacts = api.pages(f'/actions/runs/{run_id}/artifacts')
    by_id = {item['id']: item for item in artifacts}
    r.require(len(by_id) == len(artifacts), 'Duplicate nextest artifact metadata')
    plan_meta = by_id[r.positive(record['plan_artifact'])]
    r.require(plan_meta['name'] == f'rust-units-plan-{run_id}' and plan_meta['run_id'] == run_id,
              'Original nextest plan identity differs')
    plan = artifact(api, plan_meta, 'plan.json')
    r.require(set(plan) == {'schema', 'run', 'head', 'base', 'shards', 'crates', 'absent',
                            'filters', 'serial_filters'}, 'Unknown original nextest plan fields')
    r.require(plan['run'] == run_id and plan['head'] == prior and set(plan['crates']) == selected
              and all(not row['adopted'] and not row.get('only') and not row.get('exact')
                      for row in plan['crates'].values()), 'Incomplete original selected-crate census')
    r.require(isinstance(record['crate_artifacts'], dict)
              and set(record['crate_artifacts']) == selected, 'Missing selected crate receipts')
    r.require(plan['shards'] in range(1, units.MAX_SHARDS + 1)
              and not plan.get('absent'), 'Invalid or absent original plan bodies')
    config = units.Config.load(units.Tree(prior).text(str(units.CONFIG)))
    auth = units.Authenticator(ForgeView(api), current_run if current_run is not None else int(os.environ['GITHUB_RUN_ID']), config)
    jobs = api.pages(f'/actions/runs/{run_id}/jobs')
    r.require(jobs and len({job['id'] for job in jobs}) == len(jobs)
              and all(job['run_id'] == run_id and job['repo_id'] == repository and job['attempt'] == 1
                      for job in jobs), 'Original nextest job census differs')
    auth._jobs[run_id] = jobs
    outcomes, logs, log_rows = {}, {}, {}
    for crate in sorted(selected):
        meta = by_id[r.positive(record['crate_artifacts'][crate])]
        r.require(meta['run_id'] == run_id, 'Foreign nextest receipt run')
        key = auth.key_at(prior, crate)
        r.require(plan['crates'][crate]['key'] == key, 'Original plan crate key differs from source')
        claimed = artifact(api, meta, 'receipt.json')
        units.validate_receipt(claimed, crate, key)
        verified = auth.authenticate(meta, crate, key)
        r.require(verified['unproven'] == 0 and verified['listing'] == plan['crates'][crate]['listing'],
                  'Original receipt lacks authenticated body census')
        for name in claimed['evidence_jobs']:
            job = auth.job(run_id, name, {'success', 'failure'}, 'shard')
            r.require(job['run_id'] == run_id and job['repo_id'] == repository and job['attempt'] == 1,
                      'Original shard execution identity differs')
            if job['id'] not in logs:
                environment_witness(auth.log(job['id']).encode())
                rows = strict_outcomes(auth.log(job['id']).encode())
                r.require(not set(rows) & set(outcomes), 'Body executed in multiple original shards')
                outcomes.update(rows)
                logs[job['id']] = retention.digest(auth.log(job['id']).encode())
                log_rows[job['id']] = rows
        planned = set(plan['crates'][crate]['remaining'] + plan['crates'][crate]['serial'])
        r.require(all(units.crate_of(identity.split(' ', 1)[0]) == crate for identity in planned),
                  'Original plan contains a foreign crate body')
        actual = verified['passed'] | verified['failed']
        r.require(actual == planned and not verified['passed'] & verified['failed']
                  and verified['listing'] == {'count': len(planned), 'sha256': units.digest_lines(sorted(planned))},
                  'Original crate receipt is incomplete')
    r.require(isinstance(record['shard_artifacts'], dict)
              and set(record['shard_artifacts']) == {str(i) for i in range(1, plan['shards'] + 1)},
              'Every original shard JSON artifact is required')
    shards = []
    for index in range(1, plan['shards'] + 1):
        meta = by_id[r.positive(record['shard_artifacts'][str(index)])]
        name = f'rust-units-shard-{run_id}-{index}'
        r.require(meta['run_id'] == run_id and meta['name'] == name, 'Foreign shard JSON artifact')
        shard = artifact(api, meta, 'receipt.json')
        r.require(set(shard) == {'schema', 'producer', 'run', 'index', 'shards', 'head', 'job', 'results'},
                  'Unknown original nextest shard fields')
        job = auth.job(run_id, units.JOB_SHARD.format(index=index), {'success', 'failure'}, 'shard')
        r.require(shard['producer'] == 'validation.rust_units' and shard['job'] == units.JOB_SHARD.format(index=index)
                  and set(shard['results']) == selected
                  and f'Artifact {name} has been successfully uploaded!' in auth.log(job['id']),
                  'Shard JSON publisher identity differs')
        declared = {}
        for crate, row in shard['results'].items():
            r.require(set(row) == {'passed', 'failed', 'skipped'} and not row['skipped'],
                      'Incomplete original shard execution')
            for key, status in (('passed', 'PASS'), ('failed', 'FAIL')):
                for identity in row[key]:
                    r.require(identity not in declared and units.crate_of(identity.split(' ', 1)[0]) == crate,
                              'Duplicate or foreign shard body')
                    declared[identity] = status
        r.require(declared == log_rows[job['id']], 'Shard JSON contradicts original raw statuses')
        shards.append(shard)
    logged_by_index = {shard['index']: log_rows[auth.job(run_id, shard['job'],
                       {'success', 'failure'}, 'shard')['id']] for shard in shards}
    r.require(complete_shard_census(plan, shards, logged_by_index) == outcomes,
              'Original complete shard census differs from status rows')
    archive_job = auth.job(run_id, units.JOB_ARCHIVE, {'success'}, 'rust_units_archive')
    environment_witness(auth.log(archive_job['id']).encode(), archive=True)
    r.require(f"Artifact {plan_meta['name']} has been successfully uploaded!" in auth.log(archive_job['id']),
              'Original archive job did not publish the selected plan')
    universe = {test for row in plan['crates'].values() for test in row['remaining'] + row['serial']}
    planned_list = [test for row in plan['crates'].values() for test in row['remaining'] + row['serial']]
    r.require(len(planned_list) == len(universe), 'Duplicate body in original selected plan')
    r.require(set(outcomes) == universe and len(logs) == plan['shards'],
              'Original shard outcomes do not cover the complete census')
    # A shard receipt is not an outcome source; authenticated original status
    # rows remain authoritative, including all failures and abnormal exits.
    invalidated = reviewed_changes(record, candidate, universe)
    retired = reviewed_retirements(record, outcomes)
    retained = sorted(test for test, value in outcomes.items() if value == 'PASS' and test not in invalidated)
    return {'provider': POLICY, 'scope': 'selected-pr-rust-source-gate', 'candidate': candidate,
            'prior': prior, 'base': binding['base_sha'], 'run': run_id, 'logs': logs,
            'selection_projection': current['projection'], 'selected': sorted(selected), 'universe': sorted(universe), 'retained_ids': retained,
            'must_pass_ids': sorted(universe - set(retained) - set(retired)),
            'retired_ids': retired,
            'mandatory_new_ids': sorted(identity for row in record['changes'].values()
                for identity in row.get('compile_only', {}).get('new_ids', [])),
            'serial_ids': sorted(test for row in plan['crates'].values() for test in row['serial']),
            'outcomes': outcomes, 'root_ownership_passed': False,
            'compile_only_ignored_ids': [IGNORED_COMPILE_ID] if any(
                row.get('compile_only', {}).get('kind') == 'ignored-test-only' for row in record['changes'].values()) else []}



def complete_shard_census(plan, shards, logged):
    outcomes, problems = units.collect(plan, shards)
    r.require(not problems and len(shards) == plan['shards']
              and set(logged) == set(range(1, plan['shards'] + 1)),
              'Original selected shard census is incomplete')
    merged = {}
    for shard in shards:
        declared = {}
        for row in shard['results'].values():
            r.require(not row['skipped'], 'Original shard contains unexecuted bodies')
            for field, status in (('passed', 'PASS'), ('failed', 'FAIL')):
                for identity in row[field]:
                    r.require(identity not in declared, 'Duplicate shard JSON outcome')
                    declared[identity] = status
        r.require(declared == logged[shard['index']] and not set(merged) & set(declared),
                  'Shard JSON/log outcomes disagree or repeat')
        merged.update(declared)
    r.require(set(merged) == set(outcomes), 'Original complete body census differs')
    return merged


def current_census(proof, listing, config):
    """Only original selected crates; every new current body remains due."""
    suites = units.nextest_suites(listing)
    r.require({units.crate_of(binary) for binary in suites} == set(proof['selected']),
              'Current archive differs from the selected crate scope')
    current, serial = set(), set()
    for crate in proof['selected']:
        ordinary, ignored = units.crate_tests(suites, crate, set(config.serial_ignored.get(crate, ())))
        current.update(ordinary + ignored)
        serial.update(ignored)
    retired = proof.get('retired_ids', {})
    r.require(isinstance(retired, dict) and (not retired or retired == {RETIRED_FEL_ID: FEL_SUCCESSOR_ID})
              and set(retired) <= set(proof['universe'])
              and not set(retired) & current
              and set(retired.values()) <= current
              and not set(retired.values()) & (serial | set(proof['retained_ids'])),
              'Reviewed retirement is missing its mandatory ordinary successor')
    r.require(set(proof['universe']) - set(retired) <= current and set(proof['serial_ids']) == serial,
              'Historical body disappeared, became ignored, or changed serial contract')
    for identity in proof.get('compile_only_ignored_ids', []):
        binary, name = identity.split(' ', 1)
        case = listing['rust-suites'].get(binary, {}).get('testcases', {}).get(name, {})
        r.require(identity == IGNORED_COMPILE_ID and case.get('ignored') is True
                  and identity not in current and identity not in serial,
                  'Compile-only physical test entered current selected or serial bodies')
    mandatory = set(proof.get('mandatory_new_ids', []))
    r.require(mandatory <= current and not mandatory & (serial | set(proof['universe']) | set(proof['retained_ids'])),
              'New-only source lacks its mandatory ordinary current bodies')
    retained = set(proof['retained_ids'])
    r.require(retained <= current, 'Retained nextest body disappeared')
    return current, current - retained, serial



def selected_listing_ids(listing):
    identities = set()
    for binary, suite in listing['rust-suites'].items():
        for name, case in suite['testcases'].items():
            match = case.get('filter-match', {})
            r.require(match.get('status') in {'matches', 'mismatch'}, 'Missing actual nextest filter result')
            if match['status'] == 'matches':
                identities.add(binary + ' ' + name)
    return identities


def install_nextest(directory):
    """Same release asset and digest as the authenticated shard recipe."""
    directory.mkdir(parents=True, exist_ok=False)
    url = ('https://github.com/nextest-rs/nextest/releases/download/'
           'cargo-nextest-0.9.151/cargo-nextest-0.9.151-x86_64-unknown-linux-gnu.tar.gz')
    with urllib.request.urlopen(url, timeout=60) as response:
        raw = response.read(64 * 1024 * 1024 + 1)
    r.require(len(raw) <= 64 * 1024 * 1024 and retention.digest(raw) == NEXTEST_SHA256,
              'Pinned nextest executable download differs')
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:gz') as archive:
        members = [member for member in archive.getmembers() if member.name == 'cargo-nextest']
        r.require(len(members) == 1 and members[0].isfile() and members[0].size <= 64 * 1024 * 1024,
                  'Pinned nextest executable member differs')
        stream = archive.extractfile(members[0])
        executable = directory / 'cargo-nextest'
        executable.write_bytes(stream.read())
        executable.chmod(0o700)
    os.environ['PATH'] = str(directory) + os.pathsep + os.environ['PATH']
    return executable



def _pidfd_observe(descriptor, child_pid, flags, code, status, deadline):
    while time.monotonic() < deadline:
        observed = os.waitid(os.P_PIDFD, descriptor, flags | os.WNOWAIT | os.WNOHANG)
        if observed is not None:
            r.require(observed.si_pid == child_pid and observed.si_code == code
                      and observed.si_status == status, 'Owned PIDfd probe observed another lifecycle')
            return
        time.sleep(0.01)
    raise r.ReceiptError('Owned PIDfd capability probe exceeded its deadline; no bodies executed')


def require_pidfd_environment():
    """Probe the actual owned-child identity/stop/continue/exit primitives.

    Signals target only the owned probe's PIDfd. Popen cleanup retains ownership
    of an unreaped child; it is not a production process-identity fallback.
    """
    print('Nextest kernel:', os.uname().release, flush=True)
    r.require(all(hasattr(os, name) for name in ('pidfd_open', 'waitid', 'P_PIDFD', 'WNOWAIT',
              'WNOHANG', 'WSTOPPED', 'WCONTINUED', 'WEXITED', 'CLD_STOPPED', 'CLD_CONTINUED', 'CLD_KILLED'))
              and hasattr(signal, 'pidfd_send_signal'), 'Required Python PIDfd primitives are unavailable')
    child, descriptor = None, None
    deadline = time.monotonic() + 5.0
    try:
        self_descriptor = os.pidfd_open(os.getpid())
        os.close(self_descriptor)
        child = subprocess.Popen([sys.executable, '-I', '-S', '-c', 'import time; time.sleep(30)'],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        descriptor = os.pidfd_open(child.pid)
        signal.pidfd_send_signal(descriptor, signal.SIGSTOP)
        _pidfd_observe(descriptor, child.pid, os.WSTOPPED, os.CLD_STOPPED, signal.SIGSTOP, deadline)
        signal.pidfd_send_signal(descriptor, signal.SIGCONT)
        _pidfd_observe(descriptor, child.pid, os.WCONTINUED, os.CLD_CONTINUED, signal.SIGCONT, deadline)
        signal.pidfd_send_signal(descriptor, signal.SIGTERM)
        _pidfd_observe(descriptor, child.pid, os.WEXITED, os.CLD_KILLED, signal.SIGTERM, deadline)
        child.wait(timeout=max(0.01, deadline - time.monotonic()))
    except (OSError, subprocess.SubprocessError) as error:
        raise r.ReceiptError('Owned PIDfd capability is unavailable (' + type(error).__name__
                             + ', errno ' + str(getattr(error, 'errno', None)) + '); no bodies executed') from None
    finally:
        try:
            if child is not None:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5.0)
        finally:
            if descriptor is not None:
                os.close(descriptor)
    print('Nextest PIDfd: self/child open, signal STOP/CONT/TERM, waitid stop/continue/exit passed; child reaped', flush=True)


def execute(proof):
    """Compile selected tests and run only uncovered original/new IDs.

    This source gate preserves the original nextest process isolation and two
    concurrent bodies, including serial ignored checks. It does not execute or
    qualify unselected crates or doctests; those are outside the PR unit scope.
    """
    r.require(proof['scope'] == 'selected-pr-rust-source-gate'
              and os.geteuid() == 0 and os.uname().sysname == 'Linux'
              and os.uname().machine == 'x86_64'
              and os.environ.get('RUST_MIN_STACK') == '8388608'
              and os.environ.get('RUST_TEST_THREADS') == '2'
              and os.environ.get('CARGO_BUILD_JOBS') == '1'
              and not os.environ.get('CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER')
              and subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == proof['candidate'],
              'Nextest source-gate runtime or candidate differs')
    require_pidfd_environment()
    from validation.qualification_unit_continuation import require_unmodified_cargo_environment
    require_unmodified_cargo_environment(os.environ)
    r.require(not any(os.environ.get(key) for key in ('RUSTDOCFLAGS', 'CARGO_ENCODED_RUSTDOCFLAGS',
              'RUSTDOC', 'CARGO_BUILD_RUSTDOC', 'CARGO_BUILD_TARGET_DIR')),
              'Unexpected nextest compiler environment')
    r.require(not subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no']),
              'Nextest checkout contains changed tracked sources')
    ffmpeg = os.environ.get('PLURX_FFMPEG', 'ffmpeg')
    r.require(subprocess.check_output([ffmpeg, '-version'], text=True).splitlines()[0].startswith(
              'ffmpeg version ' + FFMPEG_VERSION + ' '), 'Current FFmpeg runtime witness differs')
    compiler = subprocess.check_output(['rustc', '-vV'], text=True).splitlines()
    r.require(all(line in compiler for line in ('release: 1.97.1',
              'commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452',
              'host: x86_64-unknown-linux-gnu')), 'Nextest compiler identity differs')
    from validation import workspace_rust_continuation as workspace
    workspace.check_workflow_environment(retention.git_bytes(proof['candidate'], '.github/workflows/ci.yml').decode(),
                                         workspace.CURRENT_JOB)
    selection = source_selection(proof['candidate'], proof['base'])
    r.require({crate for crate, row in selection['crates'].items() if row['due']} == set(proof['selected']),
              'Current required Rust selection changed')
    output = Path('target/validation/nextest-continuation')
    output.mkdir(parents=True, exist_ok=False)
    install_nextest(Path(os.environ['RUNNER_TEMP']) / ('nextest-continuation-' + proof['candidate']))
    subprocess.run(['make', 'fmt-check', 'spike-lock-check', 'lint', 'test-socket-permission-check'], check=True)
    archive = output / 'archive.tar.zst'
    crate_filter = ' | '.join('package(=' + crate + ')' for crate in proof['selected'])
    subprocess.run(['cargo', 'nextest', 'archive', '--locked', '--workspace', '--exclude',
                    'plurx-cluster-check', '-E', crate_filter, '--archive-file', str(archive)], check=True)
    listing_raw = subprocess.check_output(['cargo-nextest', 'nextest', 'list', '--archive-file', str(archive),
                    '--workspace-remap', '.', '--profile', 'ci', '--run-ignored', 'all', '--message-format', 'json'])
    (output / 'listing.json').write_bytes(listing_raw)
    listing = json.loads(listing_raw)
    config = units.Config.load(units.Tree(proof['candidate']).text(str(units.CONFIG)))
    current, due, serial = current_census(proof, listing, config)
    units.require_test_disk()
    units.bound_cpus()
    units.EXTRACT = output / 'extract'
    observed = {}
    for identities, extra in ((due - serial, ['--test-threads', '2']),
                              (due & serial, ['--run-ignored', 'only', '--test-threads', '1'])):
        if not identities:
            continue
        filters = units.filter_expressions(sorted(identities), {})
        list_command = ['cargo-nextest', 'nextest', 'list', '--archive-file', str(archive),
                        '--workspace-remap', '.', '--profile', 'ci', '--message-format', 'json']
        if '--run-ignored' in extra:
            list_command += ['--run-ignored', 'only']
        for expression in filters:
            list_command += ['-E', expression]
        planned = json.loads(subprocess.check_output(list_command))
        r.require(selected_listing_ids(planned) == identities,
                  'Actual nextest filter selects applicable passes or omits due bodies')
        command = units.nextest_command(archive, filters, extra)
        process = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        # Preserve the actual original status log; this is never a libtest/JUnit surrogate.
        raw = process.stdout
        print(raw.decode('utf-8', 'strict'), end='', flush=True)
        with (output / 'execution.log').open('ab') as stream:
            stream.write(raw)
        rows = strict_outcomes(raw)
        r.require(process.returncode == 0 and set(rows) == identities
                  and set(rows.values()) == {'PASS'} and not set(rows) & set(observed),
                  'Current nextest execution is failed, incomplete, duplicated, or outside the due set')
        observed.update(rows)
    r.require(set(observed) == due, 'Current nextest due census is incomplete')
    r.atomic_json(output / 'receipt.json', {'version': POLICY, 'kind': 'selected-pr-rust-source-gate',
                  'candidate': proof['candidate'], 'base': proof['base'], 'source_run': proof['run'],
                  'enrollment_digest': proof['digest'], 'selection_projection': proof['selection_projection'], 'selected': proof['selected'],
                  'retained': proof['retained_ids'], 'executed': sorted(observed),
                  'historical_retired_passes': proof.get('retired_ids', {}),
                  'current_listing_sha256': retention.digest(listing_raw), 'current_universe': sorted(current),
                  'complete': True})
