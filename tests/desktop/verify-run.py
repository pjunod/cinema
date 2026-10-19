#!/usr/bin/env python3
"""Bind qualification to the exact successful read-only candidate artifact run."""
import argparse
import json
import os
from pathlib import Path
import urllib.request
from common import check_build_artifacts, check_identity, write_json

p = argparse.ArgumentParser()
p.add_argument("--run-id", required=True)
p.add_argument("--candidate", required=True)
p.add_argument("--output", type=Path, required=True)
a = p.parse_args()
# Validate syntax before incorporating an ID into a request path.
if not a.run_id.isascii() or not a.run_id.isdecimal() or int(a.run_id) < 1:
    p.error("run ID must be a positive decimal integer")
repo = os.environ["GITHUB_REPOSITORY"]
request = urllib.request.Request(
    f"https://api.github.com/repos/{repo}/actions/runs/{a.run_id}",
    headers={"Authorization": "Bearer " + os.environ["GH_TOKEN"], "Accept": "application/vnd.github+json",
             "X-GitHub-Api-Version": "2022-11-28"})
with urllib.request.urlopen(request, timeout=30) as response:
    record = json.load(response)
verified = check_identity(a.run_id, a.candidate, os.environ["GITHUB_SHA"], record, repo)
artifacts_request = urllib.request.Request(
    f"https://api.github.com/repos/{repo}/actions/runs/{a.run_id}/artifacts?per_page=100",
    headers={"Authorization": "Bearer " + os.environ["GH_TOKEN"], "Accept": "application/vnd.github+json",
             "X-GitHub-Api-Version": "2022-11-28"})
with urllib.request.urlopen(artifacts_request, timeout=30) as response:
    artifacts = check_build_artifacts(json.load(response))
verified["build_artifacts"] = artifacts
verified.update(repository=repo, run_attempt=record["run_attempt"], html_url=record["html_url"])
write_json(a.output, verified)
