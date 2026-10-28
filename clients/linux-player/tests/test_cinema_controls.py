"""Library context and viewer intents through the real launcher and private IPC."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import support
from support import FILE_ID, ITEM_ID, TOKEN, LauncherCase
from plurx_player.cinema import Preferences, context, display_title, quality_choices
from plurx_player.playback import Playback
from plurx_player.mpv import MpvError
from plurx_player.api import Http, ApiError


class CinemaModel(unittest.TestCase):
    def test_authoritative_empty_candidates_never_invent_encode_rungs(self):
        self.assertEqual([c['id'] for c in quality_choices({'quality_candidates': [], 'ladder': [{'height': 1080}]})], ['auto', 'original'])
        choices = quality_choices({'quality_candidates': [
            {'route': 'encode', 'decoder_compatible': True, 'target_height': 1080},
            {'route': 'encode', 'decoder_compatible': False, 'target_height': 2160},
            {'route': 'copy', 'decoder_compatible': True, 'target_height': 720}]})
        self.assertEqual([c['id'] for c in choices], ['auto', 'original', '1080'])

    def test_context_is_file_bound_and_contains_only_presentation_data(self):
        value = {'file_id': FILE_ID, 'item_id': ITEM_ID, 'metadata': {'title': 'Movie', 'token': TOKEN}, 'secret': TOKEN}
        self.assertNotIn(TOKEN, json.dumps(context(value, FILE_ID)))
        with self.assertRaises(ValueError):
            context(value, FILE_ID + 1)
        self.assertEqual(display_title({'title': 'Arrival', 'series_title': 'Example', 'season_number': 2, 'episode_number': 3}), 'Example · S02E03 · Arrival')

    def test_playback_info_publishes_only_whitelisted_delivery_facts(self):
        mpv = mock.Mock()
        player = Playback(mock.Mock(), mpv, {'file_id': FILE_ID}, {}, lambda: 0, lambda _: None)
        player.decision = {'source': {'filename': '/private/movie.mkv'}, 'token': TOKEN}
        for mode, session, expected in (
                ('direct', None, ('progressive', 'none')),
                ('remux', {'session_id': TOKEN, 'playlist_url': 'https://media1/secret'}, ('hls', 'live')),
                ('transcode', {'vod': True, 'session_id': TOKEN}, ('hls', 'vod'))):
            player.mode, player.session = mode, session
            player.publish_chrome()
            value = mpv.set.call_args.args[1]
            self.assertEqual(value['playback'], {'mode': mode, 'transport': expected[0],
                                               'session': expected[1]})
            for secret in (TOKEN, '/private/', 'https://'):
                self.assertNotIn(secret, json.dumps(value))
        player.mode, player.session = TOKEN, None
        player.publish_chrome()
        self.assertEqual(mpv.set.call_args.args[1]['playback'],
                         {'mode': None, 'transport': None, 'session': None})

    def test_queued_incumbent_restart_cannot_commit_unloaded_quality(self):
        player = Playback(mock.Mock(), mock.Mock(), {'file_id': FILE_ID}, {}, lambda: 0, lambda _: None)
        player.started, player.loading = True, True
        player.pending_quality = {'loaded': False}
        player.settle_quality = mock.Mock()
        player.progress = mock.Mock()
        player.on_event({'event': 'playback-restart'})
        player.settle_quality.assert_not_called()
        self.assertTrue(player.loading)
        player.on_event({'event': 'file-loaded'})
        player.on_event({'event': 'playback-restart'})
        player.settle_quality.assert_called_once_with()

    def test_window_close_after_admission_still_owns_both_sessions_for_cleanup(self):
        mpv = mock.Mock()
        mpv.get.return_value = False
        player = Playback(mock.Mock(), mpv, {'file_id': FILE_ID}, {}, lambda: 0, lambda _: None)
        player.started = True
        player.mode = 'transcode'
        player.session = {'session_id': 'incumbent'}
        player.decision = {'quality_candidates': [{'route': 'encode', 'decoder_compatible': True, 'target_height': 1080}]}
        candidate = {'session_id': 'candidate'}
        for name in ('publish_chrome', 'progress', 'decide', 'plan_subtitle', '_final_progress', '_final_mapper', '_report_failure'):
            setattr(player, name, mock.Mock())
        player.create_session = mock.Mock(return_value=candidate)
        player.position_ms = mock.Mock(return_value=65000)
        player.delete_session = mock.Mock()
        mpv.set.side_effect = MpvError('window closed')
        with self.assertRaises(MpvError):
            player.change_quality('1080')
        player.finish()
        self.assertEqual(player.delete_session.call_args_list,
                         [mock.call({'session_id': 'incumbent'}), mock.call(candidate)])

    def test_autoplay_saves_atomically_and_failed_save_preserves_effective_choice(self):
        with tempfile.TemporaryDirectory() as root:
            prefs = Preferences(root)
            prefs.set_autoplay(False)
            self.assertFalse(Preferences(root).autoplay_next)
            with mock.patch('plurx_player.cinema.os.replace', side_effect=OSError('read only')):
                with self.assertRaises(OSError):
                    prefs.set_autoplay(True)
            self.assertFalse(prefs.autoplay_next)
            self.assertEqual([p.name for p in Path(root).iterdir()], ['playback.json'])


class CinemaControls(LauncherCase):
    def intent_at(self, when, *args):
        self.clock.at(when, lambda: self.mpv.emit({'event': 'client-message', 'args': list(args)}))

    def chrome(self):
        return [c[2] for c in self.mpv.commands() if c[:2] == ['set_property', 'user-data/cinema']]

    def advertise_quality(self):
        self.server.decision['quality_candidates'] = [
            {'route': 'encode', 'decoder_compatible': True, 'target_height': 1080}]

    def advertise_next(self):
        nxt = {'file_id': FILE_ID+1, 'item_id': ITEM_ID+1,
               'metadata': {'title': 'Second episode', 'series_title': 'Example', 'kind': 'episode',
                            'season_number': 1, 'episode_number': 2}, 'next': None}
        self.server.next_context = nxt
        self.server.context_data['next'] = {k: nxt[k] for k in ('file_id', 'item_id', 'metadata')}

    def test_library_metadata_reaches_overlay_without_credentials(self):
        self.close_at(4); self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(self.chrome()[-1]['metadata']['overview'], 'A library synopsis.')
        self.assertNotIn(TOKEN, json.dumps(self.chrome()))
        self.assertIn(['set_property', 'force-media-title', 'Fixture'], self.mpv.commands())

    def test_quality_preserves_absolute_position_pause_and_heartbeat_selection(self):
        self.advertise_quality(); self.server.origins = [60_000, 65_000]
        self.clock.at(5, lambda: self.mpv.props(pause=True))
        self.intent_at(6, 'cinema-quality', '1080'); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        creates = self.server.calls('POST', pattern=r'/hls/sessions$')
        self.assertEqual(len(creates), 2)
        self.assertEqual(creates[1]['body']['start'], 65)
        self.assertEqual(creates[1]['body']['height'], 1080)
        self.assertFalse(creates[1]['body']['quality_auto'])
        pauses = [c for c in self.mpv.commands() if c[:2] == ['set_property', 'pause']]
        self.assertEqual(pauses[-1], ['set_property', 'pause', True])
        controls = self.server.calls('POST', pattern=r'/control$')
        self.assertEqual(controls[-1]['body']['selection']['quality'], {'mode': 'manual', 'height': 1080})
        decisions = self.server.calls('POST', pattern=r'/decision$')
        self.assertEqual(decisions[-1]['query']['force'], ['transcode'])

    def test_refused_or_unadvertised_quality_keeps_existing_media(self):
        self.advertise_quality(); self.server.create_status = [200, 503]
        self.intent_at(4, 'cinema-quality', '4320')
        self.intent_at(6, 'cinema-quality', '1080'); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        loads = [c for c in self.mpv.commands() if c[0] == 'loadfile']
        self.assertEqual(len(loads), 1)
        self.assertEqual(self.chrome()[-1]['quality']['selected'], 'auto')
        self.assertTrue(self.chrome()[-1]['error'])

    def test_explicit_next_finishes_predecessor_then_opens_resolved_successor(self):
        self.advertise_next(); self.intent_at(6, 'cinema-next'); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        nexts = self.server.calls('POST', pattern=r'/self/next$')
        self.assertEqual(len(nexts), 1)
        self.assertEqual(nexts[0]['body'], {'expected_file_id': FILE_ID})
        decisions = self.server.calls('POST', pattern=r'/decision$')
        self.assertTrue(decisions[-1]['path'].endswith(f'/files/{FILE_ID+1}/decision'))
        self.assertEqual(self.chrome()[-1]['metadata']['title'], 'Second episode')
        self.assertEqual(len(self.server.calls('DELETE', pattern=r'/hls/[^/]+$')), 2)
        self.assertTrue(self.server.revoked)
        advance_at = self.server.index('POST', r'/self/next$')
        old_progress = self.server.index('POST', rf'/items/{ITEM_ID}/progress$')
        self.assertLess(old_progress, advance_at)

    def test_autoplay_advances_once_at_eof_and_saved_off_stays_on_predecessor(self):
        self.advertise_next()
        self.clock.at(5, lambda: self.mpv.props(time_pos=7130))
        eof = {'event': 'property-change', 'name': 'eof-reached', 'data': True}
        self.clock.at(6, lambda: self.mpv.emit(eof)); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(len(self.server.calls('POST', pattern=r'/self/next$')), 1)

    def test_autoplay_off_is_saved_and_eof_does_not_advance(self):
        self.advertise_next(); self.intent_at(4, 'cinema-autoplay', 'no')
        self.clock.at(5, lambda: self.mpv.props(time_pos=7130))
        self.clock.at(6, lambda: self.mpv.emit({'event': 'property-change', 'name': 'eof-reached', 'data': True}))
        self.close_at(12); self.assertEqual(self.run_launcher(), 0)
        self.assertFalse(self.server.calls('POST', pattern=r'/self/next$'))
        self.assertFalse(Preferences(self.config).autoplay_next)

    def test_early_live_eof_neither_marks_complete_nor_skips_episode(self):
        self.advertise_next()
        self.clock.at(6, lambda: self.mpv.emit({'event': 'property-change', 'name': 'eof-reached', 'data': True}))
        self.close_at(12); self.assertEqual(self.run_launcher(), 0)
        self.assertFalse(self.server.calls('POST', pattern=r'/self/next$'))
        progress = self.server.calls('POST', pattern=r'/progress$')
        self.assertTrue(all(r['body']['position_ms'] < 7_185_000 for r in progress))
        self.assertTrue(self.chrome()[-1]['retryable'])

    def test_new_quality_attachment_error_restores_incumbent_and_releases_candidate(self):
        self.advertise_quality()
        self.clock.at(5, lambda: self.mpv.props(load_error_on=2))
        self.intent_at(6, 'cinema-quality', '1080'); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        creates = self.server.calls('POST', pattern=r'/hls/sessions$')
        self.assertNotEqual(creates[0]['body']['playback_id'], creates[1]['body']['playback_id'])
        self.assertEqual(len([c for c in self.mpv.commands() if c[0] == 'loadfile']), 3)
        self.assertEqual(self.chrome()[-1]['quality']['selected'], 'auto')
        self.assertIn('restored', self.chrome()[-1]['error'])

    def test_next_accepts_another_copy_of_the_resolved_successor(self):
        self.advertise_next(); self.server.next_context['file_id'] = FILE_ID+2
        self.intent_at(6, 'cinema-next'); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        decisions = self.server.calls('POST', pattern=r'/decision$')
        self.assertTrue(decisions[-1]['path'].endswith(f'/files/{FILE_ID+2}/decision'))

    def test_current_stream_error_is_retryable_in_the_kept_open_window(self):
        self.clock.at(6, lambda: self.mpv.emit({'event': 'end-file', 'reason': 'error', 'file_error': 'read failed'}))
        self.close_at(12); self.assertEqual(self.run_launcher(), 0)
        self.assertTrue(self.chrome()[-1]['retryable'])
        self.assertIn('read failed', self.chrome()[-1]['error'])

    def test_next_request_lost_before_advance_retains_current_playback(self):
        self.advertise_next(); self.intent_at(6, 'cinema-next'); self.close_at(12)
        request = Http.request
        def fail_next(client, method, path, *args, **kwargs):
            if method == 'POST' and path.endswith('/self/next'):
                raise ApiError('connection failed before admission')
            return request(client, method, path, *args, **kwargs)
        with mock.patch.object(Http, 'request', fail_next):
            self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(len([c for c in self.mpv.commands() if c[0] == 'loadfile']), 1)
        self.assertIn('still playing', self.chrome()[-1]['error'])

    def test_replay_after_true_eof_keeps_the_new_resume_position_on_exit(self):
        self.intent_at(4, 'cinema-autoplay', 'no')
        self.clock.at(5, lambda: self.mpv.props(time_pos=7130))
        self.clock.at(6, lambda: self.mpv.emit({'event': 'property-change', 'name': 'eof-reached', 'data': True}))
        def replay():
            self.mpv.props(time_pos=30)
            self.mpv.emit({'event': 'seek'})
            self.mpv.emit({'event': 'playback-restart'})
        self.clock.at(8, replay); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        progress = self.server.calls('POST', pattern=r'/progress$')
        self.assertEqual(progress[-1]['body']['position_ms'], 90_000)

    def test_next_refusal_keeps_predecessor_and_shows_error(self):
        self.advertise_next(); self.server.next_status = 503
        self.intent_at(6, 'cinema-next'); self.close_at(12)
        self.assertEqual(self.run_launcher(), 0)
        self.assertEqual(len([c for c in self.mpv.commands() if c[0] == 'loadfile']), 1)
        self.assertIn('next refused', self.chrome()[-1]['error'])


if __name__ == '__main__':
    unittest.main()
