"""Build a relocatable arm64 macOS DV helper bundle from explicit local inputs."""

import argparse
import hashlib
from collections import deque
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

SOURCE = Path(__file__).resolve().parent
TOOLS = ("segment_decode_render", "scan_title", "mux_rgb", "author_p81")
FFMPEG = ("avformat", "avcodec", "avutil")
LOAD_COMMANDS = {
    "LC_LOAD_DYLIB", "LC_LOAD_WEAK_DYLIB", "LC_REEXPORT_DYLIB",
    "LC_LOAD_UPWARD_DYLIB", "LC_LAZY_LOAD_DYLIB", "LC_LOAD_DYLINKER",
}
ABI_PROBE = r"""
#include <stdio.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libplacebo/config.h>
int main(void) {
    printf("{\"libplacebo\":%u,\"avcodec\":%u,\"avformat\":%u,\"avutil\":%u}\n", PL_API_VER,
        avcodec_version() >> 16, avformat_version() >> 16, avutil_version() >> 16);
    return 0;
}
"""


def run(*args, env=None):
    return subprocess.run(args, env=env, check=True, text=True, stdout=subprocess.PIPE).stdout


def macho_links(image):
    """Read every dylib load command, install ID and rpath in the arm64 slice."""
    output = run("otool", "-arch", "arm64", "-l", image)
    commands = re.findall(r"\bcmd (LC_\w+)\n\s*cmdsize \d+\n\s*(?:name|path) (.*?) \(offset \d+\)", output)
    # otool may describe an unsupported input on stdout without failing.
    if "Load command 0" not in output:
        raise ValueError(f"expected an arm64 Mach-O image: {image}")
    link_commands = LOAD_COMMANDS | {"LC_RPATH", "LC_ID_DYLIB"}
    for command in link_commands:
        if len(re.findall(rf"^\s*cmd {command}$", output, re.M)) != sum(
            kind == command for kind, _ in commands
        ):
            raise ValueError(f"cannot parse {command} in {image}")
    return (
        [value for command, value in commands if command in LOAD_COMMANDS],
        [value for command, value in commands if command == "LC_RPATH"],
        [value for command, value in commands if command == "LC_ID_DYLIB"],
    )


def system_link(name):
    return name.startswith(("/usr/lib/", "/System/Library/")) and ".." not in Path(name).parts


def expand_loader_path(name, image, executable):
    for token, directory in (("@loader_path", image.parent), ("@executable_path", executable.parent)):
        if name.startswith(token + "/"):
            return directory / name[len(token) + 1:]
    path = Path(name)
    if path.is_absolute():
        return path
    raise ValueError(f"unsupported Mach-O path {name!r} in {image}")


def resolve_link(name, image, executable, runpaths, prefixes, pins):
    if name.startswith("@rpath/"):
        candidates = [root / name[len("@rpath/"):] for root in runpaths]
    else:
        candidates = [expand_loader_path(name, image, executable)]
    resolved = {path.resolve() for path in candidates if path.is_file()}
    if len(resolved) != 1:
        raise ValueError(f"unresolved or ambiguous Mach-O dependency {name!r} in {image}: {resolved}")
    path = resolved.pop()
    if not path.name.endswith(".dylib"):
        raise ValueError(f"expected a dylib: {path}")
    if pins is not None:
        if path not in pins:
            raise ValueError(f"M7 Mach-O dependency is outside the exact encoder pin: {image} -> {path}")
    else:
        if not any(path.is_relative_to(root) for root in prefixes):
            raise ValueError(f"Mach-O dependency outside supplied prefixes: {image} -> {path}")
        if re.match(r"libav(?:codec|format|util)[.]", path.name):
            raise ValueError(f"static FFmpeg unexpectedly linked a dynamic library: {path}")
    return path


def dependency_graph(executables, prefixes, pins=None):
    graph = {}
    library_names = {}
    visited = set()
    pending = deque((image, image, ()) for image in executables)
    while pending:
        image, executable, inherited = pending.popleft()
        names, rpaths, _ = macho_links(image)
        search = tuple(dict.fromkeys([*(expand_loader_path(r, image, executable) for r in rpaths), *inherited]))
        state = (image, executable, search)
        if state in visited:
            continue
        visited.add(state)
        edges = {}
        for name in names:
            if system_link(name):
                continue
            dependency = resolve_link(name, image, executable, search, prefixes, pins)
            previous = library_names.setdefault(dependency.name, dependency)
            if previous != dependency:
                raise ValueError(f"conflicting dylib basename: {previous} and {dependency}")
            if len(library_names) > 32:
                raise ValueError("bundle exceeds the daemon's 32-library manifest limit")
            edges[name] = dependency
            pending.append((dependency, executable, search))
        if graph.setdefault(image, edges) != edges:
            raise ValueError(f"Mach-O dependency changes across executable contexts: {image}")
    return graph, library_names


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def copy_macho(source, target, expected=None):
    archs = run("lipo", "-archs", source).split()
    if expected is not None and archs != ["arm64"]:
        raise ValueError(f"M7 pinned artifact must be arm64: {source}")
    if archs == ["arm64"]:
        shutil.copy2(source, target)
    else:
        run("lipo", source, "-thin", "arm64", "-output", target)
    if expected is not None and sha(target) != expected:
        raise ValueError(f"copied M7 artifact does not match the exact encoder pin: {source}")
    target.chmod(0o755)


