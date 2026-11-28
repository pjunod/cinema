#!/usr/bin/env python3
"""Attach verified Linux Player package files to an immutable Forgejo release."""

import hashlib
import json
import os
from pathlib import Path
import re
import sys
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, Request, build_opener
from uuid import uuid4


class NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def release_files(directory):
    sums = directory / "SHA256SUMS"
    files = {}
    for line in sums.read_text(encoding="ascii").splitlines():
        digest, name = line.split(maxsplit=1)
        if not re.fullmatch(r"plurx-player_[a-zA-Z0-9.+~_:-]+", name) or name in files:
            raise ValueError(f"unexpected build output: {name}")
        path = directory / name
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f"{name} differs from SHA256SUMS")
        files[name] = path
    packages = [name for name in files if name.endswith(".deb")]
    if len(packages) != 1:
        raise ValueError("expected exactly one Linux Player .deb")
    match = re.fullmatch(r"plurx-player_(\d[\w.+~:-]*)_amd64\.deb", packages[0])
    if match is None:
        raise ValueError(f"unexpected Linux Player package: {packages[0]}")
    version = match[1]
    buildinfo = directory / f"plurx-player_{version}_amd64.buildinfo"
    if not buildinfo.is_file():
        raise ValueError(f"{buildinfo.name} is missing")
    package = files[packages[0]]
    validate_buildinfo(buildinfo.read_bytes(), package)
    return version, files | {buildinfo.name: buildinfo, sums.name: sums}, package


def validate_buildinfo(data, package):
    content = data.decode("utf-8")
    checksum = hashlib.sha256(package.read_bytes()).hexdigest()
    section = re.search(r"(?m)^Checksums-Sha256:\n((?: .+\n)+)", content)
    subject = f" {checksum} {package.stat().st_size} {package.name}"
    if section is None or subject not in section[1].splitlines():
        raise ValueError(f".buildinfo does not attest {package.name}")


def attach(directory):
    version, files, package = release_files(directory)
    origin = os.environ["FORGEJO_SERVER_URL"].rstrip("/")
    repository = os.environ["FORGEJO_REPOSITORY"]
    revision = os.environ["FORGEJO_SHA"]
    # Neither underscore nor these two separators can appear in an encoded
    # Debian version unless represented by their escape here.
    tag = "plurx-player-deb-" + version.replace("_", "_u").replace(":", "_c").replace("~", "_t")
    project = f"{origin}/api/v1/repos/{repository}"
    api = f"{project}/releases"
    token = os.environ["RELEASE_TOKEN"]
    opener = build_opener(NoRedirects())

    def request(url, method="GET", payload=None, mime="application/json"):
        if not url.startswith(f"{origin}/"):
            raise ValueError("release URL escaped the Forgejo server")
        headers = {"Authorization": f"token {token}", "Content-Type": mime}
        return opener.open(Request(url, payload, headers, method=method), timeout=60)

    def check_tag(required=False):
        try:
            with request(f"{project}/tags/{tag}") as response:
                tagged = json.load(response)["commit"]["sha"]
        except HTTPError as error:
            if error.code == 404 and not required:
                return
            raise
        if tagged != revision:
            raise ValueError(f"tag {tag} points to {tagged}, expected {revision}")

    check_tag()
    receipt = f"CI-verified Linux Player Debian release. Source commit `{revision}`."
    try:
        with request(f"{api}/tags/{tag}") as response:
            release = json.load(response)
    except HTTPError as error:
        if error.code != 404:
            raise
        body = {"tag_name": tag, "target_commitish": revision, "name": f"Linux Player {version}",
                "body": receipt, "draft": True, "prerelease": True}
        with request(api, "POST", json.dumps(body).encode()) as response:
            release = json.load(response)

    if release["body"] != receipt or (release["draft"] and release["target_commitish"] != revision):
        raise ValueError(f"{tag} belongs to a different source commit")
    assets_url = f"{api}/{release['id']}/assets"
    with request(assets_url) as response:
        assets = {row["name"]: row["browser_download_url"] for row in json.load(response)}
    if assets.keys() - files.keys() or (not release["draft"] and assets.keys() != files.keys()):
        raise ValueError(f"{tag} has an unexpected asset list")
    for name, path in files.items():
        data = path.read_bytes()
        if name in assets:
            with request(assets[name]) as response:
                existing = response.read()
                if name.endswith(".buildinfo"):
                    # Build-Date is allowed to vary. Retain the original attestation.
                    validate_buildinfo(existing, package)
                elif existing != data:
                    raise ValueError(f"{tag}/{name} differs from the released bytes; bump the Debian version")
            continue
        boundary = uuid4().hex
        header = (f"--{boundary}\r\nContent-Disposition: form-data; "
                  f'name="attachment"; filename="{name}"\r\n'
                  "Content-Type: application/octet-stream\r\n\r\n")
        payload = header.encode("ascii") + data + f"\r\n--{boundary}--\r\n".encode("ascii")
        with request(f"{assets_url}?{urlencode({'name': name})}", "POST", payload,
                     f"multipart/form-data; boundary={boundary}"):
            pass
    if release["draft"]:
        with request(f"{api}/{release['id']}", "PATCH", b'{"draft":false}'):
            pass
    check_tag(required=True)
    print(f"Forgejo release {tag} has the verified Linux Player package and its build inputs")


if __name__ == "__main__":
    try:
        attach(Path(sys.argv[1]))
    except (OSError, KeyError, ValueError) as error:
        sys.exit(f"attach-apt-release: {error}")
