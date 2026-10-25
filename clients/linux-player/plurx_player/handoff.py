"""Parse the ``plurx-player://`` URL the web app navigates to.

Anything can navigate to this scheme: a hostile page as easily as plurx.
So the parser is strict and refuses rather than repairs. The URL carries
exactly two parameters, ``server`` (an http(s) origin with no path) and
``code`` (the one-time handoff code), and nothing else.
"""

import re
import urllib.parse
from dataclasses import dataclass

SCHEME = "plurx-player"
ACTION = "play"
MAX_URL = 4096
# 32 random bytes in base64url is 43 characters; allow some slack either way
# without admitting anything that is not base64url.
CODE_RE = re.compile(r"^[A-Za-z0-9_-]{16,256}$")
_CONTROL = re.compile(r"[\x00-\x20\x7f]")
_DEFAULT_PORTS = {"http": 80, "https": 443}


class HandoffUrlError(ValueError):
    """The URL is not a plurx Player handoff; nothing was contacted."""


@dataclass(frozen=True)
class Handoff:
    server: str
    code: str


def normalize_origin(value):
    """Return the canonical ``scheme://host[:port]`` form of an origin.

    Refuses anything that is not exactly an origin: a path, query, fragment,
    user information, or a scheme other than http and https.
    """
    if not isinstance(value, str) or not value or _CONTROL.search(value):
        raise HandoffUrlError("server is not an origin")
    try:
        parts = urllib.parse.urlsplit(value)
        port = parts.port
    except ValueError as error:
        raise HandoffUrlError(f"server is not an origin: {error}") from None
    if parts.scheme not in _DEFAULT_PORTS:
        raise HandoffUrlError("server must be an http or https origin")
    if parts.username is not None or parts.password is not None or "@" in parts.netloc:
        raise HandoffUrlError("server must not carry user information")
    if parts.path or parts.query or parts.fragment or value.endswith(("?", "#")):
        raise HandoffUrlError("server must be an origin with no path")
    host = parts.hostname
    if not host:
        raise HandoffUrlError("server has no host")
    if ":" in host:
        host = f"[{host}]"
    if port is None or port == _DEFAULT_PORTS[parts.scheme]:
        return f"{parts.scheme}://{host}"
    return f"{parts.scheme}://{host}:{port}"


def parse_handoff_url(url):
    if not isinstance(url, str) or len(url) > MAX_URL or _CONTROL.search(url):
        raise HandoffUrlError("not a plurx Player URL")
    parts = urllib.parse.urlsplit(url)
    if parts.scheme != SCHEME:
        raise HandoffUrlError(f"not a {SCHEME}:// URL")
    if parts.netloc.lower() != ACTION or parts.path not in ("", "/"):
        raise HandoffUrlError(f"unknown action; expected {SCHEME}://{ACTION}?...")
    if parts.fragment or url.endswith("#"):
        raise HandoffUrlError("unexpected fragment")
    try:
        query = urllib.parse.parse_qs(parts.query, keep_blank_values=True,
                                      strict_parsing=True, max_num_fields=8)
    except ValueError:
        raise HandoffUrlError("malformed query") from None
    if set(query) != {"server", "code"}:
        raise HandoffUrlError("expected exactly server= and code=")
    if any(len(values) != 1 for values in query.values()):
        raise HandoffUrlError("server= and code= must appear once each")
    code = query["code"][0]
    if not CODE_RE.match(code):
        raise HandoffUrlError("malformed handoff code")
    return Handoff(server=normalize_origin(query["server"][0]), code=code)
