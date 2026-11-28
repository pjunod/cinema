import unittest
from concurrent.futures import Future
from unittest.mock import Mock

import support
from plurx_player.playback import Playback, split_audio_names


def master():
    first, second, video = "a" * 64, "b" * 64, "c" * 64
    return "\n".join((
        "#EXTM3U",
        f'#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="English, Original",DEFAULT=YES,STABLE-RENDITION-ID="a2",URI="audio/{first}/index.m3u8"',
        f'#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="French",DEFAULT=NO,STABLE-RENDITION-ID="a9",URI="audio/{second}/index.m3u8"',
        '#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS="avc1.4d401f,mp4a.40.2",AUDIO="audio"',
        f"video/{video}/index.m3u8",
    ))


class SplitAudioTests(unittest.TestCase):
    def test_uses_only_distinct_renditions_referenced_by_the_variant(self):
        self.assertEqual(split_audio_names(master()), {"English, Original": 2, "French": 9})
        self.assertEqual(split_audio_names(master().replace('AUDIO="audio"', 'AUDIO="other"')), {})
        self.assertEqual(split_audio_names(master().replace('NAME="French"', 'NAME="English, Original"')), {})

    def test_only_the_owning_attachment_can_confirm_and_retain_a_selection(self):
        mpv = Mock()
        properties = {"path": "http://server/api/v1/hls/session/master.m3u8",
                      "track-list": [{"type": "audio", "title": "English, Original", "selected": True},
                                     {"type": "audio", "title": "French", "selected": False}]}
        mpv.get.side_effect = properties.get
        playback = Playback(Mock(), mpv, {"file_id": 3}, {}, Mock(), Mock())
        playback.session = {"session_id": "session"}
        probe = Future()
        probe.set_result(split_audio_names(master()))
        playback.split_audio_probe = (properties["path"], "session", probe)
        playback.observe_split_audio()
        self.assertEqual(playback.observed_split_audio, 2)
        self.assertIsNone(playback.requested_audio)
        properties["track-list"][0]["selected"] = False
        properties["track-list"][1]["selected"] = True
        playback.observe_split_audio()
        self.assertEqual(playback.requested_audio, 9)
        mpv.set.assert_called_with("user-data/cinema-audio-rendition", "a9")
        properties["path"] = "http://server/api/v1/hls/different/master.m3u8"
        properties["track-list"][0]["selected"] = True
        properties["track-list"][1]["selected"] = False
        playback.observe_split_audio()
        self.assertEqual(playback.requested_audio, 9)
