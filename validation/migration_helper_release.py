"""Closed optional helper cohort for the signed desktop release feed.

Only the packaging job executes the native helper. This signer-side verifier
reads its bounded description and actual archive without executing candidate code.
"""
import hashlib
import json
from pathlib import Path
import re
import tarfile
import zlib

TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux", "amd64"),
    "aarch64-unknown-linux-gnu": ("linux", "arm64"),
    "x86_64-apple-darwin": ("macos", "amd64"),
    "aarch64-apple-darwin": ("macos", "arm64"),
}
DESCRIPTION_FIELDS = {"version", "build", "source_cohorts", "converter_sha256", "native_schema_version", "manifest_version", "transition_version"}
HELPER_FIELDS = {"artifact", "daemon_build", "helper_build", "helper_version", "profile", "executable_name", "executable_sha256", "bundle_sha256", "helper_protocol", "request_format", "receipt_format", "source_cohorts", "output", "converter_sha256"}
MAX_EXECUTABLE = 2 * 1024**3
EXECUTABLE = "plurx-storage-migrate"


def exact(value, fields):
    if not isinstance(value, dict) or set(value) != fields:
        raise ValueError("unexpected migration helper fields")


def sha(value):
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise ValueError("invalid helper SHA256")


def build(value, commit, tree):
    exact(value, {"commit", "tree", "target"})
    if value["commit"] != commit or value["tree"] != tree or (not isinstance(value["target"], str) or value["target"] not in TARGETS):
        raise ValueError("migration helper build differs from exact daemon source/target")


def cohorts(value):
    if not isinstance(value, list) or len(value) != 2:
        raise ValueError("migration helper requires the closed AUTH81/AUTH82 cohorts")
    for row, version in zip(value, (81, 82)):
        exact(row, {"auth_version", "canonical_layout_fingerprint"})
        if type(row["auth_version"]) is not int or row["auth_version"] != version:
            raise ValueError("migration helper source cohorts differ")
        sha(row["canonical_layout_fingerprint"])


def validate_helpers(value, manifest):
    if not isinstance(value, list) or not 1 <= len(value) <= 4:
        raise ValueError("invalid optional migration helper cohort")
    names = {a["name"] for a in manifest["artifacts"]}
    targets = set()
    for helper in value:
        exact(helper, HELPER_FIELDS)
        build(helper["daemon_build"], manifest["source_commit"], manifest["source_tree"])
        if helper["helper_build"] != helper["daemon_build"] or helper["helper_version"] != manifest["version"]:
            raise ValueError("helper and installed daemon build/version differ")
        target = helper["daemon_build"]["target"]
        if target in targets:
            raise ValueError("duplicate migration helper target")
        targets.add(target)
        if helper["profile"] != "storage_migration" or helper["executable_name"] != EXECUTABLE:
            raise ValueError("unsupported helper executable/profile")
        for field in ("helper_protocol", "request_format", "receipt_format"):
            if type(helper[field]) is not int or helper[field] != 1:
                raise ValueError("unsupported helper protocol")
        cohorts(helper["source_cohorts"])
        exact(helper["output"], {"native_schema_version", "manifest_format", "transition_format"})
        output = helper["output"]
        if type(output["native_schema_version"]) is not int or not 0 < output["native_schema_version"] < 2**63:
            raise ValueError("invalid helper native schema")
        for field in ("manifest_format", "transition_format"):
            if type(output[field]) is not int or output[field] != 1:
                raise ValueError("unsupported helper output format")
        for field in ("converter_sha256", "executable_sha256", "bundle_sha256"):
            sha(helper[field])
        artifact = helper["artifact"]
        exact(artifact, {"name", "platform", "architecture", "format", "size", "sha256"})
        expected = f"{EXECUTABLE}-{manifest['version']}-{target}.tar.gz"
        platform, architecture = TARGETS[target]
        if artifact["name"] != expected or artifact["name"] in names or artifact["platform"] != platform or artifact["architecture"] != architecture or artifact["format"] != "tar_gz":
            raise ValueError("helper artifact identity differs from closed target cohort")
        names.add(artifact["name"])
        if type(artifact["size"]) is not int or not 0 < artifact["size"] <= 8 * 1024**3 or artifact["sha256"] != helper["bundle_sha256"]:
            raise ValueError("invalid helper artifact size/hash")


