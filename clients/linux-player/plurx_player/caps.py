"""The capabilities document plurx Player sends (caps v2).

Field names are crates/plurx-core/src/playback/caps.rs `DeviceCaps`; the
shape follows the Apple template (clients/apple/Sources/Caps.swift) except
where docs/clients/LINUX-PLAYER-PLAN.md section 3.2 says otherwise:

* HEVC claims Dolby Vision profiles 5 and 8 only. mpv/libplacebo rebuild P5
  and P8 from the RPU; Profile 7 is left to the server's P7 -> P8.1 routes.
* ``display`` carries no ``max_nits``: on the preserved route the player maps
  locally at the compositor's target, so the server never display-targets
  this client, and a guessed peak would be an over-claim.
* audio is every codec mpv decodes, never passthrough, at most 8 channels:
  mpv decodes and downmixes to the real sink.
* no subtitle overlays in v1; text subtitles load as sidecar tracks.
"""

PRESENT = ["sdr", "pq", "hlg"]
AUDIO_CODECS = ["aac", "ac3", "eac3", "alac", "mp3", "opus", "flac", "vorbis", "truehd", "dts"]
# Rates mpv's decoder and resampler reproduce on any PipeWire sink.
SAMPLE_RATES_HZ = [32000, 44100, 48000, 88200, 96000, 176400, 192000]
CONTAINERS = ["mkv", "mp4", "webm", "mov", "m4v", "ts", "m4a", "m4b", "mp3", "aac",
              "flac", "ogg", "opus", "wav"]


def device_caps(build):
    return {
        "v": 2,
        "client": {"kind": "linux-player", "build": build[:48],
                   "ua": f"plurx-player/{build} (Linux; mpv)"[:160]},
        "video": [
            {"codec": "hevc", "profiles": ["main", "main10"], "max_height": 2160,
             "present": list(PRESENT), "dv_profiles": [5, 8]},
            {"codec": "h264", "profiles": [], "present": list(PRESENT)},
            {"codec": "av1", "profiles": ["main"], "present": list(PRESENT)},
        ],
        "audio": list(AUDIO_CODECS),
        "audio_sinks": [
            {"codec": codec, "max_channels": 8, "passthrough": False,
             "sample_rates_hz": list(SAMPLE_RATES_HZ)}
            for codec in AUDIO_CODECS
        ],
        "containers": list(CONTAINERS),
        "transports": ["progressive", "hls"],
        "progressive_hevc_sample_entries": ["hvc1"],
        "hls_hevc_sample_entries": ["hvc1", "hev1"],
        "subtitle_overlays": [],
        "dv_transport": "hls",
        "display": {"hdr": True, "dolby_vision": True},
    }