def relocate(root, executables, graph, libraries, *, library_dir="lib", copies=None, pins=None):
    lib_dir = root / library_dir
    lib_dir.mkdir(parents=True)
    installed = {image: image for image in executables}
    for source, target in (copies or {}).items():
        copy_macho(source, target, pins[source])
        installed[source] = target
    for name, source in libraries.items():
        copy_macho(source, lib_dir / name, None if pins is None else pins[source])
    for src, edges in graph.items():
        image = installed.get(src, lib_dir / src.name)
        _, rpaths, ids = macho_links(image)
        args = ["install_name_tool"]
        for old, dependency in edges.items():
            relative = os.path.relpath(lib_dir / dependency.name, image.parent)
            args += ["-change", old, f"@loader_path/{relative}"]
        if image.parent == lib_dir:
            if len(ids) != 1:
                raise ValueError(f"expected exactly one install identity in {src}")
            args += ["-id", f"@loader_path/{image.name}"]
        for rpath in rpaths:
            args += ["-delete_rpath", rpath]
        if len(args) > 1:
            run(*args, image)
        run("codesign", "--force", "--sign", "-", "--timestamp=none", image)
        run("codesign", "--verify", "--strict", image)

    receipt = {}
    for image in [*installed.values(), *(lib_dir / name for name in sorted(libraries))]:
        names, rpaths, ids = macho_links(image)
        expected_id = [f"@loader_path/{image.name}"] if image.parent == lib_dir else []
        if rpaths or ids != expected_id:
            raise ValueError(f"unexpected post-relocation load commands in {image}")
        for name in names:
            if system_link(name):
                continue
            target = expand_loader_path(name, image, image).resolve(strict=True)
            if not name.startswith("@loader_path/") or target.parent != lib_dir or target.name not in libraries:
                raise ValueError(f"dependency escaped the installed bundle: {image} -> {name}")
        receipt[image.relative_to(root).as_posix()] = names
    return receipt


def pkg_config_path(prefixes):
    directories = []
    for prefix in prefixes:
        for relative in ("lib/pkgconfig", "share/pkgconfig"):
            directory = prefix / relative
            if directory.is_dir():
                directory = directory.resolve()
                if not directory.is_relative_to(prefix):
                    raise ValueError(f"pkg-config directory escaped supplied prefix: {directory}")
                directories.append(directory)
    return os.pathsep.join(str(path) for path in dict.fromkeys(directories))


def pkg_config(search_path, *args):
    env = dict(os.environ, PKG_CONFIG_PATH="", PKG_CONFIG_LIBDIR=search_path)
    return run("pkg-config", *args, env=env).strip()


def read_encoder_pin(path, prefix):
    # The native comparison receipt already records the independent CLI and
    # every non-system dylib it used. Consume only those input identities.
    if path.stat().st_size > 1024 * 1024:
        raise ValueError("M7 encoder pin exceeds its size bound")
    data = json.loads(path.read_bytes())
    try:
        if type(data["schema"]) is not int or data["schema"] != 1:
            raise ValueError("unsupported M7 encoder pin schema")
        cli_artifact = data["artifacts"]["ffmpeg"]
        closure = data["encoder_closure"]
        if not isinstance(closure, list) or not 1 <= len(closure) <= 32:
            raise ValueError("invalid M7 encoder closure")
        pins = {}
        for artifact in [cli_artifact, *closure]:
            source = Path(artifact["path"])
            digest = artifact["sha256"]
            if not source.is_absolute() or not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise ValueError("invalid M7 encoder artifact reference")
            resolved = source.resolve(strict=True)
            if resolved in pins or not resolved.is_file() or sha(resolved) != digest:
                raise ValueError(f"M7 encoder artifact differs from its pin: {resolved}")
            pins[resolved] = digest
    except (KeyError, TypeError) as error:
        raise ValueError("invalid M7 encoder pin") from error
    cli = Path(cli_artifact["path"]).resolve(strict=True)
    if cli != (prefix / "bin/ffmpeg").resolve(strict=True) or not cli.is_relative_to(prefix):
        raise ValueError("M7 CLI is outside the supplied encoder prefix")
    libraries = {path: digest for path, digest in pins.items() if path != cli}
    required = ("libavcodec.63", "libavformat.63", "libavutil.61", "libx265.217")
    if any(sum(re.fullmatch(rf"{re.escape(stem)}(?:\.\d+)*\.dylib", path.name) is not None
               for path in libraries) != 1 for stem in required):
        raise ValueError("M7 encoder pin lacks its exact shared libav/x265 ABI")
    return cli, pins, libraries