class SingleGzip:
    """Streaming, bounded single gzip member. No concatenation or CRC bypass."""
    def __init__(self, stream):
        self.stream = stream
        self.decoder = zlib.decompressobj(31)
        self.pending = b""
        self.total = 0
        self.finished = False

    def read(self, size):
        if size <= 0:
            return b""
        output = bytearray()
        while len(output) < size and not self.finished:
            data = self.pending or self.stream.read(65536)
            if not data:
                raise ValueError("truncated helper gzip member")
            try:
                chunk = self.decoder.decompress(data, size - len(output))
            except zlib.error as error:
                raise ValueError("invalid helper gzip member") from error
            self.pending = self.decoder.unconsumed_tail
            self.total += len(chunk)
            if self.total > MAX_EXECUTABLE + 1024**2:
                raise ValueError("helper archive expansion exceeds bound")
            output.extend(chunk)
            if self.decoder.eof:
                if self.decoder.unused_data or self.stream.read(1):
                    raise ValueError("trailing helper gzip members or bytes")
                self.finished = True
        return bytes(output)


def executable_digest(path):
    with path.open("rb") as compressed:
        stream = SingleGzip(compressed)
        block = stream.read(512)
        if len(block) != 512:
            raise ValueError("helper archive lacks a complete header")
        try:
            member = tarfile.TarInfo.frombuf(block, "utf-8", "strict")
        except (tarfile.HeaderError, UnicodeError) as error:
            raise ValueError("invalid helper tar header") from error
        if member.name != EXECUTABLE or member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE) or member.linkname or not 0 < member.size <= MAX_EXECUTABLE or member.mode & 0o7000 or not member.mode & 0o111:
            raise ValueError("helper archive must contain only one root regular executable")
        remaining = member.size
        digest = hashlib.sha256()
        while remaining:
            block = stream.read(min(65536, remaining))
            if not block:
                raise ValueError("truncated helper executable")
            remaining -= len(block)
            digest.update(block)
        padding = (-member.size) % 512
        if stream.read(padding) != bytes(padding):
            raise ValueError("invalid helper tar padding")
        zeros = 0
        while True:
            block = stream.read(512)
            if not block:
                break
            if len(block) != 512 or block != bytes(512):
                raise ValueError("extra helper archive member or trailing nonzero data")
            zeros += 1
        if zeros < 2:
            raise ValueError("missing helper tar terminator")
        return digest.hexdigest()


def read_description(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
        raise ValueError("helper description is not bounded regular data")
    def pairs(rows):
        result = {}
        for key, value in rows:
            if key in result:
                raise ValueError("duplicate helper description key")
            result[key] = value
        return result
    with path.open("rb") as stream:
        raw = stream.read(65537)
    if len(raw) > 65536:
        raise ValueError("helper description grew beyond bound")
    return json.loads(raw, object_pairs_hook=pairs)


def validate_description(description, commit, tree, target=None):
    exact(description, DESCRIPTION_FIELDS)
    build(description["build"], commit, tree)
    if target is not None and description["build"]["target"] != target:
        raise ValueError("actual helper target differs from packaging target")
    for field in ("version", "manifest_version", "transition_version"):
        if type(description[field]) is not int or description[field] != 1:
            raise ValueError("unsupported actual helper description")
    cohorts(description["source_cohorts"])
    sha(description["converter_sha256"])
    if type(description["native_schema_version"]) is not int or not 0 < description["native_schema_version"] < 2**63:
        raise ValueError("invalid actual helper native schema")
    return description


def from_description(path, assets, manifest, artifact_digest):
    description = read_description(path)
    validate_description(description, manifest["source_commit"], manifest["source_tree"])
    target = description["build"]["target"]
    name = f"{EXECUTABLE}-{manifest['version']}-{target}.tar.gz"
    bundle = assets / name
    artifact = dict(name=name, platform=TARGETS[target][0], architecture=TARGETS[target][1], format="tar_gz") | artifact_digest(bundle)
    helper = dict(artifact=artifact, daemon_build=description["build"], helper_build=description["build"],
                  helper_version=manifest["version"], profile="storage_migration", executable_name=EXECUTABLE,
                  executable_sha256=executable_digest(bundle), bundle_sha256=artifact["sha256"],
                  helper_protocol=description["version"], request_format=1, receipt_format=1,
                  source_cohorts=description["source_cohorts"], output=dict(native_schema_version=description["native_schema_version"],
                  manifest_format=description["manifest_version"], transition_format=description["transition_version"]),
                  converter_sha256=description["converter_sha256"])
    # Detect replacement between compressed hashing and archive inspection.
    if artifact_digest(bundle) != {k: artifact[k] for k in ("size", "sha256")}:
        raise ValueError("helper bundle changed during qualification")
    validate_helpers([helper], manifest)
    return helper
