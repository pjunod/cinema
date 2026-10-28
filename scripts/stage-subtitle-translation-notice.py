#!/usr/bin/env python3
"""Stage the production self-test fixture attribution and its full license."""

import json
from pathlib import Path
import sys


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: stage-subtitle-translation-notice.py SOURCE_ROOT STAGE_DIR")
    source_root = Path(sys.argv[1])
    stage = Path(sys.argv[2])
    reference = json.loads(
        (source_root / "tests/fixtures/subtitle-translation/references/fleurs-de-1586.json").read_text()
    )
    required = (
        "dataset",
        "revision",
        "config",
        "split",
        "sentence_id",
        "archive_member",
        "attribution",
        "publisher_card_url",
        "license",
        "reference_origin",
    )
    if any(not isinstance(reference.get(key), str) or not reference[key] for key in required):
        raise SystemExit("subtitle translation fixture attribution is incomplete")
    if reference["dataset"] != "Google FLEURS" or reference["license"] != "CC BY4.0":
        raise SystemExit("unexpected subtitle translation fixture or license")

    license_url = "https://creativecommons.org/licenses/by/4.0/"
    notice = "\n".join(
        (
            "FLEURS recording used by the subtitle translation self-test",
            f"Dataset: {reference['dataset']}, {reference['config']} {reference['split']} split, sentence {reference['sentence_id']}",
            f"Dataset revision: {reference['revision']}",
            f"Original archive member: {reference['archive_member']}",
            f"Attribution: {reference['attribution']}",
            f"Dataset card: {reference['publisher_card_url']}",
            f"License: {reference['license']} ({license_url})",
            f"Reference provenance: {reference['reference_origin']}",
            "The recording is used unmodified; decoded/resampled audio is kept private during the self-test.",
            "The complete license text is in CC-BY-4.0.txt beside this notice.",
            "",
        )
    )
    stage.mkdir(parents=True, exist_ok=True)
    (stage / "subtitle-translation-notice.txt").write_text(notice)
    (stage / "CC-BY-4.0.txt").write_text(
        (source_root / "licenses/CC-BY-4.0.txt").read_text()
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
