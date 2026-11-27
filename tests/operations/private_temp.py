"""Keep a test module's temporary files outside the roots packaging refuses.

scripts/release-readiness runs the release gate with TMPDIR set to a host path
under the runner's work directory (/opt/forgejo-runner/...), because sibling
containers must see the same paths. The packaging scripts rightly refuse to
write under installed roots such as /opt, so a case about some other refusal
would otherwise stop at the root guard instead of reaching its own check.
"""

import tempfile
from pathlib import Path

# The union of the roots scripts/package-macos, build-macos-source-parser,
# build-linux-dolby-ffmpeg and prepare-linux-dolby-ffmpeg refuse.
REFUSED_ROOTS = ("/Applications", "/Library", "/System", "/usr", "/opt", "/lib", "/etc")


def refused(path):
    path = Path(path).resolve()
    return any(path.is_relative_to(root) for root in REFUSED_ROOTS)


def move_out_of_refused_roots():
    """Point tempfile at /tmp while the inherited TMPDIR is refused; returns the undo."""
    previous = tempfile.tempdir
    if refused(tempfile.gettempdir()):
        tempfile.tempdir = "/tmp"
    return lambda: setattr(tempfile, "tempdir", previous)
