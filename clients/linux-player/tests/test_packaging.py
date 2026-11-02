"""The desktop entry registers the scheme, the package never edits associations
as root, and the private mpv/libplacebo build is pinned where the launcher and
the package version expect it."""

import configparser
import glob
import hashlib
import os
import re
import shutil
import subprocess
import tempfile
import unittest

import support
from plurx_player import VERSION, mpv

ROOT = support.PACKAGE_ROOT


class Packaging(unittest.TestCase):
    def test_desktop_entry_handles_the_scheme(self):
        entry = configparser.ConfigParser(interpolation=None)
        entry.optionxform = str
        entry.read(os.path.join(ROOT, "plurx-player.desktop"), encoding="utf-8")
        desktop = entry["Desktop Entry"]
        self.assertEqual(desktop["Type"], "Application")
        self.assertEqual(desktop["MimeType"], "x-scheme-handler/plurx-player;")
        self.assertEqual(desktop["Exec"], "/usr/bin/plurx-player %u")
        self.assertEqual(desktop["Terminal"], "false")

    def test_debian_installs_what_exists_and_never_runs_xdg_mime(self):
        with open(os.path.join(ROOT, "debian/install"), encoding="utf-8") as handle:
            rows = [line.split() for line in handle if line.strip() and not line.startswith("#")]
        for source, destination in rows:
            self.assertTrue(glob.glob(os.path.join(ROOT, source)), source)
            self.assertTrue(destination.startswith("usr/"), destination)
        self.assertIn(["plurx-player", "usr/bin/"], rows)
        self.assertIn(["plurx_player/scripts/*.lua", "usr/lib/plurx-player/plurx_player/scripts/"], rows)
        for name in os.listdir(os.path.join(ROOT, "debian")):
            path = os.path.join(ROOT, "debian", name)
            if os.path.isfile(path):
                with open(path, encoding="utf-8") as handle:
                    self.assertNotIn("xdg-mime", handle.read(), name)
        self.assertFalse(os.path.exists(os.path.join(ROOT, "debian/postinst")))
        with open(os.path.join(ROOT, "debian/control"), encoding="utf-8") as handle:
            control = handle.read()
        self.assertIn("zenity", control)

    def test_private_launcher_bytecode_is_owned_by_dh_python3(self):
        # A root run of the launcher writes __pycache__ under the private
        # directory (the -I shebang ignores PYTHONDONTWRITEBYTECODE); only
        # dh_python3 over that directory makes prerm remove it, so dpkg -r
        # leaves nothing behind.
        with open(os.path.join(ROOT, "debian/rules"), encoding="utf-8") as handle:
            rules = handle.read()
        self.assertRegex(rules, r"(?m)^override_dh_python3:\n\tdh_python3 /\$\(PRIVATE\)$")
        self.assertRegex(rules, r"(?m)^PRIVATE = usr/lib/plurx-player$")
        with open(os.path.join(ROOT, "debian/control"), encoding="utf-8") as handle:
            control = handle.read()
        self.assertIn("dh-sequence-python3", control)
        self.assertIn("${python3:Depends}", control)

    def test_launcher_script_is_executable(self):
        self.assertTrue(os.access(os.path.join(ROOT, "plurx-player"), os.X_OK))


def read(relative):
    with open(os.path.join(ROOT, relative), encoding="utf-8") as handle:
        return handle.read()


def pins():
    rows = {}
    for line in read("packaging/upstream.lock").splitlines():
        if line.strip() and not line.startswith("#") and not line.startswith("apt-snapshot "):
            name, version, sha256, url = line.split()
            rows[name] = (version, sha256, url)
    return rows


