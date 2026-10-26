"""Portable layout capture pins host observations, not readiness contracts."""
import copy
from pathlib import Path
import runpy
import unittest
from unittest.mock import Mock

ROOT = Path(__file__).resolve().parents[2]


class UiReadinessFixtureCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.harness = runpy.run_path(str(ROOT / 'scripts/ui-baseline'), run_name='ui_readiness_fixture')

    def response(self, status='met', evidence='Native host observation'):
        # These are the route's actual runtime families; delivery qualification
        # is separate evidence and must never acquire a fixture verdict.
        families = {
            'macos_video_processing': ['effective_encoder', 'sdr_scale', 'hdr10_metal',
                'hlg_metal', 'subtitle_burns', 'deinterlace', 'dolby_vision', 'live_upload'],
            'macos_hevc_output': ['hevc_effective_encoder', 'hevc_sdr', 'hevc_hdr10'],
            'subtitle_cluster_sources': ['local_cache', 'free_space'],
            'subtitle_stored_sources': ['stored_source_local_cache', 'stored_source_free_space'],
            'chapter_thumbnails': ['chapter_thumbs_cache_space'],
        }
        items = [{'id': name, 'title': name, 'enabled': False, 'setting': name + '_enabled',
                  'requirements': [{'id': row, 'title': row, 'status': status, 'evidence': evidence}
                                   for row in rows]} for name, rows in families.items()]
        for item, row in [(items[0], 'delivery_qualification'), (items[1], 'hevc_delivery_qualification')]:
            item['requirements'].append({'id': row, 'title': 'Physical qualification',
                'status': 'unobservable', 'evidence': 'Physical display evidence remains required.'})
        return {'observed_at_ms': 1750000000000, 'items': items}

    def normalize(self, value):
        return self.harness['fixture_runtime_readiness'](value)

    def test_host_variants_produce_identical_fixture_input_without_mutating_observations(self):
        mac = self.response('met', 'Apple VideoToolbox; filesystem observation unavailable')
        linux = self.response('unmet', 'Software encoder; Linux statfs capacity observed')
        mac_before, linux_before = copy.deepcopy(mac), copy.deepcopy(linux)
        self.assertEqual(self.normalize(mac), self.normalize(linux))
        self.assertEqual(mac, mac_before)
        self.assertEqual(linux, linux_before)
        for item in self.normalize(mac)['items']:
            for row in item['requirements']:
                if 'qualification' not in row['id']:
                    self.assertEqual(row['status'], 'unknown')
                    self.assertIn('UI layout fixture', row['evidence'])

    def test_physical_qualification_choices_order_and_new_rows_remain_observable(self):
        value = self.response()
        value['future_schema'] = {'keep': True}
        value['items'][0]['enabled'] = True
        value['items'][0]['requirements'][-1].update(status='unmet', evidence='Physical receipt missing')
        new_requirement = {'id': 'future_graph', 'title': 'New runtime observation',
                           'status': 'unmet', 'evidence': 'New fact must remain visible', 'future_field': 3}
        value['items'][0]['requirements'].insert(1, new_requirement)
        new_card = {'id': 'future_card', 'title': 'New card', 'requirements': [
            {'id': 'physical', 'title': 'Device receipt', 'status': 'unmet', 'evidence': 'Not qualified'}]}
        value['items'].insert(1, new_card)
        result = self.normalize(value)
        self.assertEqual(result['observed_at_ms'], value['observed_at_ms'])
        self.assertEqual(result['future_schema'], value['future_schema'])
        self.assertEqual(result['items'][0]['enabled'], True)
        self.assertEqual(result['items'][0]['setting'], value['items'][0]['setting'])
        self.assertEqual(result['items'][0]['requirements'][-1], value['items'][0]['requirements'][-1])
        self.assertEqual(result['items'][0]['requirements'][1], new_requirement)
        self.assertEqual(result['items'][1], new_card)
        self.assertEqual([i['id'] for i in result['items']], [i['id'] for i in value['items']])

    def test_bad_schema_and_missing_known_observations_fail_before_overrides(self):
        variants = []
        value = self.response(); value['observed_at_ms'] = True; variants.append(value)
        value = self.response(); value['items'] = {}; variants.append(value)
        value = self.response(); value['items'][0]['enabled'] = 'false'; variants.append(value)
        value = self.response(); value['items'][0]['requirements'][0]['status'] = 'new-status'; variants.append(value)
        value = self.response(); value['items'][0]['requirements'][0]['evidence'] = None; variants.append(value)
        value = self.response(); value['items'][0]['requirements'].pop(0); variants.append(value)
        value = self.response(); value['items'].append(copy.deepcopy(value['items'][0])); variants.append(value)
        value = self.response(); value['items'][0]['requirements'].append(copy.deepcopy(value['items'][0]['requirements'][0])); variants.append(value)
        for value in variants:
            with self.subTest(value=value), self.assertRaises(self.harness['Fail']):
                self.normalize(value)

    def test_http_and_json_failures_are_preserved_and_mark_capture_failed(self):
        for status, document in [(503, self.response()), (200, {'items': []})]:
            response = Mock(status=status)
            response.json.return_value = document
            intercepted = Mock()
            intercepted.fetch.return_value = response
            problems = []
            self.harness['capture_runtime_readiness'](intercepted, problems)
            self.assertTrue(problems)
            intercepted.fulfill.assert_called_once_with(response=response)
        response = Mock(status=200)
        response.json.side_effect = ValueError('invalid JSON')
        intercepted = Mock(); intercepted.fetch.return_value = response
        problems = []
        self.harness['capture_runtime_readiness'](intercepted, problems)
        self.assertEqual(problems, ['invalid JSON'])
        intercepted.fulfill.assert_called_once_with(response=response)


if __name__ == '__main__':
    unittest.main()
