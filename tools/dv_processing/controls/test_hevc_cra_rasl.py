"""Manual decoded-picture regression for a held multi-slice CRA/RASL source.

Supply an unpatched multithreaded helper, an unpatched single-worker reference,
and the patched multithreaded helper built from otherwise identical source.
No source media or private paths are distributed with this regression.
"""
import argparse
from fractions import Fraction
import json
import os
from pathlib import Path
import subprocess


def test_cra_rasl_windows_keep_exact_native_pictures(source, broken, single, patched,
                                                     scratch, rate=Fraction(24000, 1001),
                                                     native=(3840, 2160)):
    scratch = Path(scratch)
    scratch.mkdir()
    env = os.environ | {'PLURX_DV_FRAME_HASHES': '1', 'LP_NUM_THREADS': '1'}
    span = Fraction(48, 1) / rate
    results = []

    def run(helper, name, entry, cap=64):
        directory = scratch / f'{name}-{entry}-{cap}'
        directory.mkdir()
        start, end = entry * span, (entry + 1) * span
        rational = lambda value: f'{value.numerator}/{value.denominator}'
        args = [str(helper), str(source), str(directory), '0', str(cap),
                str(native[0]), str(native[1]), '0', '0', '0',
                'bt2020-pq-master-clip', '/dev/null', rational(start), rational(end),
                '512', 'fel', '1280', '720', 'pq-bilinear-rgb48-v1']
        process = subprocess.run(args, env=env, capture_output=True, timeout=30)
        (directory / 'stdout').write_bytes(process.stdout)
        (directory / 'stderr').write_bytes(process.stderr)
        return process

    refused = run(broken, 'unpatched-two', 1)
    assert refused.returncode > 0 and b'PPS changed between slices' in refused.stderr
    for entry in [0, 1, 2, 360]:
        reference = run(single, 'single', entry)
        actual = run(patched, 'patched-multi', entry)
        assert reference.returncode == actual.returncode == 0, (entry, reference.stderr, actual.stderr)

        def pictures(process):
            events = [json.loads(line) for line in process.stdout.splitlines()]
            pairs = [row for row in events if row['kind'] == 'accepted_source_pair']
            rendered = [row for row in events if row['kind'] == 'rendered_frame']
            completed = [row for row in events if row['kind'] == 'window_complete']
            assert len(pairs) == len(rendered) == 48
            assert len(completed) == 1 and completed[0]['frames'] == 48
            # This includes all leading pictures at a later CRA during the
            # uninterrupted first window, and the requested pictures after seek.
            return [tuple(pair[key] for key in ['frame', 'pts', 'duration',
                                                'bl_sha256', 'el_sha256', 'rpu_sha256'])
                    + (image['rgb_sha256'],)
                    for pair, image in zip(pairs, rendered)]

        assert pictures(reference) == pictures(actual), ('native BL/EL/RPU/RGB changed', entry)
        results.append({'entry': entry, 'frames': 48, 'same_native_pictures': True})
    # A missing requested picture still refuses instead of padding coverage.
    incomplete = run(patched, 'missing-picture-budget', 1, 47)
    assert incomplete.returncode > 0 and b'coded window frame cap exhausted' in incomplete.stderr
    (scratch / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    return results


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source', 'broken', 'single', 'patched', 'scratch']:
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    print(json.dumps(test_cra_rasl_windows_keep_exact_native_pictures(**vars(args))))
