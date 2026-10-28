"""Which plurx servers this user lets the player talk to.

Any web page can navigate to a ``plurx-player://`` URL. Without this list a
hostile page could point the player at its own server and feed mpv and
libavcodec whatever it likes. So a new origin is played only after the user
says yes once, in a desktop dialog naming it. Plain http is allowed only
through the same confirmation, because LAN servers are often plain http.

The file holds origins (and an optional CA path per origin), never a token.
"""

import html
import json
import os
import subprocess
import tempfile
import time


def default_config_dir():
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config")
    return os.path.join(base, "plurx-player")


class TrustStore:
    def __init__(self, config_dir=None):
        self.config_dir = config_dir or default_config_dir()
        self.path = os.path.join(self.config_dir, "servers.json")

    def _load(self):
        try:
            with open(self.path, encoding="utf-8") as handle:
                data = json.load(handle)
        except (FileNotFoundError, NotADirectoryError):
            return {}
        except (OSError, ValueError):
            # Unreadable or corrupt: trust nothing, ask again.
            return {}
        servers = data.get("servers") if isinstance(data, dict) else None
        return servers if isinstance(servers, dict) else {}

    def entry(self, origin):
        value = self._load().get(origin)
        return value if isinstance(value, dict) else None

    def is_trusted(self, origin):
        return self.entry(origin) is not None

    def add(self, origin, ca=None):
        servers = self._load()
        entry = dict(servers.get(origin) or {})
        entry.setdefault("trusted_at", time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
        if ca:
            entry["ca"] = os.path.abspath(ca)
        servers[origin] = entry
        self._save(servers)

    def _save(self, servers):
        os.makedirs(self.config_dir, mode=0o700, exist_ok=True)
        fd, temp = tempfile.mkstemp(prefix=".servers-", suffix=".json", dir=self.config_dir)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump({"servers": servers}, handle, indent=2, sort_keys=True)
                handle.write("\n")
            os.chmod(temp, 0o600)
            os.replace(temp, self.path)
        except BaseException:
            try:
                os.unlink(temp)
            except OSError:
                pass
            raise


def zenity_confirm(origin, zenity="zenity"):
    """Ask once whether to play from ``origin``. Anything but Allow refuses."""
    plain = origin.startswith("http://")
    text = (f"Allow plurx Player to play from <b>{html.escape(origin)}</b>?\n\n"
            "Only allow a plurx server you use. A web page asked to open it.")
    if plain:
        text += ("\n\nThis server uses plain http: the connection is not "
                 "encrypted, so allow it only on a network you trust.")
    try:
        result = subprocess.run(
            [zenity, "--question", "--title=plurx Player", "--width=420",
             "--ok-label=Allow", "--cancel-label=Refuse", f"--text={text}"],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            timeout=600, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def zenity_error(message, zenity="zenity"):
    """Tell the user something failed. Launched from a browser, there is no terminal."""
    try:
        subprocess.run([zenity, "--error", "--title=plurx Player", "--width=420",
                        f"--text={html.escape(message)}"],
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, timeout=600, check=False)
    except (OSError, subprocess.TimeoutExpired):
        pass


def ensure_trusted(store, origin, confirm, ca=None):
    """True when ``origin`` may be contacted; asks (once) for a new origin."""
    if store.is_trusted(origin):
        if ca:
            store.add(origin, ca)
        return True
    if not confirm(origin):
        return False
    store.add(origin, ca)
    return True
