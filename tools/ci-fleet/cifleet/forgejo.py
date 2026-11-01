"""A small, read-mostly Forgejo Actions client.

Two facts about Forgejo 16 shape it, both measured on the live forge on
2026-11-01 (docs/ci/CI-FLEET.md, "What Forgejo's API gives us"):

* `/actions/tasks` and `/actions/runs` ignore `limit` unless `page` is also
  sent, and then return every row ever recorded (25,040 tasks, 11.6 MB, 15 s).
  Every list call here therefore carries `page`, and `get` refuses a list path
  without one, so the collector can never put that load on the forge host.
* No task, job or runner record names the runner that took a job. Line 1 of
  the job's log does: `Runner <uuid> (version:…) received task <id> …`.
  `job_runner` reads only the first few hundred bytes of the log.
"""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.parse
import urllib.request

PAGED = ("/actions/tasks", "/actions/runs", "/actions/runners")
PAGE_SIZE = 50
LOG_HEAD_BYTES = 512
RUNNER_LINE = re.compile(r"Runner ([0-9a-f-]{36}) \(version:([^)]*)\) received task (\d+)")


class ForgejoError(RuntimeError):
    def __init__(self, status: int, path: str, body: str = ""):
        super().__init__(f"{status} {path}: {body[:200]}")
        self.status = status


class Forgejo:
    def __init__(self, base_url: str, repo: str, token: str, *, timeout: float = 20.0, opener=None):
        self.base = base_url.rstrip("/") + "/api/v1"
        self.repo = repo
        self.token = token
        self.timeout = timeout
        self.opener = opener or urllib.request.urlopen
        self.calls = 0

    # -- transport -----------------------------------------------------------
    def _request(self, method: str, path: str, params: dict | None = None, *, token: str | None = None,
                 body: dict | None = None, head: int | None = None):
        if method == "GET" and path.endswith(PAGED) and not (params and "page" in params):
            raise ValueError(f"{path} must be paged: Forgejo ignores limit without page")
        url = self.base + path
        if params:
            url += "?" + urllib.parse.urlencode(params)
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        request.add_header("Authorization", "token " + (token or self.token))
        request.add_header("Accept", "application/json")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        self.calls += 1
        try:
            with self.opener(request, timeout=self.timeout) as response:
                raw = response.read(head) if head else response.read()
        except urllib.error.HTTPError as error:
            raise ForgejoError(error.code, path, error.read(300).decode("utf-8", "replace")) from None
        return raw

    def get(self, path: str, **params):
        raw = self._request("GET", path, params or None)
        return json.loads(raw) if raw else None

    def repo_path(self, suffix: str) -> str:
        return f"/repos/{self.repo}{suffix}"

    def _pages(self, suffix: str, key: str | None = None, *, limit_pages: int = 20, **params):
        rows: list = []
        for page in range(1, limit_pages + 1):
            data = self.get(self.repo_path(suffix), page=page, limit=PAGE_SIZE, **params)
            batch = data.get(key, []) if key else data
            rows.extend(batch or [])
            if not batch or len(batch) < PAGE_SIZE:
                break
        return rows

    # -- reads ---------------------------------------------------------------
    def version(self) -> str:
        return (self.get("/version") or {}).get("version", "")

    def runners(self) -> list[dict]:
        return self._pages("/actions/runners")

    def queue(self) -> list[dict]:
        """Waiting and running jobs with their `runs_on`. Omits blocked jobs."""
        return self.get(self.repo_path("/actions/runners/jobs")) or []

    def runs(self, status: str, *, pages: int = 2) -> list[dict]:
        return self._pages("/actions/runs", "workflow_runs", limit_pages=pages, status=status)

    def run(self, run_id: int) -> dict:
        return self.get(self.repo_path(f"/actions/runs/{run_id}"))

    def run_jobs(self, run_id: int) -> list[dict]:
        return self.get(self.repo_path(f"/actions/runs/{run_id}/jobs")) or []

    def tasks(self, page: int = 1, *, status: str | None = None) -> list[dict]:
        params = {"page": page, "limit": PAGE_SIZE}
        if status:
            params["status"] = status
        data = self.get(self.repo_path("/actions/tasks"), **params) or {}
        return data.get("workflow_runs", [])

    def job_runner(self, job_id: int) -> tuple[str, str, int] | None:
        """(runner uuid, runner version, task id) from line 1 of the job log.

        None while the job has not been placed (Forgejo answers 404 "job has not
        been executed yet").
        """
        try:
            raw = self._request("GET", self.repo_path(f"/actions/jobs/{job_id}/logs"), head=LOG_HEAD_BYTES)
        except ForgejoError as error:
            if error.status == 404:
                return None
            raise
        return parse_runner_line(raw.decode("utf-8", "replace"))

    def file(self, path: str, ref: str = "main") -> str:
        raw = self._request("GET", self.repo_path("/raw/" + urllib.parse.quote(path)), {"ref": ref})
        return raw.decode("utf-8")

    def directory(self, path: str, ref: str = "main") -> list[str]:
        entries = self.get(self.repo_path("/contents/" + urllib.parse.quote(path)), ref=ref) or []
        return [entry["path"] for entry in entries if entry.get("type") == "file"]

    def head(self, ref: str = "main") -> str:
        return (self.get(self.repo_path("/branches/" + urllib.parse.quote(ref))) or {}).get("commit", {}).get("id", "")

    # -- identity and writes (operator token) ---------------------------------
    def operator(self, token: str) -> tuple[str, bool]:
        """(login, may write the repository) for an operator's own token."""
        user = json.loads(self._request("GET", "/user", token=token))
        repo = json.loads(self._request("GET", self.repo_path(""), token=token))
        permissions = repo.get("permissions") or {}
        return user.get("login", ""), bool(permissions.get("push") or permissions.get("admin"))

    def cancel_run(self, run_id: int, token: str) -> None:
        self._request("POST", self.repo_path(f"/actions/runs/{run_id}/cancel"), token=token, body={})

    def delete_runner(self, runner_id: int, token: str) -> None:
        self._request("DELETE", self.repo_path(f"/actions/runners/{runner_id}"), token=token)


def parse_runner_line(text: str) -> tuple[str, str, int] | None:
    match = RUNNER_LINE.search(text.split("\n", 1)[0])
    if not match:
        return None
    return match.group(1), match.group(2), int(match.group(3))
