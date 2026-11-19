"""Resolve the standard simulator runtime for the active Xcode SDK."""

import argparse
import json
import re
import subprocess

SDKS = {"iOS": "iphonesimulator", "tvOS": "appletvsimulator"}


def runtime_identifier(platform, version):
    if platform not in SDKS or not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", version):
        raise RuntimeError(f"Invalid simulator SDK: {platform} {version}")
    return f"com.apple.CoreSimulator.SimRuntime.{platform}-{version.replace('.', '-')}"


def current_runtime(platform):
    version = subprocess.check_output(
        ["xcrun", "--sdk", SDKS[platform], "--show-sdk-version"], text=True
    ).strip()
    identifier = runtime_identifier(platform, version)
    inventory = json.loads(subprocess.check_output(
        ["xcrun", "simctl", "list", "runtimes", "--json"], text=True
    ))
    for runtime in inventory["runtimes"]:
        if (runtime["identifier"] == identifier and runtime["version"] == version
                and runtime["isAvailable"] is True):
            return identifier
    raise RuntimeError(f"The active Xcode SDK requires an available {platform} {version} simulator ({identifier}).")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=SDKS)
    try:
        print(current_runtime(parser.parse_args().platform))
    except (RuntimeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
