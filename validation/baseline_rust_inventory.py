"""Compile and list the ordinary Rust cell; successful discovery is not a test pass."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

from validation.baseline_commands import run_process
from validation.test_cell_manifest import libtest_inventory

PINNED_RUST = "1.97.1"


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def compile_command(command):
    if len(command) < 2 or Path(command[0]).name != "cargo" or command[1] != "test":
        raise ValueError("expected cargo test followed by the exact runtime cell flags")
    if "--" in command or "--doc" in command:
        raise ValueError("libtest arguments and doctest-only commands do not describe this compile cell")
    if any(item.startswith("--message-format") for item in command):
        raise ValueError("message format is owned by the inventory compiler")
    return [*command, *([] if "--no-run" in command else ["--no-run"]), "--message-format=json"]


def parse_listing(raw):
    return set(libtest_inventory(raw))


def artifact_inventory(path):
    artifacts = {}
    doctest_targets = set()
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        item = json.loads(line)
        if item.get("reason") != "compiler-artifact":
            continue
        target = item.get("target", {})
        if target.get("doctest"):
            doctest_targets.add((item.get("package_id", ""), target.get("name", "")))
        executable = item.get("executable")
        if not executable or item.get("profile", {}).get("test") is not True:
            continue
        record = dict(executable=executable, package_id=item["package_id"], target=target,
                      features=sorted(item.get("features", [])), profile=item["profile"])
        if executable in artifacts and artifacts[executable] != record:
            raise ValueError(f"conflicting compiler artifact identity: {executable}")
        artifacts[executable] = record
    return list(artifacts.values()), [dict(package_id=p, target=t) for p, t in sorted(doctest_targets)]


def run_capture(command, stdout, stderr, timeout):
    # The JSON compiler stream must remain separate from diagnostics. Both
    # streams share the same bounded owned-process protocol as runtime cells.
    with Path(stdout).open("wb") as output, Path(stderr).open("wb") as errors:
        result = run_process(command, output, errors, timeout)
    return dict(result, exit_code=result["returncode"])


def git_identity():
    def git(*arguments):
        return subprocess.check_output(["git", *arguments], text=True, stderr=subprocess.PIPE).strip()
    identity = dict(commit=git("rev-parse", "HEAD"), tree=git("rev-parse", "HEAD^{tree}"))
    identity["tracked_clean"] = subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], check=False).returncode == 0
    if not identity["tracked_clean"]:
        raise ValueError("tracked source differs from the candidate commit")
    return identity


def discover(output, command, artifacts=None, compiler_exit_code=None, compile_timeout=7200, list_timeout=90):
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    report = dict(schema=1, cell="ordinary-rust-sqlite", discovery_complete=False,
                  execution_status="not_executed", binaries=[], errors=[], command=command,
                  doctests=dict(status="not_discovered_or_executed", targets=[],
                                reason="cargo test --no-run omits doctests; runtime doctest evidence remains separately required"))
    started = time.monotonic()

    def persist():
        report["seconds"] = round(time.monotonic() - started, 3)
        report["expected_cases"] = sum(len(binary.get("cases", [])) for binary in report["binaries"])
        report["ignored_cases"] = sum(case["ignored"] for binary in report["binaries"]
                                      for case in binary.get("cases", []))
        temporary = output / "inventory.json.tmp"
        temporary.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        temporary.replace(output / "inventory.json")

    report["stage"] = "initializing"
    persist()
    try:
        if any(isinstance(value, bool) or not isinstance(value, (int, float))
               or not math.isfinite(value) or value <= 0 for value in (compile_timeout, list_timeout)):
            raise ValueError("compile and listing timeouts must be positive and finite")
        built_command = compile_command(command)
        report["compile_command"] = built_command
        report["source"] = git_identity()
        rustc = os.environ.get("RUSTC", "rustc")
        version = subprocess.check_output([rustc, "--version"], text=True, stderr=subprocess.PIPE).strip()
        if not re.match(r"rustc " + re.escape(PINNED_RUST) + r"(?:\s|$)", version):
            raise ValueError(f"expected Rust {PINNED_RUST}, found {version}")
        verbose = subprocess.check_output([rustc, "-vV"], text=True, stderr=subprocess.PIPE)
        host = next((line.removeprefix("host: ") for line in verbose.splitlines() if line.startswith("host: ")), None)
        if not host:
            raise ValueError("rustc did not report its host target")
        target = os.environ.get("CARGO_BUILD_TARGET", host)
        for index, item in enumerate(command):
            if item == "--target":
                target = command[index + 1]
            elif item.startswith("--target="):
                target = item.split("=", 1)[1]
        report["compiler"] = dict(version=version, verbose=verbose, target=target,
                                  rustflags=os.environ.get("RUSTFLAGS", ""),
                                  encoded_rustflags=os.environ.get("CARGO_ENCODED_RUSTFLAGS", ""),
                                  target_dir=os.environ.get("CARGO_TARGET_DIR", ""))
        artifact_path = output / "compiler-artifacts.jsonl"
        if artifacts is None:
            report["stage"] = "compiling"
            persist()
            report["compile"] = run_capture(built_command, artifact_path, output / "compiler-stderr.log", compile_timeout)
        else:
            if compiler_exit_code is None:
                raise ValueError("ingested artifacts require --compiler-exit-code")
            if Path(artifacts).resolve() != artifact_path.resolve():
                shutil.copyfile(artifacts, artifact_path)
            report["compile"] = dict(exit_code=compiler_exit_code, seconds=None, mode="ingested")
        report["stage"] = "enumerating"
        persist()
        report["compiler_artifacts_sha256"] = sha256(artifact_path)
        # Partial compiler artifacts remain useful even if compilation failed.
        try:
            binaries, targets = artifact_inventory(artifact_path)
            report["doctests"]["targets"] = targets
        except (ValueError, KeyError, OSError) as error:
            binaries = []
            report["errors"].append(f"compiler artifact discovery: {error}")
        if report["compile"]["exit_code"] != 0:
            report["errors"].append("compilation did not complete successfully")
        cleanup_blocked = report["compile"].get("cleanup_complete") is False
        if cleanup_blocked:
            report["errors"].append("compiler process cleanup unqualified; binary listing blocked")
        elif report["compile"].get("success") is False:
            report["errors"].append("compiler execution did not complete cleanly")
        for index, artifact in enumerate(binaries):
            binary = dict(artifact, listing_complete=False)
            report["binaries"].append(binary)
            if cleanup_blocked:
                binary["error"] = "listing blocked by unqualified previous process cleanup"
                persist()
                continue
            try:
                executable = Path(artifact["executable"])
                binary["sha256"] = sha256(executable)
                listings = {}
                for selection, flags in (("all", []), ("ignored", ["--ignored"])):
                    stdout = output / f"binary-{index}-{selection}.list"
                    stderr = output / f"binary-{index}-{selection}.stderr"
                    report["stage"] = f"listing:{artifact['target']['name']}:{selection}"
                    persist()
                    result = run_capture([str(executable), "--list", "--format=terse", *flags], stdout, stderr, list_timeout)
                    listings[selection] = result
                    binary["listing"] = listings
                    persist()
                    if result.get("cleanup_complete") is False:
                        cleanup_blocked = True
                    if result["exit_code"] != 0 or result.get("success") is False:
                        raise ValueError(f"{selection} listing failed: {result}")
                all_names = parse_listing((output / f"binary-{index}-all.list").read_text())
                ignored = parse_listing((output / f"binary-{index}-ignored.list").read_text())
                if not ignored <= all_names:
                    raise ValueError("ignored identities are absent from the full binary inventory")
                if sha256(executable) != binary["sha256"]:
                    raise ValueError("binary changed while its identities were listed")
                binary.update(listing_complete=True, listing=listings,
                              cases=[dict(name=name, ignored=name in ignored) for name in sorted(all_names)])
            except (OSError, ValueError) as error:
                binary["error"] = str(error)
                report["errors"].append(f"binary {artifact['target']['name']}: {error}")
        if not report["binaries"] or not any(binary.get("cases") for binary in report["binaries"]):
            report["errors"].append("no runtime test identities were discovered")
            persist()
        report["discovery_complete"] = not report["errors"]
    except (OSError, ValueError, IndexError, subprocess.SubprocessError) as error:
        report["errors"].append(str(error))
    report["stage"] = "finished"
    persist()
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--compiler-exit-code", type=int)
    parser.add_argument("--compile-timeout", type=int, default=7200)
    parser.add_argument("--list-timeout", type=int, default=90)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    report = discover(args.output, command, args.artifacts, args.compiler_exit_code,
                      args.compile_timeout, args.list_timeout)
    print(json.dumps(dict(discovery_complete=report["discovery_complete"], execution_status="not_executed",
                         expected_cases=report["expected_cases"], errors=report["errors"])))
    return 0 if report["discovery_complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