class PrivateBuild(unittest.TestCase):
    def test_sources_are_pinned_by_sha256(self):
        rows = pins()
        self.assertEqual(sorted(rows), ["libplacebo", "mpv"])
        for name, (version, sha256, url) in rows.items():
            self.assertRegex(sha256, r"^[0-9a-f]{64}$", name)
            self.assertTrue(url.startswith("https://"), url)
            if "+git" in version:
                # A commit pin: <release>+git<date>.<short commit>, and the URL
                # names the full commit.
                short = version.rsplit(".", 1)[1]
                commit = re.search(r"/([0-9a-f]{40})\.tar\.gz$", url)
                self.assertTrue(commit and commit.group(1).startswith(short), (name, version, url))
            else:
                self.assertIn(version, url, name)

    def test_the_build_environment_is_a_pinned_apt_snapshot(self):
        rows = [line.split() for line in read("packaging/upstream.lock").splitlines()
                if line.startswith("apt-snapshot ")]
        self.assertEqual(len(rows), 1, rows)
        self.assertRegex(rows[0][1], r"^[0-9]{8}T[0-9]{6}Z$")

    def test_every_container_shares_the_snapshot_apt_cache(self):
        # A 503 from snapshot.ubuntu.com used to fail every build: each of
        # the three containers fetched the same immutable indexes and
        # packages again.
        build = read("packaging/build-deb.sh")
        self.assertIn('-v "$apt_cache:/apt-cache"', build)
        self.assertIn("chown -R", build)
        snapshot = read("packaging/apt-snapshot.sh")
        self.assertIn('Dir::State::lists "/apt-cache/lists";', snapshot)
        self.assertIn('Dir::Cache::archives "/apt-cache/archives";', snapshot)
        # Reused only once fetched and verified in full for this timestamp.
        self.assertIn("marker=/apt-cache/lists/.plurx-snapshot-$ts", snapshot)
        self.assertLess(snapshot.index("apt-get -q -o APT::Update::Error-Mode=any update\n["), snapshot.index('touch "$marker"'))
        self.assertIn('Acquire::Retries "10";', snapshot)
        # A partial index must fail the run, never be marked complete.
        self.assertEqual(snapshot.count("-o APT::Update::Error-Mode=any"), 2)
        self.assertLess(snapshot.index('if [ -f "$marker" ]'), snapshot.index("apt-get -q -o APT::Update::Error-Mode=any"))
        self.assertIn('flock -n 9', build)

    def test_package_version_names_the_pinned_versions(self):
        rows = pins()
        version = re.match(r"plurx-player \(([^)]+)\)", read("debian/changelog")).group(1)
        self.assertTrue(version.startswith(f"{VERSION}+"), (version, VERSION))
        self.assertIn(f'"plurx-player {VERSION}"', read("plurx-player.1"))
        upstream, _, revision = version.rpartition("-")
        self.assertRegex(revision, r"^[0-9]+$", "a Debian revision: the source package is 3.0 (quilt)")
        self.assertTrue(
            upstream.endswith(f"+lp{rows['libplacebo'][0]}.mpv{rows['mpv'][0]}"), version)
        self.assertEqual(read("debian/source/format").strip(), "3.0 (quilt)")

    def test_each_patch_series_lists_exactly_its_patches(self):
        for name in ("libplacebo", "mpv"):
            directory = os.path.join(ROOT, "packaging/patches", name)
            entries = os.listdir(directory)
            self.assertIn("series", entries)
            for entry in entries:
                self.assertTrue(entry == "series" or entry.endswith(".patch"), entry)
            listed = [line.split("#")[0].strip() for line in read(f"packaging/patches/{name}/series").splitlines()]
            listed = [entry for entry in listed if entry]
            self.assertEqual(sorted(listed), sorted(e for e in entries if e.endswith(".patch")), name)

    def test_verification_never_lets_a_failed_check_fall_through_to_pass(self):
        # Under set -e, `cmd && pass` lets a failing cmd fall through; every
        # check is `cmd || fail ...` then `pass ...`.
        for number, line in enumerate(read("packaging/verify-in-container.sh").splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            self.assertNotRegex(line, r"&&\s*pass\b", f"verify-in-container.sh:{number}")
            self.assertNotRegex(line, r"\|\|\s*true\b", f"verify-in-container.sh:{number}")

    def test_launcher_default_is_the_installed_private_mpv(self):
        rules = read("debian/rules")
        private = re.search(r"^PRIVATE = (\S+)$", rules, re.M).group(1)
        self.assertIn("$(PKG)/$(PRIVATE)/bin/mpv", rules)
        self.assertEqual(mpv.DEFAULT_BINARY, f"/{private}/bin/mpv")

    def test_description_states_what_hdr_needs(self):
        control = read("debian/control")
        self.assertIn("Architecture: amd64", control)
        self.assertIn("${shlibs:Depends}", control)
        self.assertIn("wp_color_manager_v1 in HDR mode", control)
        self.assertIn("reports the panel's real luminance", control)
        self.assertNotIn("Recommends: mpv", control)
        self.assertIn("Recommends: desktop-file-utils, mesa-vulkan-drivers | vulkan-icd", control)
        self.assertIn("Built-Using: ${plurx:Built-Using}", control)

    def test_libplacebo_is_linked_in_renamed_and_hidden(self):
        # The system FFmpeg's libavfilter loads the system libplacebo into
        # mpv's process; libplacebo's symbols are unversioned, so the private
        # copy must neither be a shared object nor share its symbol names.
        build = read("packaging/build-private.sh")
        self.assertIn("-Ddefault_library=static", build)
        self.assertIn('CFLAGS="${CFLAGS:-} -fno-lto"', build)   # objcopy cannot rename LTO bytecode
        self.assertIn('objcopy --redefine-syms="$out/libplacebo.rename" "$archive"', build)
        self.assertIn("-Wl,--exclude-libs,ALL", build)
        self.assertIn("nm -D --undefined-only", build)          # mpv imports no pl_* symbol
        self.assertIn("mpv exports libplacebo symbols", build)
        self.assertNotIn("-Wl,-rpath", build)
        rules = read("debian/rules")
        self.assertNotIn("libplacebo.so", rules)
        self.assertNotIn("$(PRIVATE)/lib", rules)
        verify = read("packaging/verify-in-container.sh")
        self.assertIn("mpv would call the system libplacebo", verify)
        self.assertIn("the package installs a private library directory", verify)


# The libdovi C API header plurx's processing helper pins
# (tools/dv_quality/backends/parsed-rpu/fetch_parser.py).
LIBDOVI_HEADER_SHA256 = "77e130d174a285ec4d141a88245f12b068606174a745f39f7aca54a2c1439f82"

DM_HEADERS = ("dm_closed_form.h", "dv_dm.h")


def read_dm(header):
    tools = os.path.join(ROOT, "..", "..", "tools", "dv_processing")
    base = tools if os.path.isdir(tools) else os.path.join(ROOT, "dm")
    with open(os.path.join(base, header), encoding="utf-8") as handle:
        return handle.read()


class ClosedFormMapping(unittest.TestCase):
    """mpv compiles plurx's own display management, not a copy of it."""

    def test_the_mpv_series_maps_dolby_vision_with_plurx_headers(self):
        series = read("packaging/patches/mpv/series")
        patch = "0003-vo_gpu_next-map-Dolby-Vision-with-plurx-s-closed-for.patch"
        self.assertIn(patch, series)
        text = read(f"packaging/patches/mpv/{patch}")
        self.assertIn('#include "plurx/dv_dm.h"', text)
        self.assertIn('#include "plurx/rpu_parser.h"', text)
        self.assertIn("plurx mapper: %s", text)
        # The equations are never carried in the patch itself.
        self.assertNotIn("+++ b/video/out/placebo/plurx/", text)

    def test_the_build_installs_the_headers_the_patch_includes(self):
        build = read("packaging/build-private.sh")
        for file in ("dm/dm_closed_form.h", "dm/dv_dm.h", "packaging/libdovi/rpu_parser.h"):
            self.assertIn(file, build)
        self.assertIn("video/out/placebo/plurx", build)
        container = read("packaging/build-in-container.sh")
        for header in DM_HEADERS:
            self.assertIn(f"/repo/tools/dv_processing/{header}", container)
            self.assertIn(header, read("packaging/fetch-sources.sh"))

    def test_the_display_management_headers_exist(self):
        # In the plurx tree they are tools/dv_processing's; in the unpacked
        # source package, the dm/ copies build-in-container.sh made.
        tools = os.path.join(ROOT, "..", "..", "tools", "dv_processing")
        for header in DM_HEADERS:
            self.assertTrue(os.path.isfile(os.path.join(tools, header)) or
                            os.path.isfile(os.path.join(ROOT, "dm", header)), header)

    def test_the_libdovi_header_is_the_pinned_one(self):
        with open(os.path.join(ROOT, "packaging/libdovi/rpu_parser.h"), "rb") as handle:
            self.assertEqual(hashlib.sha256(handle.read()).hexdigest(), LIBDOVI_HEADER_SHA256)
        helper = os.path.join(ROOT, "..", "..", "tools/dv_quality/backends/parsed-rpu/fetch_parser.py")
        if os.path.isfile(helper):
            with open(helper, encoding="utf-8") as handle:
                self.assertIn(LIBDOVI_HEADER_SHA256, handle.read())
        self.assertIn("libdovi-dev", read("debian/control"))
        # The header's struct layouts are proven against libdovi3 3.3.
        self.assertIn("libdovi3 (<< 3.4~)", read("debian/control"))

    def test_the_changelog_names_the_equations_it_ships(self):
        # dm/ is copied from tools/dv_processing at build time, so a change to
        # the equations must come with a new package version: the newest
        # changelog entry records their hash.
        tools = os.path.join(ROOT, "..", "..", "tools", "dv_processing")
        base = tools if os.path.isdir(tools) else os.path.join(ROOT, "dm")
        digest = hashlib.sha256()
        for header in DM_HEADERS:
            with open(os.path.join(base, header), "rb") as handle:
                digest.update(handle.read())
        newest = read("debian/changelog").split("\n -- ", 1)[0]
        self.assertIn(f"dm {digest.hexdigest()[:12]}", newest)

    def test_cm29_evaluates_its_lattice_once_per_frame(self):
        # One 4K CM v2.9 frame cost 34 ms on an Intel Arc iGPU when the final
        # pass evaluated eight lattice vertices per pixel inside a shader that
        # also held CM v4.0; the frame's lattice is now a table, and each
        # method's passes are their own shaders.
        dm = read_dm("dv_dm.h")
        self.assertIn("static const char dv_dm_body_lattice[]", dm)
        self.assertIn("acc+=c29corner(lat,origin,step,base+b)", dm)
        self.assertNotIn("acc+=c29vertex(", dm)
        for body in ("point_cm4", "point_cm29", "final_cm4", "final_cm29"):
            self.assertIn(f"static const char dv_dm_body_{body}[]", dm)
        self.assertNotIn("u[18].x<10.0", dm)   # no runtime switch between methods


GIT_AM_PATCH = """From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001
From: Test <test@plurx.invalid>
Date: Thu, 1 Jan 2026 00:00:00 +0000
Subject: [PATCH] two becomes TWO

---
 a.txt | 2 +-
 1 file changed, 1 insertion(+), 1 deletion(-)

{diff}--
2.0
"""


def diff(old, new):
    return f"--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-{old}\n+{new}\n three\n"


@unittest.skipUnless(shutil.which("git"), "needs git")
class WaylandColourSurface(unittest.TestCase):
    """mpv, not the Vulkan driver, owns the window's colour description."""

    def test_the_first_wayland_swapchain_is_pass_through(self):
        # NVIDIA's WSI keeps the colour surface it made for an sRGB swapchain
        # after that swapchain is gone, and mpv's own request then ends the
        # Wayland connection ("surface already requested").
        series = read("packaging/patches/mpv/series")
        patch = "0004-vulkan-context-start-Wayland-swapchains-pass-through.patch"
        self.assertIn(patch, series)
        text = read(f"packaging/patches/mpv/{patch}")
        self.assertIn("+++ b/video/out/vulkan/context.c", text)
        hunk = text.split("        goto error;\n \n+    // A Wayland surface can have only one", 1)[1]
        self.assertIn("+    if (owns_wayland_color(sw))", hunk)
        self.assertIn("+        pl_swapchain_colorspace_hint(vk->swapchain, &(struct pl_color_space){0});", hunk)
        self.assertIn("@@ bool ra_vk_ctx_init(", text)


class EnhancementLayerOverHls(unittest.TestCase):
    """Profile 7's enhancement layer is split from streams demux_lavf reads."""

    def test_the_split_reads_the_hvcc_that_demux_lavf_keeps(self):
        # Every plurx stream comes through demux_lavf (HLS), which leaves the
        # hvcC in lav_codecpar. Read from extradata alone, the length-prefixed
        # samples were scanned as Annex B, no layer was found, and mpv
        # buffered ~150 MB before showing the base layer alone.
        text = read("packaging/patches/mpv/0002-demux-dovi_split-split-the-enhancement-layer-without.patch")
        hunk = text.split("+static int hvcc_nal_length(const struct mp_codec_params *c)", 1)[1].split("+}", 1)[0]
        self.assertIn("+    if ((!x || size <= 0) && c->lav_codecpar) {", hunk)
        self.assertIn("c->lav_codecpar->extradata_size", hunk)
        self.assertIn("x[0] != 1", hunk)


class FilmTimelineOsc(unittest.TestCase):
    """mpv's OSC draws the whole film for a live session (patch 0005)."""

    def test_the_osc_reads_the_film_timeline_and_hands_back_outside_seeks(self):
        series = read("packaging/patches/mpv/series")
        patch = "0005-osc-draw-a-film-timeline-the-application-provides.patch"
        self.assertIn(patch, series)
        text = read(f"packaging/patches/mpv/{patch}")
        self.assertIn("+++ b/player/lua/osc.lua", text)
        self.assertIn('+mp.observe_property("user-data/osc/film", "native", function (_, value)', text)
        self.assertIn('+        mp.commandv("script-message", "osc-film-seek", tostring(target))', text)
        # The launcher sets exactly what the OSC reads and answers what it sends.
        playback = read("plurx_player/playback.py")
        self.assertIn('"user-data/osc/film"', playback)
        self.assertIn('"osc-film-seek"', playback)


class PatchSeries(unittest.TestCase):
    """packaging/apply-series.sh applies the series exactly, in series order."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.tree = os.path.join(self.tmp, "tree")
        self.patches = os.path.join(self.tmp, "patches")
        os.makedirs(self.tree)
        os.makedirs(self.patches)
        with open(os.path.join(self.tree, "a.txt"), "w", encoding="utf-8") as handle:
            handle.write("one\ntwo\nthree\n")
        self.series()

    def series(self, *names):
        with open(os.path.join(self.patches, "series"), "w", encoding="utf-8") as handle:
            handle.write("# comment\n" + "".join(f"{name}\n" for name in names))

    def patch(self, name, text):
        with open(os.path.join(self.patches, name), "w", encoding="utf-8") as handle:
            handle.write(text)

    def apply(self):
        return subprocess.run(
            ["bash", os.path.join(ROOT, "packaging/apply-series.sh"), self.tree, self.patches,
             "1.2.3", "1767225600"], capture_output=True, text=True)

    def describe(self):
        return subprocess.run(["git", "-C", self.tree, "describe"], capture_output=True,
                              text=True, check=True).stdout.strip()

    def test_an_empty_series_tags_the_base(self):
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.describe(), "v1.2.3-plurx")

    def test_patches_apply_in_series_order_not_name_order(self):
        self.patch("0002-first.patch", GIT_AM_PATCH.format(diff=diff("two", "TWO")))
        self.patch("0001-second.patch", diff("TWO", "2"))
        self.series("0002-first.patch", "0001-second.patch")
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stderr)
        with open(os.path.join(self.tree, "a.txt"), encoding="utf-8") as handle:
            self.assertEqual(handle.read(), "one\n2\nthree\n")
        self.assertRegex(self.describe(), r"^v1\.2\.3-plurx-2-g[0-9a-f]+$")

    def test_a_patch_whose_context_does_not_match_fails(self):
        self.patch("0001-fuzzy.patch", diff("zwei", "2"))
        self.series("0001-fuzzy.patch")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not apply exactly", result.stderr)

    def test_an_unlisted_patch_fails(self):
        self.patch("0001-unlisted.patch", diff("two", "2"))
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("present, unlisted: 0001-unlisted.patch", result.stderr)

    def test_a_listed_patch_that_is_missing_fails(self):
        self.series("0001-missing.patch")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("listed, missing: 0001-missing.patch", result.stderr)


if __name__ == "__main__":
    unittest.main()
