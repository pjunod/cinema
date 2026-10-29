"""Cinema's bounded presentation data and local playback preferences.

Only display metadata reaches mpv's user-data property. Authentication stays
in Http; the overlay sends intents and never receives tokens or server URLs.
"""
import json
import os
from pathlib import Path
import tempfile


def quality_choices(decision):
    result = [{"id": "auto", "label": "Auto"}, {"id": "original", "label": "Original"}]
    candidates = decision.get("quality_candidates")
    if isinstance(candidates, list):
        heights = {c.get("target_height") for c in candidates if isinstance(c, dict)
                   and c.get("route") == "encode" and c.get("decoder_compatible") is True
                   and type(c.get("target_height")) is int and 0 < c["target_height"] <= 2160}
    else:
        heights = {c.get("height") for c in decision.get("ladder", []) if isinstance(c, dict)
                   and type(c.get("height")) is int and 0 < c["height"] <= 2160}
    return result + [{"id": str(h), "label": f"{h}p"} for h in sorted(heights, reverse=True)]


def metadata(value):
    value = value if isinstance(value, dict) else {}
    result = {}
    for key in ("title", "series_title", "kind", "overview", "air_date"):
        text = value.get(key)
        if isinstance(text, str):
            result[key] = text[:8000 if key == "overview" else 300]
    for key in ("year", "season_number", "episode_number", "runtime_ms"):
        n = value.get(key)
        if type(n) is int and n >= 0:
            result[key] = n
    return result


def context(value, file_id=None):
    if not isinstance(value, dict) or type(value.get("file_id")) is not int or value["file_id"] <= 0:
        raise ValueError("Cinema returned no playback file")
    if file_id is not None and value["file_id"] != file_id:
        raise ValueError("Cinema returned metadata for a different playback file")
    if type(value.get("item_id")) is not int or value["item_id"] <= 0:
        raise ValueError("Cinema returned no library item")
    result = {"file_id": value["file_id"], "item_id": value["item_id"],
              "start_ms": 0, "audio": None, "subtitle": None, "metadata": metadata(value.get("metadata"))}
    nxt = value.get("next")
    result["next"] = None
    if isinstance(nxt, dict) and type(nxt.get("file_id")) is int and nxt["file_id"] > 0 \
            and type(nxt.get("item_id")) is int and nxt["item_id"] > 0:
        result["next"] = {"file_id": nxt["file_id"], "item_id": nxt["item_id"],
                          "metadata": metadata(nxt.get("metadata"))}
    return result


def display_title(info):
    title = info.get("title") or "Cinema"
    series = info.get("series_title")
    season, episode = info.get("season_number"), info.get("episode_number")
    if series and type(season) is int and type(episode) is int:
        title = f"{series} · S{season:02d}E{episode:02d} · {title}"
    return title.replace("\n", " ").replace("\r", " ")[:600]


class Preferences:
    """Only autoplay is saved; no token, URL or library history belongs here."""
    def __init__(self, directory):
        self.path = Path(directory) / "playback.json"
        self.autoplay_next = True
        self.quality = "auto"
        try:
            value = json.loads(self.path.read_text(encoding="utf-8"))
            if isinstance(value, dict) and type(value.get("autoplay_next")) is bool:
                self.autoplay_next = value["autoplay_next"]
            if isinstance(value, dict):
                candidate = value.get("quality")
                if isinstance(candidate, str) and (candidate in ("auto", "original") or
                        candidate.isdigit() and 0 < int(candidate) <= 2160):
                    self.quality = candidate
        except (OSError, ValueError):
            pass

    def set_autoplay(self, enabled):
        self._save(enabled, self.quality)
        self.autoplay_next = enabled

    def set_quality(self, choice):
        self._save(self.autoplay_next, choice)
        self.quality = choice

    def _save(self, enabled, choice):
        # Persist before changing the effective setting so a failed Save is visible.
        self.path.parent.mkdir(parents=True, exist_ok=True)
        name = None
        try:
            with tempfile.NamedTemporaryFile(mode="w", dir=self.path.parent, delete=False,
                                             encoding="utf-8") as handle:
                name = handle.name
                json.dump({"autoplay_next": enabled, "quality": choice}, handle)
                handle.write("\n")
            os.replace(name, self.path)
        finally:
            if name and os.path.exists(name):
                os.unlink(name)
