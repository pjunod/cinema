"""Build a relocatable arm64 macOS DV helper bundle from explicit local inputs."""

import argparse
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


def resolve_link(name, image, executable, runpaths, prefixes):
    if name.startswith("@rpath/"):
        candidates = [root / name[len("@rpath/"):] for root in runpaths]
    else:
        candidates = [expand_loader_path(name, image, executable)]
    resolved = {path.resolve() for path in candidates if path.is_file()}
    if len(resolved) != 1:
        raise ValueError(f"unresolved or ambiguous Mach-O dependency {name!r} in {image}: {resolved}")
    path = resolved.pop()
    if not any(path.is_relative_to(root) for root in prefixes):
        raise ValueError(f"Mach-O dependency outside supplied prefixes: {image} -> {path}")
    if not path.name.endswith(".dylib"):
        raise ValueError(f"expected a dylib: {path}")
    if re.match(r"libav(?:codec|format|util)[.]", path.name):
        raise ValueError(f"static FFmpeg unexpectedly linked a dynamic library: {path}")
    return path


def dependency_graph(executables, prefixes):
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
            dependency = resolve_link(name, image, executable, search, prefixes)
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


def relocate(root, executables, graph, libraries):
    lib_dir = root / "lib"
    lib_dir.mkdir()
    for name, src in libraries.items():
        target = lib_dir / name
        if run("lipo", "-archs", src).split() == ["arm64"]:
            shutil.copy2(src, target)
        else:
            run("lipo", src, "-thin", "arm64", "-output", target)
        target.chmod(0o755)
    for src, edges in graph.items():
        image = src if src in executables else lib_dir / src.name
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
    for image in [*executables, *(lib_dir / name for name in sorted(libraries))]:
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


def build(args):
    ffmpeg, dovi, placebo, provenance = (p.resolve(strict=True) for p in
                                         (args.ffmpeg, args.dovi, args.placebo, args.provenance))
    dependencies = tuple(p.resolve(strict=True) for p in args.dependency_prefix)
    prefixes = (ffmpeg, dovi, placebo, *dependencies)
    root = args.output.resolve()
    if root.is_relative_to(provenance) or any(root.is_relative_to(prefix) for prefix in prefixes):
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
    run("sh", SOURCE / "build.sh", ffmpeg, placebo_pc, dovi, root / "bin", env=env)
    executables = [root / "bin" / name for name in TOOLS]
    graph, libraries = dependency_graph(executables, prefixes)
    renderer_libraries = set(graph[root / "bin/segment_decode_render"].values())
    placebo_abi = [path for path in renderer_libraries
                   if re.fullmatch(r"libplacebo\.387(?:\.\d+)*\.dylib", path.name)]
    if len(placebo_abi) != 1 or not placebo_abi[0].is_relative_to(placebo_lib_dir):
        raise ValueError("the renderer must link to ABI 387 libplacebo from PLACEBO")
    receipt = relocate(root, executables, graph, libraries)

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
    print(run(sys.executable, SOURCE / "bundle-manifest.py", SOURCE, root, "--darwin"), end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ffmpeg", type=Path, help="static FFmpeg install prefix")
    parser.add_argument("dovi", type=Path, help="libdovi archive and include prefix")
    parser.add_argument("placebo", type=Path, help="libplacebo 387 dynamic install prefix")
    parser.add_argument("output", type=Path, help="new bundle root")
    parser.add_argument("--dependency-prefix", type=Path, action="append", default=[],
                        help="permitted transitive dylib prefix (repeatable)")
    parser.add_argument("--provenance", type=Path, required=True,
                        help="directory of source archives and licensing notices under sources/ and licenses/")
    args = parser.parse_args()
    paths = [args.ffmpeg, args.dovi, args.placebo, args.output, args.provenance, *args.dependency_prefix]
    if sys.platform != "darwin" or not all(path.is_absolute() for path in paths):
        parser.error("requires macOS and absolute paths")
    try:
        build(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"macOS DV bundle: {error}\n")


if __name__ == "__main__":
    main()
