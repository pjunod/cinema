"""The shipping producer/parser relation binds bytes and the entire source union."""
import importlib.machinery
import importlib.util
import json
import subprocess
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


def recipe():
    path = Path(__file__).resolve().parents[2] / "scripts/build-linux-dolby-ffmpeg"
    loader = importlib.machinery.SourceFileLoader("linux_dolby_package", str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class LinuxDolbyPackageCase(unittest.TestCase):
    def test_provenance_links_are_metadata_and_never_dereferenced(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "original"
            source.mkdir()
            (source / "recipe").write_bytes(b"genuine source")
            (source / "recipe").chmod(0o755)
            (source / "unbuilt.so").symlink_to("unbuilt.so.1")
            host = root / "host-secret"
            host.write_bytes(b"must not copy")
            (source / "tool-alias").symlink_to(host)
            links = module.copy_provenance_bytes(source, root / "emitted")
            self.assertEqual(links["links"], {"unbuilt.so": "unbuilt.so.1", "tool-alias": str(host)})
            self.assertEqual(list((root / "emitted").iterdir()), [root / "emitted/recipe"])
            self.assertEqual((root / "emitted/recipe").read_bytes(), b"genuine source")
            self.assertEqual((root / "emitted/recipe").stat().st_mode & 0o777, 0o755)
            import os
            os.mkfifo(source / "special")
            with self.assertRaisesRegex(ValueError, "special file"):
                module.copy_provenance_bytes(source, root / "refused")

    def test_assembly_retains_original_recipe_and_refuses_build_witness_tampering(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = root / "original-recipe"
            old.write_bytes(b"actual old compiler recipe")
            steps = {}
            for step in ["configure", "compile", "install"]:
                evidence = root / "build-evidence" / step / "attempt-1"
                evidence.mkdir(parents=True)
                log = evidence / "output.log"
                log.write_bytes(step.encode())
                steps[step] = {"status": 0, "log_name": "output.log", "log_sha256": module.sha(log),
                               "environment": {"HOME": str(evidence)}}
            facts = {"state": "installed", "recipe_sha256": module.sha(old), "executed_steps": steps}
            state = root / "build-state.json"
            state.write_text(json.dumps(facts))
            self.assertEqual(module.installed_build_facts(root, old), (facts, old))
            wrong = root / "different-recipe"
            wrong.write_bytes(b"different")
            with self.assertRaisesRegex(ValueError, "recipe differs"):
                module.installed_build_facts(root, wrong)
            facts["state"] = "compiled"
            state.write_text(json.dumps(facts))
            with self.assertRaisesRegex(ValueError, "three successful"):
                module.installed_build_facts(root, old)
            facts["state"] = "installed"
            state.write_text(json.dumps(facts))
            (root / "build-evidence/compile/attempt-1/output.log").write_bytes(b"changed witness")
            with self.assertRaisesRegex(ValueError, "witness log changed"):
                module.installed_build_facts(root, old)
            with self.assertRaisesRegex(ValueError, "exact prepared recipe"):
                module.read_facts(root)

    def test_actual_elf_version_requirements_keep_library_and_weak_association(self):
        module = recipe()
        text = """Version symbols section '.gnu.version' contains 3 entries:
  000: 0 (*local*) 1 (*global*) 2 (GLIBC_2.36)
Version definition section '.gnu.version_d' contains 2 entries:
  0x001c: Rev: 1  Flags: none  Index: 2  Cnt: 1  Name: GLIBC_2.36
Version needs section '.gnu.version_r' contains 1 entry:
  0x0010: Version: 1 File: libc.so.6 Cnt: 2
  0x0020: Name: GLIBC_2.36  Flags: none  Version: 2
  0x0030: Name: OPTIONAL_VERSION  Flags: WEAK  Version: 3
"""
        definitions, needs = module.elf_versions(text)
        self.assertEqual(definitions, ["GLIBC_2.36"])
        self.assertEqual(needs, {"libc.so.6": ["GLIBC_2.36"]})
        with self.assertRaisesRegex(ValueError, "lacks its library"):
            module.elf_versions("Version needs section '.gnu.version_r' contains 1 entry:\n"
                                "  Name: GLIBC_2.36  Flags: none  Version: 2\n")

    def test_sealed_parser_requires_actual_bytes_and_identical_source_union(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "sealed-ffprobe"
            binary.write_bytes(b"actual test parser bytes")
            (root / "config.h").write_bytes(b"actual resolved configuration")
            (root / "config_components.h").write_bytes(b"actual decoder component configuration")
            (root / "build-static-ffprobe").write_bytes(b"actual recipe")
            source = {"source_sha256": "a" * 64, "source_commit": "b" * 40,
                      "local_patch_sha256": "c" * 64, "upstream_quilt": {"one.patch": "d" * 64}}
            union = {"archive_sha256": source["source_sha256"], "commit": source["source_commit"],
                     "local_patch_sha256": source["local_patch_sha256"],
                     "quilt_digest": module.canonical_digest(source["upstream_quilt"])}
            sealed = {"schema_version": 1, "kind": "sealed-source-parser", "source": union,
                      "executable_sha256": module.sha(binary),
                      "configuration_sha256": module.sha(root / "config.h"),
                      "components_sha256": module.sha(root / "config_components.h"),
                      "build_recipe_sha256": module.sha(root / "build-static-ffprobe")}
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps(sealed))
            self.assertEqual(module.validate_parser_union(source, binary, root), union)
            for field in union:
                changed = json.loads(json.dumps(sealed))
                changed["source"][field] = "0" * len(union[field])
                manifest.write_text(json.dumps(changed))
                with self.subTest(field=field), self.assertRaisesRegex(ValueError, "source union"):
                    module.validate_parser_union(source, binary, root)
            manifest.write_text(json.dumps(sealed))
            for path in [binary, root / "config.h", root / "config_components.h", root / "build-static-ffprobe"]:
                original = path.read_bytes()
                path.write_bytes(original + b"tampered")
                with self.subTest(path=path.name), self.assertRaisesRegex(ValueError, "actual bytes"):
                    module.validate_parser_union(source, binary, root)
                path.write_bytes(original)

    def test_build_preserves_official_features_and_assembly_cannot_claim_elf_admission(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source/ffbuild").mkdir(parents=True)
            (root / "source/config.h").write_text("resolved actual configuration")
            (root / "source/ffbuild/config.mak").write_text("resolved actual make configuration")
            (root / "source/Dockerfile.in").write_text("original authenticated environment fixture")
            flags = ["--prefix=/usr/lib/jellyfin-ffmpeg", "--enable-shared", "--enable-libx265",
                     "--enable-libass", "--enable-cuda-llvm", "--enable-vulkan-static"]
            facts = {"state": "source-prepared", "sdk": str(root / "sdk"), "sdk_digest": "a" * 64,
                     "preserved_configure": flags, "recipe_sha256": module.sha(Path(module.__file__)),
                     "environment_recipe_sha256": module.sha(root / "source/Dockerfile.in"),
                     "executed_steps": {}}
            (root / "build-state.json").write_text(json.dumps(facts))
            calls, selected_tools = [], []
            # Pinned Jellyfin configure (source 87bedce..., commit 253db2...)
            # initializes lower-case defaults, parses CMDLINE_SET tool options,
            # then applies set_default. These actual option-assignment/default
            # fragments reproduce that consumer; unrelated feature parsing is
            # outside this focused fixture. Upper-case tool env is not read.
            tool_parser = r"""
set_default(){
    for opt; do
        eval : \${$opt:=\$${opt}_default}
    done
}
cc_default="gcc"
cxx_default="g++"
pkg_config_default=pkg-config
for opt do
    optval="${opt#*=}"
    case "$opt" in
        --cc=*|--cxx=*|--pkg-config=*)
            optname="${opt%%=*}"
            optname="${optname#--}"
            optname=$(echo "$optname" | sed 's/-/_/g')
            eval $optname='$optval'
        ;;
    esac
done
set_default cc cxx pkg_config
printf '%s\n' "$cc" "$cxx" "$pkg_config"
"""
            expected_tools = []
            def capture(argv, cwd, environment, log, deadline):
                calls.append(argv)
                expected_tools.extend(environment[name] for name in ["CC", "CXX", "PKG_CONFIG"])
                baseline = subprocess.check_output(["/bin/sh", "-c", tool_parser],
                    env=environment, text=True, timeout=10).splitlines()
                assert baseline == ["gcc", "g++", "pkg-config"]
                selected_tools.extend(subprocess.check_output(
                    ["/bin/sh", "-c", tool_parser, "configure", *argv[1:]],
                    env=environment, text=True, timeout=10).splitlines())
                return {"argv": argv, "status": 0}
            helper = SimpleNamespace(verified_staging=lambda path: {"sdk_digest": "a" * 64,
                "generator_tool_versions": {name: {"sha256": "e" * 64} for name in
                    ["gcc", "g++", "make", "pkg-config"]}},
                                     bounded_generator=capture)
            actual_sha = module.sha
            def tool_or_file_sha(path):
                return "e" * 64 if path.parent == Path("/usr/bin") else actual_sha(path)
            with patch.object(module, "host"), patch.object(module, "load", return_value=helper), \
                    patch.object(module, "sha", side_effect=tool_or_file_sha):
                result = module.build_step(root, "configure", 2, 10)
            self.assertEqual(calls[0][1:1 + len(flags)], flags)
            self.assertEqual(selected_tools, expected_tools)
            self.assertEqual(result["state"], "configured")
            self.assertFalse((root / "provenance/linux-dolby-package.json").exists())

    def test_elf_admission_refuses_changed_distributed_executable_before_inspection(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "provenance").mkdir()
            for name in ["ffmpeg", "ffprobe", "sealed-ffprobe"]:
                (root / name).write_bytes(b"actual distinct executable " + name.encode())
            facts = {"executables": {"ffmpeg_sha256": module.sha(root / "ffmpeg"),
                     "ffprobe_sha256": module.sha(root / "ffprobe"),
                     "sealed_parser_sha256": module.sha(root / "sealed-ffprobe")}, "distributed_objects": {}}
            (root / "provenance/pending-linux-dolby-package.json").write_text(json.dumps(facts))
            (root / "ffmpeg").write_bytes(b"swapped")
            with patch.object(module, "host"), patch.object(module, "load") as loader:
                with self.assertRaisesRegex(ValueError, "executable changed"):
                    module.verify_closure(root, 10)
                loader.assert_not_called()
            self.assertFalse((root / "provenance/linux-dolby-package.json").exists())


def test_normal_docker_caller_requires_audited_amd64_package_and_keeps_arm_incumbent():
    root = Path(__file__).resolve().parents[2]
    make = (root / "Makefile").read_text()
    docker = (root / "Dockerfile").read_text()
    assert "PLURX_LINUX_DOLBY_PACKAGE" in make
    assert "--step docker-build" in make
    assert "--target runtime --platform linux/arm64" in make
    assert "FROM runtime AS runtime-dolby-amd64" in docker
    assert "--mount=from=linux-dolby-package" in docker
    assert "--step install-shipping" in docker


def test_docker_install_tools_are_temporary_and_build_identity_arguments_survive():
    root = Path(__file__).resolve().parents[2]
    docker = (root / "Dockerfile").read_text()
    installer = docker.split("FROM runtime-assets AS linux-dolby-install", 1)[1].split("FROM runtime AS runtime-dolby-amd64", 1)[0]
    final = docker.split("FROM runtime AS runtime-dolby-amd64", 1)[1].split("FROM runtime AS default-runtime", 1)[0]
    assert installer.index("USER root") < installer.index("--step install-shipping")
    assert "apt-get install -y --no-install-recommends python3" in installer
    assert "COPY --from=linux-dolby-install /usr/lib/jellyfin-ffmpeg" in final
    assert final.index("USER root") < final.index("COPY --from=") < final.index("USER plurx")
    assert "python3" not in final
    make = (root / "Makefile").read_text()
    for option, variable in [("plurx-build-ref", "BUILD_REF"), ("plurx-build-sha", "BUILD_SHA"), ("source-date-epoch", "SOURCE_DATE_EPOCH")]:
        assert '--' + option + ' "$(' + variable + ')"' in make
    builder = (root / "scripts/build-linux-dolby-ffmpeg").read_text()
    assert '*build_arguments' in builder
    assert 'argument + "=" + getattr(args, argument.lower())' in builder


def test_package_export_observes_official_configuration_and_reuses_only_matched_parser():
    root = Path(__file__).resolve().parents[2]
    builder = (root / "scripts/build-linux-dolby-ffmpeg").read_text()
    pipeline = builder.split("def package_pipeline", 1)[1].split("def main", 1)[0]
    assert 'helper.package_members' in pipeline
    assert 'official configuration reporter differs from authenticated package' in pipeline
    assert '["/usr/lib/jellyfin-ffmpeg/ffmpeg", "-version"]' in pipeline
    assert pipeline.index('helper.generate(') < pipeline.index('for step in ["configure", "compile", "install"]')
    assert pipeline.index('validate_parser_union(') < pipeline.index('shutil.copy2(retained_binary')
    assert pipeline.index('assemble(') < pipeline.index('return verify_closure(')
    docker = (root / "Dockerfile").read_text()
    stage = docker.split('FROM runtime-assets AS linux-dolby-package-build', 1)[1].split('FROM runtime-assets AS linux-dolby-install', 1)[0]
    assert 'test "$TARGETARCH" = amd64' in stage
    assert '--step pipeline' in stage
    assert 'FROM scratch AS linux-dolby-package-export' in stage
    assert 'COPY --from=linux-dolby-package-build /work/linux-dolby/package /package' in stage
    assert 'cargo' not in stage and 'rust:' not in stage


def test_sdk_link_search_resolves_real_transitive_elf_without_runtime_build_paths():
    import shlex
    import shutil
    import subprocess
    import sys
    if not sys.platform.startswith("linux") or not shutil.which("gcc") or not shutil.which("readelf"):
        raise unittest.SkipTest("requires the GNU Linux linker and readelf")
    module = recipe()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        sdk = root / "prepared-sdk"
        libraries = sdk / "sdk/lib"
        libraries.mkdir(parents=True)
        (root / "leaf.c").write_text("int leaf(void) { return 42; }\n")
        (root / "consumer.c").write_text("extern int leaf(void); int consumer(void) { return leaf(); }\n")
        (root / "main.c").write_text("extern int consumer(void); int main(void) { return consumer(); }\n")
        def command(argv):
            return subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=15)
        leaf = libraries / "libplurx_sdk_leaf.so.1"
        result = command(["gcc", "-shared", "-fPIC", str(root / "leaf.c"),
                          "-Wl,-soname,libplurx_sdk_leaf.so.1", "-o", str(leaf)])
        assert result.returncode == 0, result.stderr
        shutil.copyfile(leaf, libraries / "libplurx_sdk_leaf.so")
        result = command(["gcc", "-shared", "-fPIC", str(root / "consumer.c"),
                          "-L" + str(libraries), "-lplurx_sdk_leaf", "-o", str(libraries / "libplurx_sdk_consumer.so")])
        assert result.returncode == 0, result.stderr
        direct = ["gcc", str(root / "main.c"), "-L" + str(libraries), "-lplurx_sdk_consumer", "-o", str(root / "direct")]
        assert command(direct).returncode != 0
        executable = root / "linked"
        result = command(["gcc", str(root / "main.c"), "-Wl,--disable-new-dtags",
                          "-Wl,-rpath=/usr/lib/jellyfin-ffmpeg/lib",
                          *shlex.split(module.sdk_link_flags(sdk)), "-lplurx_sdk_consumer", "-o", str(executable)])
        assert result.returncode == 0, result.stderr
        dynamic = command(["readelf", "-d", str(executable)])
        assert dynamic.returncode == 0, dynamic.stderr
        assert "RPATH" in dynamic.stdout and "/usr/lib/jellyfin-ffmpeg/lib" in dynamic.stdout
        assert str(sdk) not in dynamic.stdout
