"""The shipping producer/parser relation binds bytes and the entire source union."""
import importlib.machinery
import importlib.util
import json
import os
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
    def test_download_checksum_failure_identifies_only_public_input_and_hashes(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / "fc-source.tar.gz"
            archive.write_bytes(b"wrong downloaded payload")
            actual = module.sha(archive)
            module.verify_source_download(archive, actual)
            expected = "a" * 64
            with self.assertRaises(ValueError) as failure:
                module.verify_source_download(archive, expected)
            self.assertEqual(str(failure.exception),
                "immutable source download checksum mismatch: "
                f"fc-source.tar.gz expected={expected} actual={actual}")
            self.assertNotIn(str(archive.parent), str(failure.exception))

    def test_git_source_offer_fetches_pinned_commit_instead_of_repository_html(self):
        import hashlib
        import time
        module = recipe()
        helper = module.load("prepare-linux-dolby-sdk")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repository = root / "public-fixture"
            repository.mkdir()
            home = root / "git-home"
            home.mkdir()
            git_environment = {"PATH": "/usr/bin:/bin", "HOME": str(home), "LC_ALL": "C",
                               "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
                               "GIT_TERMINAL_PROMPT": "0"}
            def git(*args):
                return subprocess.check_output(["/usr/bin/git", "-C", str(repository),
                    "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", *args],
                    env=git_environment, stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q", "--template=")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (repository / "source.txt").write_text("pinned source\n")
            git("add", "source.txt")
            git("commit", "-qm", "pinned")
            commit = git("rev-parse", "HEAD")
            expected = root / "expected.tar.gz"
            git("archive", "--format=tar.gz", "--output", str(expected), commit)
            (repository / "source.txt").write_text("later branch content must not ship\n")
            git("commit", "-qam", "later")
            inputs = root / "inputs"
            inputs.mkdir()
            target = inputs / "source.tar.gz"
            invoked = []
            def transport(argv, cwd, environment, log, deadline):
                request = json.loads(argv[-1])
                command = request["argv"]
                invoked.append((list(command), dict(environment)))
                if "fetch" in command:
                    self.assertEqual(command[-1], commit)
                    self.assertEqual(command[-2], "https://example.invalid/public-repository")
                    request["argv"] = [*command[:-2], str(repository), command[-1]]
                    argv = [*argv[:-1], json.dumps(request)]
                return helper.bounded_generator(argv, cwd, environment, log, deadline)
            with patch.object(module, "load", return_value=SimpleNamespace(bounded_generator=transport)), \
                    patch("urllib.request.urlopen", side_effect=AssertionError("repository HTML must never be downloaded")):
                module.fetch_source_archive(target, "https://example.invalid/public-repository",
                    hashlib.sha256(expected.read_bytes()).hexdigest(), time.monotonic() + 30, "git-archive", commit)
            self.assertEqual(target.read_bytes(), expected.read_bytes())
            self.assertEqual(len(invoked), 3)
            self.assertTrue(all(env["GIT_TERMINAL_PROMPT"] == "0" and env["GIT_CONFIG_GLOBAL"] == "/dev/null"
                                and env["GIT_CONFIG_NOSYSTEM"] == "1" for _, env in invoked))
            self.assertEqual(list(root.glob("public-git-source-*")), [])
            evidence = json.loads((root / "source-fetch-evidence/source.tar.gz/receipt.json").read_text())
            self.assertEqual(evidence["commit"], commit)

    def test_git_source_storage_overflow_terminates_the_owned_process_group(self):
        import sys
        import time
        module = recipe()
        helper = module.load("prepare-linux-dolby-sdk")
        for kind in ["git-object-database", "source-archive"]:
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                storage = root / "bounded-storage"
                if kind == "git-object-database":
                    storage.mkdir()
                    output = storage / "pack"
                else:
                    output = storage
                descendants = root / "descendant-pid"
                command = [sys.executable, "-c",
                    "import subprocess,sys,time; from pathlib import Path; "
                    "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); "
                    "Path(sys.argv[1]).write_text(str(child.pid)); "
                    "Path(sys.argv[2]).write_bytes(b'x'*16384); time.sleep(30)",
                    str(descendants), str(output)]
                log = root / "overflow.log"
                started = time.monotonic()
                with self.assertRaises(ValueError):
                    module.bounded_source_command(helper, command, root, {"PATH": "/usr/bin:/bin"}, log,
                        started + 10, [{"kind": kind, "path": str(storage), "bytes": 4096}])
                self.assertLess(time.monotonic() - started, 5)
                self.assertIn("source storage limit exceeded: kind=" + kind, log.read_text())
                self.assertIn("limit=4096", log.read_text())
                pid = int(descendants.read_text())
                state = subprocess.run(["/bin/ps", "-o", "stat=", "-p", str(pid)],
                                       capture_output=True, text=True, timeout=3)
                self.assertTrue(state.returncode != 0 or state.stdout.strip().startswith("Z"),
                                "overflow left a running descendant: " + state.stdout)

    def test_archive_provider_and_commit_are_rejected_before_fetch(self):
        import time
        module = recipe()
        with patch.object(module, "load") as generator, patch("urllib.request.urlopen") as download:
            for provider, commit in [("unknown", None), ("git-archive", "main"),
                                     ("git-archive", "a" * 39), ("git-archive", "--upload-pack=other")]:
                with self.subTest(provider=provider, commit=commit), self.assertRaisesRegex(ValueError, "provider"):
                    module.fetch_source_archive(Path("/unused"), "https://example.invalid/source", "0" * 64,
                                                time.monotonic() + 30, provider, commit)
            generator.assert_not_called()
            download.assert_not_called()

    def public_git_fixture(self, module, root, commit_hex=None):
        """A local repository standing in for the public HTTPS Git remote."""
        import hashlib
        import time
        helper = module.load("prepare-linux-dolby-sdk")
        repository = root / "public-fixture"
        repository.mkdir()
        home = root / "git-home"
        home.mkdir()
        environment = {"PATH": "/usr/bin:/bin", "HOME": str(home), "LC_ALL": "C",
                       "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
                       "GIT_TERMINAL_PROMPT": "0"}
        def git(*args):
            return subprocess.check_output(["/usr/bin/git", "-C", str(repository),
                "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", *args],
                env=environment, stderr=subprocess.DEVNULL).decode().strip()
        git("init", "-q", "--template=")
        git("config", "user.name", "Fixture")
        git("config", "user.email", "fixture@example.invalid")
        (repository / "source.txt").write_text("pinned source\n")
        git("add", "source.txt")
        git("commit", "-qm", "pinned")
        commit = git("rev-parse", "HEAD")
        expected = root / "expected.tar.gz"
        git("archive", "--format=tar.gz", "--output", str(expected), commit)
        def transport(argv, cwd, environment, log, deadline):
            request = json.loads(argv[-1])
            command = request["argv"]
            if "fetch" in command:
                request["argv"] = [*command[:-2], str(repository), command[-1]]
                argv = [*argv[:-1], json.dumps(request)]
            # The caller's deadline may be on a fake clock; the real runner gets a real one.
            return helper.bounded_generator(argv, cwd, environment, log, time.monotonic() + 30)
        return commit, hashlib.sha256(expected.read_bytes()).hexdigest(), transport

    def test_git_source_archive_refuses_a_different_fetched_commit(self):
        # The pin is the object name that is fetched and archived, so a remote
        # that does not hold exactly that commit can produce no output at all.
        import time
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inputs = root / "inputs"
            inputs.mkdir()
            commit, digest, transport = self.public_git_fixture(module, root)
            other = ("b" if commit[0] != "b" else "c") * 40
            target = inputs / "source.tar.gz"
            with patch.object(module, "load", return_value=SimpleNamespace(bounded_generator=transport)):
                with self.assertRaises(ValueError):
                    module.fetch_source_archive(target, "https://example.invalid/public-repository",
                                                digest, time.monotonic() + 30, "git-archive", other)
            self.assertFalse(target.exists())
            self.assertEqual(list(root.glob("public-git-source-*")), [])

    def test_git_source_archive_checksum_failure_cannot_publish_output(self):
        import time
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inputs = root / "inputs"
            inputs.mkdir()
            commit, digest, transport = self.public_git_fixture(module, root)
            target = inputs / "source.tar.gz"
            with patch.object(module, "load", return_value=SimpleNamespace(bounded_generator=transport)):
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    module.fetch_source_archive(target, "https://example.invalid/public-repository",
                                                "f" * 64, time.monotonic() + 30, "git-archive", commit)
            self.assertFalse(target.exists())
            self.assertEqual(list(root.glob("public-git-source-*")), [])

    def test_git_source_acquisition_requires_explicit_matching_public_commit(self):
        import time
        module = recipe()
        commit = "a" * 40
        with patch.object(module, "load") as generator, patch("urllib.request.urlopen") as download:
            for url in ["https://user:password@example.invalid/fontconfig",
                        "https://example.invalid/fontconfig?ref=main",
                        "https://example.invalid/fontconfig#main", "https://example.invalid"]:
                with self.subTest(url=url), self.assertRaisesRegex(ValueError, "exact plain public HTTPS"):
                    module.fetch_source_archive(Path("/unused"), url, "0" * 64, time.monotonic() + 30,
                                                "git-archive", commit)
            generator.assert_not_called()
            download.assert_not_called()
        # The lock must name the same exact commit twice before anything is fetched.
        lock = json.loads((Path(__file__).resolve().parents[2] / "scripts/linux-video-ffmpeg-sdk-sources.json").read_text())
        role = next(role for role in [*lock["sources"].values(), *lock.get("build_tools", {}).values()]
                    if role.get("archive_provider") == "git-archive")
        for changed in [{"archive_acquisition": {}},
                        {"archive_acquisition": {"kind": "git-archive", "commit": "b" * 40}},
                        {"archive_provider": "https-archive"}]:
            mutated = json.loads(json.dumps(lock))
            for group in [mutated["sources"], mutated.get("build_tools", {})]:
                for name, candidate in group.items():
                    if candidate.get("archive") == role["archive"]:
                        candidate.update(changed)
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as tmp, \
                    patch.object(module, "host"), patch.object(module.json, "loads", return_value=mutated), \
                    patch("urllib.request.urlopen") as download, \
                    self.assertRaisesRegex(ValueError, "exact public pinned commit"):
                module.package_pipeline(Path(tmp) / "package", 1, 10)
                download.assert_not_called()

    def test_git_source_deadline_retires_process_group_and_reaps_owner(self):
        import time
        module = recipe()
        helper = module.load("prepare-linux-dolby-sdk")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            descendant = root / "descendant-pid"
            command = ["/bin/sh", "-c", 'sleep 60 & echo $! > "$1"; wait', "sh", str(descendant)]
            children = []
            original = subprocess.Popen
            def launch(*args, **kwargs):
                child = original(*args, **kwargs)
                children.append(child)
                return child
            started = time.monotonic()
            with patch("subprocess.Popen", side_effect=launch):
                with self.assertRaisesRegex(ValueError, "finite deadline"):
                    module.bounded_source_command(helper, command, root, {"PATH": "/usr/bin:/bin"},
                        root / "deadline.log", started + 2, [])
            self.assertLess(time.monotonic() - started, 8)
            self.assertEqual(len(children), 1)
            self.assertIsNotNone(children[0].returncode)
            with self.assertRaises(ProcessLookupError):
                os.kill(children[0].pid, 0)
            state = subprocess.run(["/bin/ps", "-o", "stat=", "-p", descendant.read_text().strip()],
                                   capture_output=True, text=True, timeout=3)
            self.assertTrue(state.returncode != 0 or state.stdout.strip().startswith("Z"),
                            "deadline left a running descendant: " + state.stdout)

    def test_git_scratch_scan_tolerates_only_live_disappearance(self):
        # The storage guard measures the object database while Git is still
        # running: a vanished entry is benign, anything unreadable is not.
        module = recipe()
        namespace = {}
        with patch("sys.argv", ["guard", json.dumps({"argv": [], "limits": []})]), \
                patch("os.getpgrp", return_value=os.getpid()):
            exec(module.SOURCE_STORAGE_GUARD.split("def enforce")[0], namespace)
        allocated = namespace["allocated"]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertEqual(allocated(str(root / "never-existed")), 0)
            (root / "pack").write_bytes(b"x" * 10)
            self.assertEqual(allocated(str(root)), 10)
            def walk_reporting(error):
                def walk(path, followlinks=False, onerror=None):
                    onerror(error)
                    return iter(())
                return walk
            with patch("os.walk", walk_reporting(FileNotFoundError())):
                self.assertEqual(allocated(str(root)), 0)
            with patch("os.walk", walk_reporting(PermissionError())), self.assertRaises(PermissionError):
                allocated(str(root))
            real_lstat = os.lstat
            def lstat(path, *args, **kwargs):
                if str(path).endswith("pack"):
                    raise PermissionError(path)
                return real_lstat(path, *args, **kwargs)
            with patch("os.lstat", side_effect=lstat), self.assertRaises(PermissionError):
                allocated(str(root))

    def test_expired_git_acquisition_cannot_spawn(self):
        import time
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp, patch.object(module, "load") as generator, \
                patch("subprocess.Popen") as launch:
            root = Path(tmp)
            (root / "inputs").mkdir()
            with self.assertRaisesRegex(ValueError, "deadline exceeded"):
                module.fetch_source_archive(root / "inputs" / "source.tar.gz", "https://example.invalid/public-repository",
                                            "0" * 64, time.monotonic() - 1, "git-archive", "a" * 40)
            launch.assert_not_called()
            generator.assert_not_called()
            self.assertFalse((root / "source-fetch-evidence").exists())
        # The runner itself also refuses an expired deadline before spawning.
        helper = module.load("prepare-linux-dolby-sdk")
        with tempfile.TemporaryDirectory() as tmp, patch("subprocess.Popen") as launch:
            with self.assertRaisesRegex(ValueError, "deadline exceeded"):
                module.bounded_source_command(helper, ["/usr/bin/git", "fetch"], Path(tmp), {},
                                              Path(tmp) / "log", time.monotonic() - 1, [])
            launch.assert_not_called()

    def test_git_archive_expiry_during_verification_removes_output(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inputs = root / "inputs"
            inputs.mkdir()
            commit, digest, transport = self.public_git_fixture(module, root)
            target = inputs / "source.tar.gz"
            clock = [0]
            def checksum(path):
                clock[0] = 11  # the deadline passes while the archive is being hashed
                return digest
            with patch.object(module, "load", return_value=SimpleNamespace(bounded_generator=transport)), \
                    patch.object(module, "sha", side_effect=checksum), \
                    patch.object(module, "time", SimpleNamespace(monotonic=lambda: clock[0])):
                with self.assertRaisesRegex(ValueError, "during verification"):
                    module.fetch_source_archive(target, "https://example.invalid/public-repository",
                                                digest, 10, "git-archive", commit)
            self.assertFalse(target.exists())

    def test_https_archive_default_keeps_pin_and_reports_mismatched_input(self):
        import hashlib
        import io
        import time
        module = recipe()
        good = b"pinned archive transport bytes"
        expected = hashlib.sha256(good).hexdigest()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(module, "load", side_effect=AssertionError("HTTPS must not invoke Git")), \
                    patch("urllib.request.urlopen", return_value=io.BytesIO(good)):
                self.assertEqual(module.fetch_source_archive(root / "good", "https://example.invalid/archive", expected,
                                  time.monotonic() + 30), len(good))
            bad = b"<!DOCTYPE html>repository landing page"
            with patch("urllib.request.urlopen", return_value=io.BytesIO(bad)):
                with self.assertRaises(ValueError) as error:
                    module.fetch_source_archive(root / "refused", "https://example.invalid/archive", expected,
                                                time.monotonic() + 30)
            self.assertIn(str(root / "refused"), str(error.exception))
            self.assertIn("expected=" + expected, str(error.exception))
            self.assertIn("actual=" + hashlib.sha256(bad).hexdigest(), str(error.exception))
            self.assertEqual((root / "refused").read_bytes(), bad)

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


    def run_make_docker(self, machine, package=None):
        """Run the real `make docker` recipe with docker and the package step recorded."""
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as tmp:
            fake, calls = Path(tmp) / "bin", Path(tmp) / "calls"
            fake.mkdir()
            real_python = subprocess.run(["sh", "-c", "command -v python3"], capture_output=True, text=True, check=True).stdout.strip()
            real_uname = subprocess.run(["sh", "-c", "command -v uname"], capture_output=True, text=True, check=True).stdout.strip()
            scripts = {
                "docker": f'printf "docker %s\\n" "$*" >> "{calls}"',
                "uname": f'if [ "$1" = -m ]; then echo {machine}; else exec {real_uname} "$@"; fi',
                "python3": f'if [ "$1" = scripts/build-linux-dolby-ffmpeg ]; then printf "package %s\\n" "$*" >> "{calls}"; else exec {real_python} "$@"; fi',
            }
            for name, body in scripts.items():
                (fake / name).write_text("#!/bin/sh\n" + body + "\n")
                (fake / name).chmod(0o755)
            env = {key: value for key, value in os.environ.items() if key not in ["PLURX_LINUX_DOLBY_PACKAGE", "DOCKER_DEFAULT_PLATFORM", "MAKEFLAGS", "MAKELEVEL"]}
            env["PATH"] = f"{fake}:{env.get('PATH', '')}"
            if package is not None:
                env["PLURX_LINUX_DOLBY_PACKAGE"] = package
            result = subprocess.run(["make", "-s", "-C", str(root), "docker"], env=env, capture_output=True, text=True, timeout=60)
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_normal_docker_caller_builds_runtime_without_package_and_installs_supplied_amd64_package(self):
        # The package is an input, never a precondition: amd64 without one
        # builds the same runtime stage Compose and arm64 build.
        for machine, platform in [("x86_64", "linux/amd64"), ("aarch64", "linux/arm64")]:
            result, calls = self.run_make_docker(machine)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(calls), 1, calls)
            self.assertTrue(calls[0].startswith(f"docker build --target runtime --platform {platform} "), calls)
            self.assertIn("-t plurx/plurxd:latest", calls[0])
        result, calls = self.run_make_docker("x86_64", package="/srv/audited-package")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 1, calls)
        self.assertTrue(calls[0].startswith("package scripts/build-linux-dolby-ffmpeg --step docker-build "), calls)
        self.assertIn("--output /srv/audited-package", calls[0])
        # docker-build compiles plurxd; a package-phase deadline would kill it.
        root = Path(__file__).resolve().parents[2]
        docker = (root / "Dockerfile").read_text()
        builder = (root / "scripts/build-linux-dolby-ffmpeg").read_text()
        invocation = builder.split('"docker", "buildx", "build"', 1)[1].split("\n    elif", 1)[0]
        assert "timeout=" not in invocation
        assert "FROM runtime AS runtime-dolby-amd64" in docker
        assert "--mount=from=linux-dolby-package" in docker
        assert "--step install-shipping" in docker


    def test_docker_install_tools_are_temporary_and_build_identity_arguments_survive(self):
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


    def test_shipping_retirement_preserves_audited_system_elf_closure(self):
        module = recipe()
        with tempfile.TemporaryDirectory() as directory:
            package = Path(directory)
            (package / "provenance").mkdir()
            capsule = package / "provenance/linux-dolby-package.json"
            system_path = "/usr/lib/x86_64-linux-gnu/libc.so.6"
            facts = {"elf_closure": {
                system_path: {"distributed": False, "sha256": "a" * 64},
                "/work/package/lib/libmfx.so.1": {"distributed": True, "sha256": "b" * 64}}}
            capsule.write_text(json.dumps(facts))
            self.assertEqual(module.shipping_system_objects(package), {system_path: "a" * 64})
            for path, digest in [("/tmp/foreign.so", "a" * 64),
                                 (system_path + "\nmalformed", "a" * 64),
                                 ("/usr/lib/x86_64-linux-gnu/../foreign.so", "a" * 64),
                                 (system_path, "a" * 64 + "\n/etc/passwd")]:
                facts["elf_closure"] = {path: {"distributed": False, "sha256": digest}}
                capsule.write_text(json.dumps(facts))
                with self.assertRaises(ValueError):
                    module.shipping_system_objects(package)
            capsule.write_text(json.dumps({"elf_closure": {}}))
            with self.assertRaisesRegex(ValueError, "no audited system ELF closure"):
                module.shipping_system_objects(package)

    def test_only_dolby_shipping_targets_retire_the_competing_debian_media_sdk(self):
        docker = (Path(__file__).resolve().parents[2] / "Dockerfile").read_text()
        incumbent = docker.split("FROM runtime-assets AS runtime-assets-dolby-amd64", 1)[0]
        self.assertNotIn("apt-get purge -y libmfx1", incumbent)
        for marker in ["FROM runtime-assets AS runtime-assets-dolby-amd64",
                       "FROM runtime AS runtime-dolby-amd64"]:
            stage = docker.split(marker, 1)[1].split("\nFROM ", 1)[0]
            self.assertIn("apt-get purge -y libmfx1", stage)
            self.assertNotIn("apt-get autoremove", stage)
            self.assertIn("sha256sum -c /usr/lib/jellyfin-ffmpeg/provenance/shipping-system-elf-sha256.txt", stage)
            self.assertLess(stage.index("while IFS= read -r path"), stage.index("apt-get purge -y libmfx1"))
            self.assertLess(stage.index("apt-get purge -y libmfx1"), stage.index("sha256sum -c"))
            for name in ["ffmpeg", "ffprobe"]:
                self.assertIn(f"ln -sfn /usr/lib/jellyfin-ffmpeg/{name} /usr/bin/{name}", stage)
            self.assertIn("media-runtime-packages.txt", stage)
        self.assertIn("FROM runtime AS default-runtime", docker)

    def test_package_export_observes_official_configuration_and_reuses_only_matched_parser(self):
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


    def test_sdk_link_search_resolves_real_transitive_elf_without_runtime_build_paths(self):
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
