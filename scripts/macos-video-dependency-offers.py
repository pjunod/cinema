#!/usr/bin/env python3
"""Verify and extract pinned dependency offers without reconstructing Git metadata."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile

POLICY = Path(__file__).with_name("macos-video-dependency-sources.json")
MODEL = "opus_data-a5177ec6fb7d15058e99e57029746100121f68e4890b1467d4094aa336b6013e.tar.gz"
ALIASES = {"openssl": "openssl-3.6.3", "xz": "v5.8.3", "libpng": "v1.6.58"}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def archive_name(role, facts):
    if facts["kind"] in ("git-source-archive", "git-submodule-source-archive"):
        return role + "-" + facts["commit"] + ".tar.gz"
    if facts["kind"] == "svn-source-archive":
        return role + "-svn-source.tar.gz"
    if facts["kind"] == "upstream-source-archive":
        return role
    raise ValueError("unknown dependency source kind")


def verify_offers(directory, roles=None):
    expected = json.loads(POLICY.read_text())
    manifest_path = directory / "manifest.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("dependency manifest must be a regular file")
    actual = json.loads(manifest_path.read_text())
    if not actual or set(actual) - set(expected):
        raise ValueError("unknown dependency source role")
    required = set(expected) if roles is None else set(roles)
    if not required <= set(actual):
        raise ValueError("missing required dependency source role")
    for role in actual:
        if actual[role] != expected[role]:
            raise ValueError("dependency source identity mismatch: " + role)
        if roles is not None and role not in required:
            continue
        path = directory / archive_name(role, actual[role])
        if path.is_symlink() or not path.is_file() or digest(path) != actual[role]["sha256"]:
            raise ValueError("dependency source archive mismatch: " + role)
    return actual


def stage_offers(directory, destination):
    actual = verify_offers(directory)
    destination.mkdir()
    for role, facts in actual.items():
        name = archive_name(role, facts)
        target = destination / name
        shutil.copyfile(directory / name, target)
        if digest(target) != facts["sha256"]:
            raise ValueError("dependency archive changed while staging")
        target.chmod(0o444)
    opus = directory / archive_name("opus", actual["opus"])
    with tarfile.open(opus) as archive:
        notice = archive.extractfile("COPYING").read()
        recipe = archive.extractfile("autogen.sh").read()
    if hashlib.sha256(notice).hexdigest() != "01e1167d54a096d123cf6dfbbeb19587278845c6481d2d66d545669846079551":
        raise ValueError("pinned Opus license notice changed")
    if ('dnn/download_model.sh "' + actual[MODEL]["sha256"] + '"').encode() not in recipe:
        raise ValueError("Opus model is not bound to the pinned official recipe")
    (destination / "opus-COPYING").write_bytes(notice)
    (destination / "opus-model-license-scope.json").write_text(json.dumps({
        "model_archive": MODEL, "model_sha256": actual[MODEL]["sha256"],
        "source_role": "opus", "recipe": "autogen.sh", "recipe_sha256": hashlib.sha256(recipe).hexdigest(),
        "notice": "opus-COPYING", "notice_sha256": hashlib.sha256(notice).hexdigest(),
        "official_implementation_license": "https://www.opus-codec.org/license/",
        "scope": "Pinned Opus implementation COPYING retained; model archive contains no separate license notice."}, indent=2, sort_keys=True) + "\n")
    (destination / "manifest.json").write_text(json.dumps(actual, indent=2, sort_keys=True) + "\n")
    return {"manifest_sha256": digest(destination / "manifest.json"), "roles": sorted(actual)}


def verify_preparation(root):
    facts = json.loads((root / "prepared-source.json").read_text())["dependency_source_inputs"]
    paths = {"manifest_sha256": root / "dependency-source-inputs/manifest.json",
             "recipe_bindings_sha256": root / "dependency-recipe-bindings.json",
             "helper_sha256": Path(__file__), "policy_sha256": POLICY}
    for key, path in paths.items():
        if path.is_symlink() or not path.is_file() or digest(path) != facts.get(key):
            raise ValueError("prepared dependency input changed: " + key)
    return json.loads(paths["recipe_bindings_sha256"].read_text())


def verify_recipe(root, recipe, role):
    bindings = verify_preparation(root)
    if recipe not in bindings or role not in bindings[recipe]["roles"]:
        raise ValueError("dependency role is not bound to this pinned recipe")
    path = root / "source" / recipe
    if path.is_symlink() or digest(path) != bindings[recipe]["sha256"]:
        raise ValueError("dependency recipe changed after preparation")
    return {"recipe": recipe, "recipe_sha256": digest(path)}


def copy_upstream(root, role, recipe):
    binding = verify_recipe(root, recipe, role)
    directory = root / "dependency-source-inputs"
    actual = verify_offers(directory, [role])
    if actual[role]["kind"] != "upstream-source-archive":
        raise ValueError("requested source is not an upstream archive")
    source = directory / archive_name(role, actual[role])
    destination = Path.cwd() / source.name
    shutil.copyfile(source, destination)
    if digest(destination) != actual[role]["sha256"]:
        raise ValueError("upstream source changed while copying")
    record = {**binding, "role": role, "recipe_revision": actual[role]["sha256"],
              "inputs": [{"role": role, "archive": source.name, "sha256": actual[role]["sha256"]}],
              "extraction_helper_sha256": digest(Path(__file__))}
    with (root / "dependency-source-consumption.jsonl").open("a") as output:
        output.write(json.dumps(record, sort_keys=True) + "\n")
    return record


def extract_source(root, role, revision, destination, recipe):
    binding = verify_recipe(root, recipe, role)
    directory = root / "dependency-source-inputs"
    expected = json.loads(POLICY.read_text())
    if role not in expected:
        raise ValueError("unknown requested dependency role")
    entry = expected[role]
    if str(entry.get("commit", entry.get("revision", entry["sha256"]))) != revision and ALIASES.get(role) != revision:
        raise ValueError("dependency recipe revision mismatch")
    selected = [role] + sorted(name for name in expected if name.startswith(role + "--"))
    if role == "opus":
        selected.append(MODEL)
    actual = verify_offers(directory, selected)
    if destination.exists() or destination.is_symlink():
        raise ValueError("dependency extraction destination already exists")
    destination.mkdir()
    records = []
    for name in selected:
        path = directory / archive_name(name, actual[name])
        if name == MODEL:
            shutil.copyfile(path, destination / name)
        else:
            target = destination
            if name != role:
                target = destination.joinpath(*name[len(role) + 2:].split("--"))
                target.mkdir(parents=True, exist_ok=True)
            with tarfile.open(path) as archive:
                archive.extractall(target, filter="data")
        records.append({"role": name, "archive": path.name, "sha256": actual[name]["sha256"]})
    record = {**binding, "role": role, "recipe_revision": revision, "inputs": records,
              "extraction_helper_sha256": digest(Path(__file__))}
    with (root / "dependency-source-consumption.jsonl").open("a") as output:
        output.write(json.dumps(record, sort_keys=True) + "\n")
    return record


def verify_consumption(root):
    verify_preparation(root)
    directory = root / "dependency-source-inputs"
    actual = verify_offers(directory)
    receipt = root / "dependency-source-consumption.jsonl"
    if receipt.is_symlink() or not receipt.is_file():
        raise ValueError("missing actual dependency consumption receipt")
    rows = [json.loads(line) for line in receipt.read_text().splitlines() if line]
    if not rows:
        raise ValueError("empty dependency consumption receipt")
    consumed = set()
    for row in rows:
        role = row["role"]
        binding = verify_recipe(root, row["recipe"], role)
        if row.get("recipe_sha256") != binding["recipe_sha256"] or row.get("extraction_helper_sha256") != digest(Path(__file__)):
            raise ValueError("consumed recipe/helper identity mismatch")
        if role not in actual:
            raise ValueError("unknown consumed dependency role")
        expected_revision = str(actual[role].get("commit", actual[role].get("revision", actual[role]["sha256"])))
        if row["recipe_revision"] != expected_revision and ALIASES.get(role) != row["recipe_revision"]:
            raise ValueError("consumed recipe revision mismatch")
        required = {role} | {name for name in actual if name.startswith(role + "--")}
        if role == "opus":
            required.add(MODEL)
        if {item["role"] for item in row["inputs"]} != required:
            raise ValueError("consumed dependency closure mismatch")
        for item in row["inputs"]:
            if item["archive"] != archive_name(item["role"], actual[item["role"]]) or item["sha256"] != actual[item["role"]]["sha256"]:
                raise ValueError("consumed archive identity mismatch")
            consumed.add(item["role"])
    required = set(actual)
    config = root / "source/config.h"
    if config.is_file() and "#define ARCH_X86_64 1" in config.read_text():
        required.discard("Ne10")
    if consumed != required:
        raise ValueError("actual consumed closure does not cover pinned build inputs")
    return {name: actual[name] for name in sorted(consumed)}


def audit_historical_sources(root, offers, logs):
    """Compare original source members; refuse unexplained historical edits."""
    actual = verify_offers(offers)
    changes = []
    generated_lame = {"aclocal.m4", "compile", "config.guess", "config.h.in", "config.rpath",
                      "config.sub", "configure", "depcomp", "install-sh", "ltmain.sh", "missing",
                      "m4/libtool.m4", "m4/ltoptions.m4", "m4/ltsugar.m4", "m4/ltversion.m4", "m4/lt~obsolete.m4"}
    for role, facts in actual.items():
        if facts["kind"] == "upstream-source-archive":
            continue
        target = root / "source/builder/build" / Path(*role.split("--"))
        with tarfile.open(offers / archive_name(role, facts)) as archive:
            for member in archive:
                if not member.isfile():
                    continue
                relative = Path(member.name)
                if relative.is_absolute() or ".." in relative.parts:
                    raise ValueError("unsafe original source member")
                path = target / relative
                if path.is_symlink() or not path.is_file():
                    raise ValueError("historical original source missing: " + role + "/" + member.name)
                original = archive.extractfile(member).read()
                current = path.read_bytes()
                if current == original:
                    continue
                transform = None
                if role == "libbluray" and member.name.startswith("src/libbluray/disc/") and member.name.endswith((".c", ".h")):
                    if current == original.replace(b"dec_init", b"libbluray_dec_init"):
                        transform = "pinned-libbluray-symbol-substitution"
                if role == "libbluray" and member.name == "src/meson.build" and current == original.replace(b"-DBLURAY_API_EXPORT", b"-DBLURAY_API_EXPORT_DISABLED"):
                    transform = "pinned-libbluray-export-substitution"
                if role == "libudfread" and member.name == "src/meson.build" and current == original.replace(b"-DUDFREAD_API_EXPORT", b"-DUDFREAD_API_EXPORT_DISABLED"):
                    transform = "pinned-libudfread-export-substitution"
                if role == "openssl" and member.name == "Configure":
                    expected = b"".join(line.rstrip(b"\n") + b'"apps",\n' if line.startswith(b"my @disablables =") else line for line in original.splitlines(keepends=True))
                    if current == expected:
                        transform = "pinned-openssl-disable-apps-substitution"
                if role == "lame" and (member.name in generated_lame or member.name.endswith("Makefile.in")) and "autoreconf -fi" in logs:
                    transform = "executed-lame-autoreconf-generated-output"
                if role == "zlib" and member.name in {"Makefile", "zconf.h"} and "./configure" in logs:
                    transform = "executed-zlib-configure-generated-output"
                if transform is None:
                    raise ValueError("unexplained historical source difference: " + role + "/" + member.name)
                changes.append({"role": role, "path": member.name, "transform": transform,
                                "original_sha256": hashlib.sha256(original).hexdigest(), "compiled_sha256": digest(path)})
    return changes


def retain_historical_build(root, evidence, lineage_path):
    """Truthful historical closure, never a future-helper consumption receipt."""
    lineage = json.loads(lineage_path.read_text())
    if lineage.get("mode") != "historical-archive-build":
        raise ValueError("historical lineage must identify its mode explicitly")
    offers = Path(lineage["offers"])
    verify_offers(offers)
    retained = evidence / "historical-build-lineage"
    retained.mkdir()
    log_text = ""
    files = lineage["executed_files"]
    if not files or not any(row["kind"] == "build-log" for row in files) or not any(row["kind"] == "extraction-helper" for row in files):
        raise ValueError("historical build requires actual logs and executed extraction helper")
    manifest = []
    for index, row in enumerate(files):
        path = Path(row["path"])
        if row["kind"] not in {"build-log", "extraction-helper", "transformation-helper", "tool-provenance"}:
            raise ValueError("unknown historical evidence kind")
        if path.is_symlink() or not path.is_file() or digest(path) != row["sha256"]:
            raise ValueError("historical executed evidence changed")
        name = str(index) + "-" + path.name
        shutil.copyfile(path, retained / name)
        manifest.append({"name": name, "kind": row["kind"], "sha256": row["sha256"]})
        if row["kind"] == "build-log":
            log_text += path.read_text(errors="replace")
    if "archive-source" not in log_text:
        raise ValueError("historical logs do not show archive reconstruction")
    differences = audit_historical_sources(root, offers, log_text)
    (retained / "manifest.json").write_text(json.dumps({"mode": lineage["mode"], "files": manifest, "original_source_differences": differences}, indent=2, sort_keys=True) + "\n")
    stage_offers(offers, evidence / "dependency-source")
    closure = retain_compiled_snapshot(root, evidence, Path(lineage["tool_source"]) if lineage.get("tool_source") else None)
    return {"mode": "historical-archive-build", "dependency_sources": json.loads(POLICY.read_text()),
            "historical_lineage_sha256": digest(retained / "manifest.json"), **closure}


def retain_archive_build(root, evidence):
    """Retain verified future-recipe inputs and the actual compiled source closure."""
    consumed = verify_consumption(root)
    offers = evidence / "dependency-source"
    stage_offers(root / "dependency-source-inputs", offers)
    recipes = evidence / "compiled-build-recipes"
    for name in ("dependency-recipe-bindings.json", "dependency-source-consumption.jsonl"):
        shutil.copyfile(root / name, recipes / name)
    shutil.copytree(root / "extraction-helpers", recipes / "extraction-helpers")
    return {"mode": "verified-archive-recipe-consumption", "dependency_sources": consumed,
            **retain_compiled_snapshot(root, evidence)}


def retain_compiled_snapshot(root, evidence, tool_source=None):
    tool_files = {}
    if tool_source is not None:
        pinned = {"automake": "23c091faee8dac047b0670d8c10da030bf104610fc064b904407de1a1ff8f437",
                  "libtool": "719ae9c4597d0198ee77da6bb09dfcb6f68ae0302330f1a1e61503648e59fdd2"}
        retained_tools = evidence / "source-generation-tools"
        retained_tools.mkdir()
        for name, expected in pinned.items():
            original = tool_source / "archives" / (name + ".tar.gz")
            if original.is_symlink() or digest(original) != expected:
                raise ValueError("source-generation tool archive mismatch")
            shutil.copyfile(original, retained_tools / original.name)
            with tarfile.open(original) as archive:
                for member in archive:
                    if member.isfile():
                        tool_files[str((tool_source / member.name).resolve())] = {"sha256": hashlib.sha256(archive.extractfile(member).read()).hexdigest(), "archive": original.name, "archive_sha256": expected, "member": member.name}

    snapshot = evidence / "compiled-source.tar.gz"
    index = {}
    # Keep generated C/headers, configuration, licenses and resources as well as
    # original sources. Omit native objects/binaries and repository metadata.
    with tarfile.open(snapshot, "w:gz") as archive:
        for path in sorted((root / "source").rglob("*")):
            relative = path.relative_to(root / "source")
            if any(part in {".git", ".svn"} for part in relative.parts):
                continue
            if path.suffix in {".o", ".a", ".dylib", ".so", ".d", ".pyc"}:
                continue
            if str(relative) in {"ffmpeg", "ffprobe", "builder/ffmpeg", "builder/ffprobe"}:
                continue
            if path.is_symlink():
                resolved = path.resolve()
                if not resolved.is_relative_to((root / "source").resolve()):
                    if str(resolved) not in tool_files or not resolved.is_file() or digest(resolved) != tool_files[str(resolved)]["sha256"]:
                        raise ValueError("compiled source symlink escapes verified source-generation tools")
                    # Retain actual tool-provided source as a regular member;
                    # no task-local absolute symlink survives redistribution.
                    info = archive.gettarinfo(str(resolved), arcname=str(relative))
                    with resolved.open("rb") as content:
                        archive.addfile(info, content)
                    index[str(relative)] = {**tool_files[str(resolved)], "source": "verified-source-generation-tool", "original_symlink_target": tool_files[str(resolved)]["member"]}
                else:
                    archive.add(path, arcname=str(relative), recursive=False)
                    index[str(relative)] = {"symlink": str(path.readlink())}
            elif path.is_file():
                index[str(relative)] = {"sha256": digest(path)}
                archive.add(path, arcname=str(relative), recursive=False)
                if digest(path) != index[str(relative)]["sha256"]:
                    raise ValueError("compiled source changed while retaining snapshot")
    (evidence / "compiled-source-index.json").write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")
    return {"compiled_source_sha256": digest(snapshot),
            "compiled_source_index_sha256": digest(evidence / "compiled-source-index.json")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--upstream", action="store_true")
    parser.add_argument("--recipe", required=True)
    parser.add_argument("role", nargs="?")
    parser.add_argument("revision", nargs="?")
    parser.add_argument("destination", type=Path, nargs="?")
    args = parser.parse_args()
    if args.upstream:
        if not args.role or args.revision or args.destination:
            parser.error("upstream copy does not take extraction arguments")
        record = copy_upstream(args.root, args.role, args.recipe)
    else:
        if not all((args.role, args.revision, args.destination)):
            parser.error("source extraction requires role, revision and destination")
        record = extract_source(args.root, args.role, args.revision, args.destination, args.recipe)
    print(json.dumps(record, sort_keys=True))


if __name__ == "__main__":
    main()
