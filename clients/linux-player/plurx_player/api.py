"""A small JSON client for one plurx origin.

The handoff token lives only in this object's memory. It is sent in the
Authorization header and nowhere else, and redirects are never followed:
urllib's default handler would carry the header to wherever a redirect
points.
"""

import json
import ssl
import urllib.error
import urllib.parse
import urllib.request


class ApiError(Exception):
    def __init__(self, message, status=None, body=None):
        super().__init__(message)
        self.status = status
        self.body = body


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def error_text(body, default):
    """The server's own words for an error, from either envelope (API.md section 3.1)."""
    if isinstance(body, dict):
        for key in ("message", "error", "code"):
            value = body.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()[:200]
    return default


class Http:
    def __init__(self, origin, ca_file=None, user_agent="plurx-player", timeout=10.0):
        self.origin = origin
        self.token = None
        self.user_agent = user_agent
        self.timeout = timeout
        context = ssl.create_default_context()
        if ca_file:
            context.load_verify_locations(cafile=ca_file)
        self._opener = urllib.request.build_opener(
            urllib.request.HTTPSHandler(context=context), _NoRedirect())

    def path_of(self, url):
        """A server path from a path or a same-origin absolute URL; refuses any other origin."""
        if not isinstance(url, str) or not url:
            raise ApiError("the server sent no URL")
        if url.startswith("/") and not url.startswith("//"):
            return url
        parts = urllib.parse.urlsplit(url)
        from .handoff import normalize_origin  # local import: no cycle at module load
        try:
            origin = normalize_origin(f"{parts.scheme}://{parts.netloc}")
        except ValueError:
            raise ApiError("the server sent a URL that is not on this server") from None
        if origin != self.origin:
            raise ApiError("the server sent a URL on another origin")
        return urllib.parse.urlunsplit(("", "", parts.path or "/", parts.query, ""))

    def url(self, path):
        return self.origin + self.path_of(path)

    def request(self, method, path, body=None, auth=True, timeout=None):
        """Return ``(status, json_or_none)``; raise ApiError only when nothing answered."""
        headers = {"Accept": "application/json", "User-Agent": self.user_agent}
        data = None
        if body is not None:
            data = json.dumps(body, separators=(",", ":")).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if auth and self.token:
            headers["Authorization"] = "Bearer " + self.token
        request = urllib.request.Request(self.url(path), data=data, headers=headers, method=method)
        try:
            with self._opener.open(request, timeout=timeout or self.timeout) as response:
                return response.status, _json(response.read())
        except urllib.error.HTTPError as error:
            try:
                payload = _json(error.read())
            finally:
                error.close()
            return error.code, payload
        except (urllib.error.URLError, OSError, ValueError) as error:
            reason = getattr(error, "reason", error)
            raise ApiError(f"{method} {self.path_of(path).split('?')[0]}: {reason}") from None


def _json(raw):
    if not raw:
        return None
    try:
        return json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return {"error": raw[:200].decode("utf-8", "replace")}
