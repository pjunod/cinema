#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Per-user registration; never installs libCEC, root services or OS key hooks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import sys

NAME = "tv.plurx.cinema_remote"
SOURCE_FILES = ("native_host.py", "errors.py", "kernel_cec.py", "libcec_backend.py")

def layout(system, browser, home):
    if browser not in ("chrome", "chromium", "edge"):
        raise ValueError("unsupported browser")
    if system == "linux":
        profile = {"chrome": "google-chrome", "chromium": "chromium", "edge": "microsoft-edge"}[browser]
        return home / ".config" / profile / "NativeMessagingHosts", None
    if system == "darwin":
        profile = {"chrome": "Google/Chrome", "chromium": "Chromium", "edge": "Microsoft Edge"}[browser]
        return home / "Library/Application Support" / profile / "NativeMessagingHosts", None
    if system == "win32":
        key = {"chrome": "Google\\Chrome", "chromium": "Chromium", "edge": "Microsoft\\Edge"}[browser]
        return None, "Software\\" + key + "\\NativeMessagingHosts\\" + NAME
    raise ValueError("unsupported platform")

def launcher(system, python, prefix):
    if system == "win32":
        values = (str(python), str(prefix / "native_host.py"), str(prefix / "config.json"))
        if any(any(c in value for c in '\r\n"%') for value in values):
            raise ValueError("unsafe Windows launcher path")
        return "launcher.cmd", '@echo off\r\n"' + values[0] + '" -I "' + values[1] + '" --config "' + values[2] + '" %*\r\n'
    return "launcher", "#!/bin/sh\nexec " + shlex.quote(str(python)) + " -I " + shlex.quote(str(prefix / "native_host.py")) + " --config " + shlex.quote(str(prefix / "config.json")) + ' "$@"\n'

def plan(system, browser, home, prefix, extension_id, backend, device, python, manifest_dir=None):
    if not re.fullmatch("[a-p]{32}", extension_id):
        raise ValueError("extension ID must be the exact 32-character browser ID")
    if backend not in ("kernel", "libcec") or not device or len(device) > 256 or any(c in device for c in "\r\n\0"):
        raise ValueError("invalid backend/device")
    if backend == "kernel" and not re.fullmatch(r"/dev/cec[0-9]+", device):
        raise ValueError("kernel backend needs one /dev/cecN")
    prefix = prefix.absolute()
    default_dir, registry = layout(system, browser, home)
    target_dir = manifest_dir or default_dir or prefix
    target_dir = target_dir.absolute()
    launch_name, launch = launcher(system, python, prefix)
    manifest = {"name": NAME, "description": "Cinema local CEC input", "path": str(prefix / launch_name),
                "type": "stdio", "allowed_origins": ["chrome-extension://" + extension_id + "/"]}
    source = Path(__file__).resolve().parent
    files = {prefix / name: (source / name).read_bytes() for name in SOURCE_FILES}
    files[prefix / launch_name] = launch.encode()
    files[prefix / "config.json"] = json.dumps({"extension_id": extension_id, "backend": backend, "device": device}, indent=2).encode()
    manifest_path = target_dir / (NAME + ".json")
    files[manifest_path] = json.dumps(manifest, indent=2).encode()
    return files, manifest_path, registry, launch_name

def digest(data):
    return hashlib.sha256(data).hexdigest()

def inspect_receipt(prefix):
    receipt_path = prefix / "installation.json"
    if not receipt_path.exists():
        return None
    if receipt_path.is_symlink() or receipt_path.stat().st_size > 16 * 1024:
        raise ValueError("invalid installation receipt")
    receipt = json.loads(receipt_path.read_text())
    allowed = {*SOURCE_FILES, "launcher", "launcher.cmd", "config.json"}
    manifest = Path(receipt["manifest"])
    if receipt.get("registry") is not None and receipt["registry"] not in WINDOWS_KEYS:
        raise ValueError("receipt names a foreign registry key")
    for value, expected in receipt["files"].items():
        path = Path(value)
        if not ((path.parent == prefix and path.name in allowed) or (path == manifest and path.name == NAME + ".json")):
            raise ValueError("receipt names a foreign file")
        if path.is_symlink():
            raise ValueError("installation contains symlinks")
        if path.exists() and digest(path.read_bytes()) != expected:
            raise ValueError("modified installed file; preserve it and resolve manually")
    return receipt

