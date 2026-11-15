"""Select only prior-attempt simulators owned by the current CI run."""

from __future__ import annotations

import argparse
import json
import re
import sys
import uuid
from typing import Any


DEVICE_NAME = re.compile(r"plurx CI (?:iPhone|iPad|Apple TV) ([1-9][0-9]*)-([1-9][0-9]*)\Z")


def _positive_decimal(value: str, label: str) -> int:
    if not re.fullmatch(r"[1-9][0-9]*", value):
        raise ValueError(f"{label} must be a positive decimal integer")
    return int(value)


def prior_owned_device_ids(
    payload: str, run_id: str, current_attempt: str
) -> list[str]:
    """Return valid UUIDs for exact-name devices from this run's prior attempts."""
    _positive_decimal(run_id, "run ID")
    attempt = _positive_decimal(current_attempt, "run attempt")
    data: Any = json.loads(payload)
    if not isinstance(data, dict) or not isinstance(data.get("devices"), dict):
        raise ValueError("simctl JSON must contain a devices object")

    selected: list[str] = []
    for runtime_devices in data["devices"].values():
        if not isinstance(runtime_devices, list):
            raise ValueError("simctl runtime devices must be lists")
        for device in runtime_devices:
            if not isinstance(device, dict):
                raise ValueError("simctl device entries must be objects")
            name = device.get("name")
            if not isinstance(name, str):
                raise ValueError("simctl device name must be a string")
            match = DEVICE_NAME.fullmatch(name)
            if not match:
                continue
            named_run, named_attempt = match.groups()
            if named_run != run_id or int(named_attempt) >= attempt:
                continue
            raw_id = device.get("udid")
            if not isinstance(raw_id, str):
                raise ValueError("owned prior-attempt simulator has no UUID")
            try:
                parsed_id = uuid.UUID(raw_id)
            except (ValueError, AttributeError) as error:
                raise ValueError("owned prior-attempt simulator has an invalid UUID") from error
            if str(parsed_id).upper() != raw_id.upper():
                raise ValueError("owned prior-attempt simulator UUID is not canonical")
            selected.append(str(parsed_id).upper())
    return selected


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--attempt", required=True)
    args = parser.parse_args()
    try:
        ids = prior_owned_device_ids(sys.stdin.read(), args.run_id, args.attempt)
    except (ValueError, json.JSONDecodeError) as error:
        print(f"simulator ownership selection failed closed: {error}", file=sys.stderr)
        return 2
    if ids:
        print("\n".join(ids))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
