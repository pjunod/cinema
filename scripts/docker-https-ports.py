"""Resolve native HTTPS publication from the same node-local inputs as plurxd."""
from __future__ import annotations

import json
import os
from pathlib import Path
import runpy
import stat
import subprocess
import tomllib

# Reuse the startup preflight's Compose config/volume and CLI path resolution.
INPUTS = runpy.run_path(str(Path(__file__).with_name("validate-docker-startup-budget")))


class HttpsPortError(ValueError):
    pass


def require_local_filesystem() -> None:
    endpoint = "" if os.environ.get("DOCKER_CONTEXT") else os.environ.get("DOCKER_HOST", "")
    if not endpoint:
        result = subprocess.run(
            ["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"],
            capture_output=True, text=True, timeout=20, check=True,
        )
        endpoint = result.stdout.strip()
    if not endpoint.startswith(("unix://", "npipe://")):
        raise HttpsPortError(
            "automatic HTTPS publication requires local Docker bind/config files; "
            "run the Docker rollout target on the engine host"
        )


def mapped_file(document: dict, service: dict, name: str) -> Path | None:
    wanted = INPUTS["container_path"](name, str(service.get("working_dir") or "/"))
    path, opaque = INPUTS["mapped_config_file"](document, service, wanted)
    if opaque:
        raise HttpsPortError(
            f"cannot resolve HTTPS publication from opaque mount at {wanted}; "
            "use a readable bind mount or Compose file config"
        )
    if path is not None:
        require_local_filesystem()
    return path


def saved_settings(path: Path) -> dict | None:
    # Match the daemon's bounded, no-follow regular-file read. In particular a
    # FIFO must never hang the deployment helper before the server can reject it.
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, "rb") as handle:
        metadata = os.fstat(handle.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 16 * 1024:
            raise HttpsPortError("https-settings.json must be a regular file no larger than 16 KiB")
        body = handle.read(16 * 1024 + 1)
    if len(body) != metadata.st_size:
        raise HttpsPortError("https-settings.json changed during its bounded read")
    value = json.loads(body)
    if (not isinstance(value, dict) or set(value) != {"version", "revision", "settings"}
            or type(value["version"]) is not int or value["version"] != 1
            or type(value["revision"]) is not int or not 0 <= value["revision"] < 2**64
            or not isinstance(value["settings"], dict)
            or set(value["settings"]) - {"enabled", "port", "cert_file", "key_file", "sans"}):
        raise HttpsPortError("malformed or unsupported https-settings.json envelope")
    return value["settings"]


def port_number(value: object, label: str) -> int:
    if type(value) is int:
        port = value
    elif isinstance(value, str) and value.isascii() and value.isdecimal():
        port = int(value)
    else:
        raise HttpsPortError(f"{label} must be a port between 1 and 65535")
    if not 1 <= port <= 65535:
        raise HttpsPortError(f"{label} must be a port between 1 and 65535")
    return port


def redirect_environment(environment: dict[str, str], published: int, internal: int) -> dict:
    if "PLURX_HTTPS_REDIRECT_PORT" in environment:
        port_number(environment["PLURX_HTTPS_REDIRECT_PORT"], "PLURX_HTTPS_REDIRECT_PORT")
        return {}
    if published == internal:
        return {}
    return {"environment": {"PLURX_HTTPS_REDIRECT_PORT": str(published)}}


def https_port_overlay(document: dict, interpolation: dict[str, str]) -> dict:
    service = document["services"]["plurxd"]
    if service.get("network_mode") == "host":
        return {}
    environment = INPUTS["service_environment"](service)
    configuration = {}
    explicit = INPUTS["service_config_argument"](service) or environment.get("PLURX_CONFIG", "")
    for name in (explicit,) if explicit else INPUTS["DEFAULT_CONFIG_PATHS"]:
        path = mapped_file(document, service, name)
        if path is not None and path.is_file():
            with path.open("rb") as handle:
                configuration = tomllib.load(handle)
            break
        if explicit:
            raise HttpsPortError("explicit plurxd config must be a readable Compose bind or file config")
    server = configuration.get("server", {})
    storage = configuration.get("storage", {})
    if not isinstance(server, dict) or not isinstance(storage, dict):
        raise HttpsPortError("server and storage config must be TOML tables")
    settings = server.get("https", {})
    if not isinstance(settings, dict):
        raise HttpsPortError("server.https config must be a TOML table")
    data_dir = environment.get("PLURX_DATA_DIR") or storage.get("data_dir", "./data")
    path = mapped_file(document, service, str(Path(data_dir) / "https-settings.json"))
    if path is not None:
        saved = saved_settings(path)
        if saved is not None:
            settings = saved  # The whole saved object replaces TOML, before env.
    enabled = settings.get("enabled", False)
    port = settings.get("port", 32443)
    if "PLURX_HTTPS_ENABLED" in environment:
        raw = environment["PLURX_HTTPS_ENABLED"]
        if raw not in ("true", "false"):
            raise HttpsPortError("PLURX_HTTPS_ENABLED must be true or false")
        enabled = raw == "true"
    if type(enabled) is not bool:
        raise HttpsPortError("server.https.enabled must be a boolean")
    port = port_number(environment.get("PLURX_HTTPS_PORT", port), "HTTPS port")
    if not enabled:
        return {}
    # An operator's explicit host address/port remains authoritative. Compose
    # returns long-form ports, including random host-port publications; expose
    # is a separate field and does not publish a port.
    existing = [item for item in service.get("ports", [])
                if str(item.get("target")) == str(port) and item.get("protocol", "tcp") == "tcp"]
    if existing:
        if "PLURX_HTTPS_REDIRECT_PORT" in environment:
            return redirect_environment(environment, port, port)
        try:
            published_ports = {port_number(item.get("published"), "published HTTPS port") for item in existing}
        except HttpsPortError as error:
            raise HttpsPortError(
                "random HTTPS publications need an explicit PLURX_HTTPS_REDIRECT_PORT "
                "in the plurxd container environment"
            ) from error
        if len(published_ports) != 1:
            raise HttpsPortError(
                "multiple HTTPS publications need an explicit PLURX_HTTPS_REDIRECT_PORT "
                "in the plurxd container environment"
            )
        return redirect_environment(environment, published_ports.pop(), port)
    published = port_number(interpolation.get("PLURX_HTTPS_HOST_PORT") or port, "PLURX_HTTPS_HOST_PORT")
    overlay = {"ports": [{"target": port, "published": str(published), "protocol": "tcp"}]}
    overlay.update(redirect_environment(environment, published, port))
    return overlay