def package_encoder(root, muxer, cli, pins, libraries):
    executables = [muxer, cli]
    graph, by_name = dependency_graph(executables, (), libraries)
    if set(by_name.values()) != set(libraries):
        raise ValueError("built M7 dependency graph differs from the complete frozen closure")
    direct = set(graph[muxer].values())
    for family in ("libavcodec.", "libavformat.", "libavutil."):
        if not any(path.name.startswith(family) for path in direct):
            raise ValueError(f"M7 muxer did not link its shared {family[:-1]}")
    return relocate(root, executables, graph, by_name, library_dir="encoder/lib",
                    copies={cli: root / "encoder/ffmpeg"}, pins=pins)


def build(args):
    ffmpeg, dovi, placebo, provenance = (p.resolve(strict=True) for p in
                                         (args.ffmpeg, args.dovi, args.placebo, args.provenance))
    dependencies = tuple(p.resolve(strict=True) for p in args.dependency_prefix)
    prefixes = (ffmpeg, dovi, placebo, *dependencies)
    input_prefixes = prefixes
    encoder_prefix = args.m7_encoder_prefix
    encoder = None
    if encoder_prefix is not None:
        encoder_prefix = encoder_prefix.resolve(strict=True)
        if any(encoder_prefix.is_relative_to(prefix) or prefix.is_relative_to(encoder_prefix)
               for prefix in (ffmpeg, dovi, placebo)):
            raise ValueError("M7 shared and decoder static prefixes must be separate")
        encoder = read_encoder_pin(args.m7_encoder_pin, encoder_prefix)
        input_prefixes = (*prefixes, encoder_prefix, *(path.parent for path in encoder[2]))
    root = args.output.resolve()
    if root.is_relative_to(provenance) or any(root.is_relative_to(prefix) for prefix in input_prefixes):
        raise ValueError("OUTPUT must be separate from dependency and provenance inputs")
    if not (provenance / "sources").is_dir() or not (provenance / "licenses").is_dir():
        raise ValueError("PROVENANCE must contain sources/ and licenses/ for supplied components")
    for directory in ("sources", "licenses"):
        entries = list((provenance / directory).rglob("*"))
        if not any(p.is_file() for p in entries) or any(p.is_symlink() for p in entries):
            raise ValueError(f"provenance {directory}/ must contain regular files without symlinks")

    ffmpeg_search = pkg_config_path((ffmpeg,))
    placebo_search = pkg_config_path((placebo, *dependencies))
    ffmpeg_modules = [f"lib{name}" for name in FFMPEG]
    ffmpeg_libs = []
    for module in ffmpeg_modules:
        lib_dir = Path(pkg_config(ffmpeg_search, "--variable=libdir", module)).resolve(strict=True)
        archive = (lib_dir / f"{module}.a").resolve(strict=True)
        if not archive.is_relative_to(ffmpeg):
            raise ValueError(f"static FFmpeg library escaped FFMPEG: {archive}")
        ffmpeg_libs.append(archive)
    placebo_pc = placebo / "lib/pkgconfig"
    placebo_lib_dir = Path(pkg_config(placebo_search, "--variable=libdir", "libplacebo")).resolve(strict=True)
    if not placebo_lib_dir.is_relative_to(placebo):
        raise ValueError("libplacebo pkg-config libdir escaped PLACEBO")

    root.mkdir()
    sources_dir = root / "sources"
    shutil.copytree(provenance / "sources", sources_dir / "upstream")
    shutil.copytree(provenance / "licenses", root / "licenses" / "upstream")
    project_notices = root / "licenses/plurx"
    project_notices.mkdir()
    for name in ("LICENSE", "NOTICE"):
        shutil.copy2(SOURCE.parents[1] / name, project_notices)
    for path in sorted(SOURCE.iterdir()):
        if path.suffix in {".c", ".h", ".patch", ".sh", ".py"} or path.name == "build-requirements.txt":
            shutil.copy2(path, sources_dir)
    shutil.copy2(dovi / "lib/libdovi.a", sources_dir)
    shutil.copy2(dovi / "include/libdovi/rpu_parser.h", sources_dir)
    static = sources_dir / "ffmpeg-static-libraries"
    static.mkdir()
    for archive in ffmpeg_libs:
        shutil.copy2(archive, static)

    env = {name: value for name, value in os.environ.items() if not name.startswith("DYLD_")}
    env.update(PKG_CONFIG_LIBDIR=pkg_config_path((ffmpeg, placebo, *dependencies)),
               CFLAGS="-O2 -fno-math-errno -arch arm64 -Wl,-headerpad_max_install_names")
    build_args = ["sh", SOURCE / "build.sh", ffmpeg, placebo_pc, dovi, root / "bin"]
    if encoder is not None:
        build_args.append(encoder_prefix)
    run(*build_args, env=env)
    executables = [root / "bin" / name for name in TOOLS
                   if encoder is None or name != "mux_rgb"]
    graph, libraries = dependency_graph(executables, prefixes)
    renderer_libraries = set(graph[root / "bin/segment_decode_render"].values())
    placebo_abi = [path for path in renderer_libraries
                   if re.fullmatch(r"libplacebo\.387(?:\.\d+)*\.dylib", path.name)]
    if len(placebo_abi) != 1 or not placebo_abi[0].is_relative_to(placebo_lib_dir):
        raise ValueError("the renderer must link to ABI 387 libplacebo from PLACEBO")
    receipt = relocate(root, executables, graph, libraries)
    if encoder is not None:
        cli, pins, encoder_libraries = encoder
        receipt.update(package_encoder(root, root / "bin/mux_rgb", cli, pins, encoder_libraries))
        # Pin identities belong to the original inputs. The checked runtime
        # manifest hashes the independently relocated, signed artifacts.
        (sources_dir / "m7-encoder-inputs.json").write_text(json.dumps({
            "schema": 1,
            "cli_sha256": pins[cli],
            "libraries": {path.name: digest for path, digest in sorted(encoder_libraries.items())},
        }, indent=2) + "\n")

    probe = sources_dir / "abi.c"
    probe.write_text(ABI_PROBE)
    probe_bin = sources_dir / "abi"
    ffmpeg_flags = shlex.split(pkg_config(ffmpeg_search, "--static", "--cflags", "--libs", *ffmpeg_modules))
    placebo_flags = shlex.split(pkg_config(placebo_search, "--cflags", "libplacebo"))
    run(env.get("CC") or "cc", "-arch", "arm64", "-std=c11", "-Wall", "-Wextra", "-Werror", probe,
        "-o", probe_bin, *ffmpeg_flags, *placebo_flags, "-lm", "-lpthread", "-ldl", env=env)
    if not all(system_link(name) for name in macho_links(probe_bin)[0]):
        raise ValueError("ABI probe linked a non-system dylib")
    abi = json.loads(run(probe_bin, env=env))
    probe_bin.unlink()
    (sources_dir / "abi.json").write_text(json.dumps(abi, indent=2) + "\n")
    dependencies = {"original_libraries": {name: str(path) for name, path in sorted(libraries.items())},
                    "relocated_links": receipt}
    (sources_dir / "runtime-dependencies.json").write_text(json.dumps(dependencies, indent=2) + "\n")
    manifest_args = [sys.executable, SOURCE / "bundle-manifest.py", SOURCE, root, "--darwin"]
    if encoder is not None:
        manifest_args.append("--m7-encoder")
    print(run(*manifest_args), end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ffmpeg", type=Path, help="static FFmpeg install prefix")
    parser.add_argument("dovi", type=Path, help="libdovi archive and include prefix")
    parser.add_argument("placebo", type=Path, help="libplacebo 387 dynamic install prefix")
    parser.add_argument("output", type=Path, help="new bundle root")
    parser.add_argument("--dependency-prefix", type=Path, action="append", default=[],
                        help="permitted transitive dylib prefix (repeatable)")
    parser.add_argument("--provenance", type=Path, required=True,
                        help="sources/ and licenses/ for all supplied decoder and optional M7 encoder components")
    parser.add_argument("--m7-encoder-prefix", type=Path,
                        help="separate shared libav/x265 installation with the pinned ffmpeg CLI")
    parser.add_argument("--m7-encoder-pin", type=Path,
                        help="native encoder receipt containing the original CLI and complete dylib closure")
    args = parser.parse_args()
    if (args.m7_encoder_prefix is None) != (args.m7_encoder_pin is None):
        parser.error("M7 encoder prefix and pin must be supplied together")
    paths = [args.ffmpeg, args.dovi, args.placebo, args.output, args.provenance, *args.dependency_prefix]
    if args.m7_encoder_prefix is not None:
        paths += [args.m7_encoder_prefix, args.m7_encoder_pin]
    if sys.platform != "darwin" or not all(path.is_absolute() for path in paths):
        parser.error("requires macOS and absolute paths")
    try:
        build(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"macOS DV bundle: {error}\n")


if __name__ == "__main__":
    main()