WINDOWS_KEYS = frozenset(layout("win32", browser, Path("unused"))[1] for browser in ("chrome", "chromium", "edge"))

def read_windows_registration(key):
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key) as handle:
            value, kind = winreg.QueryValueEx(handle, "")
            if kind != winreg.REG_SZ:
                raise ValueError("foreign native-host registration")
            return value
    except FileNotFoundError:
        return None

def validate_windows_registration(key, manifest, owned):
    if key not in WINDOWS_KEYS:
        raise ValueError("foreign registry key")
    current = read_windows_registration(key)
    if current is not None and (not owned or current != str(manifest)):
        raise ValueError("refusing to replace foreign native-host registration")

def register_windows(key, manifest, owned=False):
    validate_windows_registration(key, manifest, owned)
    import winreg
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, key) as handle:
        winreg.SetValueEx(handle, "", 0, winreg.REG_SZ, str(manifest))

def install(files, prefix, manifest, registry, launch_name):
    if prefix.is_symlink() or any(path.is_symlink() for path in files):
        raise ValueError("symlink installation refused")
    receipt = inspect_receipt(prefix)
    if prefix.exists() and any(prefix.iterdir()) and not receipt:
        raise ValueError("choose an empty Cinema installation directory")
    if receipt and (receipt.get("manifest") != str(manifest) or receipt.get("registry") != registry):
        raise ValueError("registration target changed; uninstall first or use a separate prefix")
    owned = set(receipt["files"]) if receipt else set()
    if registry:
        validate_windows_registration(registry, manifest, bool(receipt and receipt.get("registry") == registry and receipt.get("manifest") == str(manifest)))
    for path in files:
        if path.exists() and str(path) not in owned:
            raise ValueError("refusing to replace an unowned file")
    prefix.mkdir(parents=True, exist_ok=True)
    prefix.chmod(0o700)
    for path, data in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o700 if path.name == launch_name else 0o600)
    record = {"manifest": str(manifest), "registry": registry, "files": {str(path): digest(data) for path, data in files.items()}}
    receipt_file = prefix / "installation.json"
    receipt_file.write_text(json.dumps(record, indent=2))
    receipt_file.chmod(0o600)
    if registry:
        register_windows(registry, manifest, bool(receipt))

def uninstall(prefix):
    if prefix.is_symlink():
        raise ValueError("symlink installation refused")
    receipt = inspect_receipt(prefix)
    if not receipt:
        raise ValueError("no Cinema installation receipt")
    registry = receipt.get("registry")
    if registry:
        if sys.platform != "win32":
            raise ValueError("uninstall Windows registry on Windows")
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry) as handle:
                current, _ = winreg.QueryValueEx(handle, "")
            if current == receipt["manifest"]:
                winreg.DeleteKey(winreg.HKEY_CURRENT_USER, registry)
        except FileNotFoundError:
            pass
    for value in receipt["files"]:
        Path(value).unlink(missing_ok=True)
    (prefix / "installation.json").unlink()
    if not any(prefix.iterdir()):
        prefix.rmdir()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, required=True)
    parser.add_argument("--uninstall", action="store_true")
    parser.add_argument("--extension-id")
    parser.add_argument("--backend", choices=("kernel", "libcec"), default="libcec")
    parser.add_argument("--device")
    parser.add_argument("--browser", choices=("chrome", "chromium", "edge"), default="chrome")
    parser.add_argument("--python", type=Path, default=Path(sys.executable))
    parser.add_argument("--manifest-dir", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if hasattr(os, "geteuid") and os.geteuid() == 0:
        parser.error("install as the logged-in user")
    prefix = args.prefix.absolute()
    if args.uninstall:
        uninstall(prefix)
        print("Cinema native host uninstalled")
        return
    if not args.extension_id or not args.device:
        parser.error("--extension-id and --device are required")
    files, manifest, registry, launch_name = plan(sys.platform, args.browser, Path.home(), prefix,
        args.extension_id, args.backend, args.device, args.python.absolute(), args.manifest_dir)
    if args.dry_run:
        print(json.dumps({"files": [str(path) for path in files], "registry": registry}, indent=2))
    else:
        install(files, prefix, manifest, registry, launch_name)
        print("Cinema native host installed; bind explicitly in the extension")

if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as exc:
        print("cinema-remote installer: " + str(exc), file=sys.stderr)
        raise SystemExit(1)
