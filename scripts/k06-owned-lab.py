#!/usr/bin/env python3
"""Owned four-host measurement only. No command runs a build or changes clocks."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import secrets
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import time
import urllib.request

HOSTS = [("nynuc", "192.168.5.236"), ("m6", "192.168.4.14"),
         ("nuc4", "192.168.4.8"), ("nuc3", "192.168.4.7")]
LABEL = "tv.plurx.k06-owner"
RAW_LIMIT = 64 * 1024 * 1024
HEX = re.compile(r"^[0-9a-f]{64}$")
TMPFS = {"/tmp": "rw,nosuid,nodev,noexec,size=128m",
         "/var/lib/plurx": "rw,nosuid,nodev,noexec,size=16m"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def private_write(path, value):
    data = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    temporary = path.with_name(path.name + ".new")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(parent)
    finally:
        os.close(parent)


def validate_manifest(m):
    require(m["schema"] == 1 and HEX.fullmatch(m["owner"]), "invalid owner/schema")
    require(HEX.fullmatch(m["artifact"]["binary_sha256"]), "invalid binary digest")
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", m["artifact"]["image"]), "immutable image required")
    require(re.fullmatch(r"[0-9a-f]{40}", m["artifact"]["source"]), "full source SHA required")
    require(re.fullmatch(r"[0-9a-f]{40}", m["artifact"]["tree"]), "full tree required")
    require(HEX.fullmatch(m["artifact"]["archive_sha256"]), "source archive hash required")
    require(m["artifact"]["build"] == m["artifact"]["source"], "exact binary build stamp required")
    require(m["artifact"]["compiler"] == "rustc 1.97.1 (8bab26f4f 2026-07-14)", "wrong compiler")
    require(m["artifact"]["command"] == "cargo build --offline --locked --release -p plurxd --bin plurxd",
            "only actual default-feature daemon artifact allowed")
    artifact = m["artifact"]
    require(HEX.fullmatch(artifact.get("config_digest", "")), "independently verified OCI config digest required")
    layers = artifact.get("rootfs_diff_ids")
    require(isinstance(layers, list) and 1 <= len(layers) <= 128
            and all(isinstance(layer, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", layer)
                    for layer in layers), "independently verified ordered RootFS diffIDs required")
    image_config = artifact.get("image_config")
    require(isinstance(image_config, dict) and len(json.dumps(image_config)) <= 64 * 1024,
            "bounded independently verified image configuration required")
    validate_image_volumes(image_config)
    labels = image_config.get("Labels", {}) or {}
    require(labels.get("org.opencontainers.image.revision") == artifact["source"]
            and labels.get("tv.plurx.k06-source-tree") == artifact["tree"],
            "canonical image configuration source/tree mismatch")
    require([(n["host"], n["ip"]) for n in m["nodes"]] == HOSTS, "exact four-host scope required")
    for n in m["nodes"]:
        stem = "plurx-k06-measure." + m["owner"] + "-" + n["host"]
        require(n["root"] == "/var/tmp/" + stem, "unsafe remote path")
        require(n["name"] == stem and n["network_name"] == stem + "-net", "identity not owner-bound")
        for field in ("container_id", "network_id"):
            require(not n.get(field) or HEX.fullmatch(n[field]), "unsafe exact Docker ID")
        require(not n.get("docker_image_id") or n["docker_image_id"] in image_ids(artifact),
                "node Docker image identity outside canonical manifest/config pair")
        if n.get("container_id"):
            require(n.get("docker_image_id"), "container lacks retained per-node image authority")
    return m


def image_ids(artifact):
    return (artifact["image"], "sha256:" + artifact["config_digest"])


def validate_image_volumes(config):
    volumes = config.get("Volumes") or {}
    require(isinstance(volumes, dict) and set(volumes).issubset({"/var/lib/plurx"})
            and all(value == {} for value in volumes.values()), "unreviewed image volume declaration")


def validate_image(item, artifact):
    require(item["Id"] in image_ids(artifact) and item["Architecture"] == "amd64"
            and item["Os"] == "linux", "image outside independently verified manifest/config pair")
    require(item.get("RootFS") == {"Type": "layers", "Layers": artifact["rootfs_diff_ids"]},
            "image ordered RootFS proof mismatch")
    require(item.get("Config") == artifact["image_config"], "image canonical configuration proof mismatch")
    validate_image_volumes(item["Config"])
    return item["Id"]


def resolve_image(artifact, retained=None):
    # No tag/source-only fallback. Containerd exposes the manifest digest as
    # Docker Id, classic stores expose its referenced config digest instead.
    # Both exact objects were independently verified in the selected archive.
    if retained:
        actual = validate_image(inspect("image", retained), artifact)
        require(actual == retained, "retained per-node Docker Id changed")
        return actual
    found = set()
    for identity in dict.fromkeys(image_ids(artifact)):
        try:
            item = inspect("image", identity)
        except ValueError:
            continue  # Missing immutable lookup is allowed, never a bad proof.
        found.add(validate_image(item, artifact))
    require(len(found) == 1, "missing or ambiguous loaded immutable artifact")
    return found.pop()


def load_manifest(path):
    require(path.is_absolute() and ".." not in path.parts, "absolute manifest path required")
    require(all(not p.is_symlink() for p in [path, *path.parents]), "symlink manifest path refused")
    require(path.name == ".active-cleanup.json", "exact cleanup manifest filename required")
    for p in (path.parent, path, path.parent / ".owner"):
        s = p.stat()
        require(s.st_uid == os.getuid() and stat.S_IMODE(s.st_mode) & 0o077 == 0,
                "private controller ownership required")
        require(stat.S_ISDIR(s.st_mode) if p == path.parent else stat.S_ISREG(s.st_mode), "unsafe manifest entry type")
    require(path.stat().st_size < 1024 * 1024, "manifest byte cap")
    m = validate_manifest(json.loads(path.read_text()))
    fd = os.open(path.parent / ".owner", os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd) as stream:
        require(stream.read(65) == m["owner"] + "\n", "owner mismatch")
    return m


def daemon_args(m, n, uid, gid):
    validate_image_volumes(m["artifact"]["image_config"])
    identity = n.get("docker_image_id")
    require(identity in image_ids(m["artifact"]), "persisted per-node image preflight required before create")
    return ["docker", "create", "--name", n["name"], "--label", LABEL + "=" + m["owner"],
            "--label", "tv.plurx.k06-source=" + m["artifact"]["source"],
            "--label", "tv.plurx.k06-config=sha256:" + m["artifact"]["config_digest"],
            "--network", n["network_id"], "--user", f"{uid}:{gid}", "--cpus=2",
            "--memory=2g", "--memory-swap=2g", "--pids-limit=256", "--read-only",
            "--cap-drop=ALL", "--security-opt=no-new-privileges", "--restart=no",
            *["--tmpfs=" + path + ":" + options for path, options in TMPFS.items()],
            "--log-driver=local", "--log-opt=max-size=10m", "--log-opt=max-file=3",
            "--log-opt=compress=false", "--mount", "type=bind,src=" + n["root"] + ",dst=/data",
            "--env", "PLURX_MDNS_ADVERTISE=false", "--env", "PLURX_BIND=0.0.0.0:55420",
            "--env", "PLURX_DATA_DIR=/data/state", "--env", "PLURX_CONFIG=/data/plurx.toml",
            "--entrypoint=/usr/bin/timeout",
            *sum((["--publish", f"{n['ip']}:{p}:{p}/tcp"] for p in (55420, 55421, 55422)), []),
            identity, "-k", "10s", "5400s", "/usr/local/bin/plurxd",
            "--config", "/data/plurx.toml", "run"]


def config(n, joining):
    return (f'[server]\nname = "k06-{n["host"]}"\nbind = "0.0.0.0:55420"\n'
            '[storage]\ndata_dir = "/data/state"\n[cluster]\n'
            'raft_bind = "0.0.0.0:55421"\napi_bind = "0.0.0.0:55422"\n'
            f'advertise_host = "{n["ip"]}"\njoin_url = "http://{n["ip"]}:55420"\n'
            f'artwork_url = "http://{n["ip"]}:55420"\ntrusted_network = "192.168.0.0/16"\n'
            + ('join_token_file = "/data/join.token"\n' if joining else ''))


LIMITER_CODE = """import json,os,resource,sys
for name,value in json.loads(sys.argv[1]).items():
    resource.setrlimit(getattr(resource,name),(value,value))
os.execvp(sys.argv[2],sys.argv[2:])
"""


def limited_args(args, caps):
    # Limits run in a fresh exec'ed interpreter, never a threaded post-fork callback.
    return [sys.executable, "-c", LIMITER_CODE, json.dumps(caps, sort_keys=True), *args]


def command(args, limit=1024 * 1024, timeout=20, capture_stderr=False):
    # Temporary file prevents subprocess PIPE accumulation; reject oversized output.
    import tempfile
    with tempfile.TemporaryFile() as output:
        result = subprocess.run(limited_args(args, {"RLIMIT_FSIZE": limit}), stdout=output,
                                stderr=subprocess.STDOUT if capture_stderr else subprocess.DEVNULL,
                                timeout=timeout)
        require(output.tell() <= limit, "command output cap exceeded")
        output.seek(0)
        data = output.read(limit + 1)
    require(result.returncode == 0, "command failed: " + args[0])
    return data.decode()


POLICY_REFUSALS = frozenset((
    "available RAM below 8 GiB", "available disk below 10 GiB", "host not idle",
    "sustained host pressure", "active swap traffic", "active build campaign",
))


def remote_failure(stderr, returncode, timed_out=False):
    """Retain bounded diagnostic facts, never arbitrary remote text or inputs."""
    import tempfile
    stderr.seek(0)
    digest = hashlib.sha256()
    total = 0
    tail = b""
    while chunk := stderr.read(64 * 1024):
        digest.update(chunk)
        total += len(chunk)
        tail = (tail + chunk)[-64 * 1024:]
    # Exact whole lines only: a legitimate prefix followed by a secret is not
    # a policy reason. Untrusted SSH/worker output never becomes terminal text.
    lines = tail.splitlines()
    if total > len(tail) and lines:
        lines = lines[1:]  # The first retained line might be partial.
    allowed = {("k06-owned-lab: " + reason).encode(): reason for reason in POLICY_REFUSALS}
    reasons = sorted({allowed[line] for line in lines if line in allowed})
    sanitized = "\n".join(line.decode("ascii") if line in allowed else "<redacted>"
                          for line in lines)
    retained = sanitized.encode()[-64 * 1024:].decode("ascii")
    root = Path(tempfile.mkdtemp(prefix="k06-remote-failure-"))
    private_write(root / "stderr-evidence.json", {
        "schema": 1, "returncode": returncode, "timed_out": timed_out,
        "stderr_bytes": total, "stderr_sha256": digest.hexdigest(),
        "tail_truncated": total > 64 * 1024, "redacted_tail": retained,
        "policy_reasons": reasons, "stdout_retained": False,
    })
    reason = reasons[0] if len(reasons) == 1 and not timed_out else "remote transport refused"
    return "{}; private evidence: {}".format(reason, root / "stderr-evidence.json")


def remote(path, m, n, action, payload=None):
    # The complete controller goes over stdin, not git history or credentials.
    source = Path(__file__).read_text()
    request = json.dumps({"manifest": m, "node": n, "action": action, "payload": payload})
    wrapper = "import sys,json; source=json.loads(sys.stdin.readline()); exec(compile(source,'k06-worker','exec'))"
    args = ["ssh", "-T", "-i", str(path), "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
            "-o", "ConnectionAttempts=1", "pjunod@" + n["ip"],
            "python3 -c " + shlex.quote(wrapper) + " --worker"]
    import tempfile
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as stderr:
        try:
            result = subprocess.run(limited_args(args, {"RLIMIT_FSIZE": 2 * 1024 * 1024}),
                                    input=(json.dumps(source) + "\n" + request + "\n").encode(),
                                    stdout=output, stderr=stderr, timeout=30)
        except subprocess.TimeoutExpired:
            raise ValueError(remote_failure(stderr, None, timed_out=True)) from None
        if result.returncode != 0:
            raise ValueError(remote_failure(stderr, result.returncode))
        output.seek(0)
        return json.loads(output.read(2 * 1024 * 1024))


def inspect(kind, identity):
    return json.loads(command(["docker", kind, "inspect", identity]))[0]


def owner_check(item, m):
    labels = item.get("Config", {}).get("Labels", item.get("Labels", {})) or {}
    require(labels.get(LABEL) == m["owner"], "Docker owner mismatch")


def root_check(n, m):
    root = Path(n["root"])
    require(root.parent == Path("/var/tmp") and root.is_dir() and not root.is_symlink(), "unsafe root")
    require(root.stat().st_uid == os.getuid() and stat.S_IMODE(root.stat().st_mode) == 0o700,
            "remote root ownership/mode mismatch")
    marker = root / ".owner"
    fd = os.open(marker, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd) as stream:
        require(stream.read(65) == m["owner"] + "\n", "remote owner mismatch")
    return root


def production():
    result = {}
    for name in ("plurxd", "plurx-discovery"):
        item = inspect("container", name)
        result[name] = {"id": item["Id"], "image": item["Image"], "start": item["State"]["StartedAt"],
                        "restarts": item["RestartCount"]}
    return result


def discipline():
    sync = command(["timedatectl", "show", "-p", "NTP", "-p", "NTPSynchronized"])
    require("NTP=yes" in sync and "NTPSynchronized=yes" in sync, "clock discipline not synchronized")
    raw = command(["timedatectl", "timesync-status", "--no-pager"])
    offset = re.search(r"Offset:\s*([+-]?[0-9.]+)\s*(us|µs|μs|ms|s)\b", raw)
    require(offset is not None, "absolute discipline offset unavailable")
    scale = {"us": 0.001, "µs": 0.001, "μs": 0.001, "ms": 1, "s": 1000}
    require(abs(float(offset[1])) * scale[offset[2]] < 250, "absolute discipline bound reached 250 ms")
    return {"synchronization": sync, "timesync": raw,
            "host_boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
            "host_uptime": Path("/proc/uptime").read_text().strip(),
            "host_type": Path("/sys/class/dmi/id/product_name").read_text().strip()}


def capacity(check_swap=True):
    mem = dict(line.replace(":", "").split()[:2] for line in Path("/proc/meminfo").read_text().splitlines())
    require(int(mem["MemAvailable"]) >= 8 * 1024 * 1024, "available RAM below 8 GiB")
    require(os.statvfs("/var/tmp").f_bavail * os.statvfs("/var/tmp").f_frsize >= 10 * 1024**3,
            "available disk below 10 GiB")
    require(os.getloadavg()[0] < os.cpu_count() / 2, "host not idle")
    pressure = {}
    for kind, ceiling in (("cpu", 10.0), ("memory", 0.1), ("io", 1.0)):
        raw = Path("/proc/pressure/" + kind).read_text()
        pressure[kind] = raw
        require(all(float(re.search(r"avg10=([0-9.]+)", line)[1]) < ceiling
                    for line in raw.splitlines()), "sustained host pressure")
    def swap():
        values = dict(line.split() for line in Path("/proc/vmstat").read_text().splitlines())
        return [int(values[k]) for k in ("pswpin", "pswpout")]
    before = swap()
    if check_swap:
        time.sleep(1)
        require(before == swap(), "active swap traffic")
    active = [json.loads(line) for line in command(["docker", "ps", "--format", "{{json .}}"]).splitlines()]
    require(not any(any(marker in c["Names"].lower() for marker in ("gitea_actions", "gitea-actions", "act-", "forgejo-job"))
                    for c in active), "active build campaign")
    return {"load": os.getloadavg(), "mem_available_kib": int(mem["MemAvailable"]),
            "pressure": pressure, "swap_pages": before}


def validate_mounts(item, n):
    require(item["HostConfig"].get("Tmpfs") == TMPFS, "exact bounded noexec tmpfs required")
    mounts = item.get("Mounts")
    require(isinstance(mounts, list), "actual mount inventory required")
    binds = [mount for mount in mounts if mount.get("Type") == "bind"]
    temporary = [mount for mount in mounts if mount.get("Type") == "tmpfs"]
    require(len(binds) == 1 and binds[0].get("Source") == n["root"]
            and binds[0].get("Destination") == "/data" and binds[0].get("RW") is True,
            "exact owned data bind required")
    require(len(binds) + len(temporary) == len(mounts), "volume or other mount refused")
    # --tmpfs is represented by HostConfig.Tmpfs; engines may also expose its
    # full Mounts entries. Never accept an additional or partial inventory.
    if temporary:
        require(len(temporary) == len(TMPFS)
                and {mount.get("Destination") for mount in temporary} == set(TMPFS)
                and all(mount.get("Source") == "" and mount.get("RW") is True
                        for mount in temporary), "unexpected actual tmpfs inventory")


def validate_container(item, m, n):
    owner_check(item, m)
    require(item["Id"] == n["container_id"] and item["Image"] == n.get("docker_image_id")
            and n.get("docker_image_id") in image_ids(m["artifact"]), "container identity mismatch")
    labels = item["Config"].get("Labels", {}) or {}
    require(labels.get("tv.plurx.k06-config") == "sha256:" + m["artifact"]["config_digest"]
            and labels.get("tv.plurx.k06-source") == m["artifact"]["source"], "container artifact attribution mismatch")
    resolve_image(m["artifact"], n["docker_image_id"])
    h = item["HostConfig"]
    require(h["NanoCpus"] == 2_000_000_000 and h["Memory"] == 2 * 1024**3
            and h["MemorySwap"] == 2 * 1024**3 and h["PidsLimit"] == 256
            and h["ReadonlyRootfs"] and h["RestartPolicy"]["Name"] == "no"
            and h["CapDrop"] == ["ALL"] and not h["Privileged"] and not h["Devices"], "daemon caps mismatch")
    require(h["NetworkMode"] == n["network_id"] and "no-new-privileges" in h["SecurityOpt"], "isolation mismatch")
    require(h["PortBindings"] == {str(p) + "/tcp": [{"HostIp": n["ip"], "HostPort": str(p)}]
                                       for p in (55420, 55421, 55422)}, "unexpected publication")
    require(h["LogConfig"]["Type"] == "local" and h["LogConfig"]["Config"]["max-size"] == "10m"
            and h["LogConfig"]["Config"]["max-file"] == "3", "log caps mismatch")
    require(item["Config"]["User"] == f"{os.getuid()}:{os.getgid()}", "wrong state owner")
    validate_mounts(item, n)


def process_identity(pid):
    proc = Path("/proc") / str(pid)
    if not proc.exists():
        return None
    raw = (proc / "stat").read_text()
    return {"pid": pid, "start_ticks": raw.rsplit(")", 1)[1].split()[19],
            "exe": os.readlink(proc / "exe"),
            "argv_sha256": hashlib.sha256((proc / "cmdline").read_bytes()).hexdigest()}


def start_load(root, host):
    receiver = host == "nuc4"
    require(host in ("m6", "nuc4"), "load host outside exact pair")
    args = ["timeout", "-k", "5s", "75s" if receiver else "70s", "nice", "-n", "10", "iperf3"]
    args += (["-s", "-1", "-B", "192.168.4.8", "-p", "55423", "--server-max-duration", "60",
              "--server-bitrate-limit", "22M", "-J"] if receiver else
             ["-c", "192.168.4.8", "-B", "192.168.4.14", "-p", "55423", "-t", "60", "-b", "20M",
              "--connect-timeout", "3000", "-J"])
    caps = {"RLIMIT_AS": 256 * 1024**2, "RLIMIT_CPU": 15, "RLIMIT_FSIZE": 2 * 1024**2,
            "RLIMIT_NPROC": 256, "RLIMIT_NOFILE": 64}
    fd = os.open(root / "load.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        process = subprocess.Popen(limited_args(args, caps), stdin=subprocess.DEVNULL, stdout=fd,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
    finally:
        os.close(fd)
    deadline = time.monotonic() + 2
    expected = str(Path(shutil.which("timeout")).resolve())
    identity = process_identity(process.pid)
    while identity and identity["exe"] != expected and time.monotonic() < deadline:
        time.sleep(0.01)
        identity = process_identity(process.pid)
    require(identity and identity["exe"] == expected, "load identity unavailable")
    private_write(root / ".load-process.json", identity)
    return identity


def worker(request):
    m = validate_manifest(request["manifest"])
    n = request["node"]
    require(n in m["nodes"], "unbound host record")
    require(command(["hostname"]).strip() == n["host"], "host identity mismatch")
    action, payload = request["action"], request["payload"]
    if action == "preflight":
        facts = capacity()
        sockets = command(["ss", "-H", "-ltn"])
        require(not any(re.search(r":" + str(p) + r"\s", sockets) for p in (55420, 55421, 55422, 55423)),
                "reserved port occupied")
        publications = command(["docker", "ps", "--format", "{{.Ports}}"])
        require(not any(str(p) in publications for p in (55420, 55421, 55422, 55423)), "Docker port occupied")
        identity = resolve_image(m["artifact"])
        return {"production": production(), "capacity": facts,
                "discipline": discipline(), "docker_image_id": identity}
    if action == "claim":
        os.mkdir(n["root"], 0o700)
        root = Path(n["root"])
        fd = os.open(root / ".owner", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "w") as stream:
            stream.write(m["owner"] + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.mkdir(root / "state", 0o700)
        return {"claimed": True}
    if action == "probe-root":
        if not os.path.lexists(n["root"]):
            return {"claimed": False}
        root_check(n, m)
        return {"claimed": True}
    root = root_check(n, m)
    if action == "load-start":
        return {"load_process": start_load(root, n["host"])}
    if action == "load-result":
        deadline = time.monotonic() + 10
        while True:
            data = (root / "load.json").read_bytes()
            require(len(data) <= 2 * 1024**2, "load output cap")
            try:
                return {"load_result": json.loads(data)}
            except json.JSONDecodeError:
                require(time.monotonic() < deadline, "incomplete load output")
                time.sleep(0.1)
    if action == "export":
        import tarfile
        require(n.get("stopped"), "stop and retain state before export")
        require(not command(["docker", "ps", "-a", "--filter", "label=" + LABEL + "=" + m["owner"],
                             "--format", "{{.ID}}"]).strip(), "owned daemon still present")
        target = Path(n["root"] + ".tar")
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        total = 0
        try:
            with os.fdopen(fd, "wb") as stream, tarfile.open(fileobj=stream, mode="w") as archive:
                for entry in [root, *root.rglob("*")]:
                    require(not entry.is_symlink() and (entry.is_dir() or entry.is_file()), "unsafe state export entry")
                    total += entry.stat().st_size if entry.is_file() else 0
                    require(total <= 1024**3 - 8 * 1024**2, "state export cap")
                    archive.add(entry, arcname=entry.relative_to(root.parent), recursive=False)
            require(target.stat().st_size <= 1024**3, "archive cap")
            digest = hashlib.sha256()
            with target.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
            return {"export": {"path": str(target), "sha256": digest.hexdigest(), "bytes": target.stat().st_size}}
        except BaseException:
            # Keep a failed artifact and owner manifest; never mislabel it a successful export.
            raise
    if action == "purge":
        evidence = n.get("export", {})
        require(n.get("stopped") and evidence.get("path") == n["root"] + ".tar"
                and HEX.fullmatch(evidence.get("sha256", "")), "exact completed export required")
        require(not command(["docker", "ps", "-a", "--filter", "label=" + LABEL + "=" + m["owner"],
                             "--format", "{{.ID}}"]).strip(), "owned daemon still present")
        archive = Path(evidence["path"])
        require(not archive.is_symlink() and archive.stat().st_uid == os.getuid()
                and archive.stat().st_size == evidence["bytes"]
                and stat.S_IMODE(archive.stat().st_mode) == 0o600, "export ownership mismatch")
        digest = hashlib.sha256()
        with archive.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        require(digest.hexdigest() == evidence["sha256"], "export hash mismatch")
        require(shutil.rmtree.avoids_symlink_attacks, "fd-safe deletion unavailable")
        root_check(n, m)
        shutil.rmtree(root)  # one validated owner-marked root; export remains private
        return {"purged": True, "retained_export": evidence}
    if action == "recover":
        result = {"network_id": None, "container_id": None}
        if (root / ".load-process.json").exists():
            result["load_process"] = json.loads((root / ".load-process.json").read_text())
        # Lookup names only to recover exact IDs; labels and artifact must match before mutation.
        for kind, field, name in (("network", "network_id", n["network_name"]),
                                  ("container", "container_id", n["name"])):
            try:
                item = inspect(kind, name)
            except ValueError:
                continue
            owner_check(item, m)
            if kind == "container":
                require(item["Image"] == n.get("docker_image_id"), "recovered container artifact mismatch")
                validate_container(item, m, dict(n, container_id=item["Id"]))
                result["started"] = item["State"]["StartedAt"]
            result[field] = item["Id"]
        return result
    if action == "network":
        identity = command(["docker", "network", "create", "--driver=bridge", "--label", LABEL + "=" + m["owner"],
                            n["network_name"]]).strip()
        return {"network_id": identity}
    if action == "create":
        resolve_image(m["artifact"], n.get("docker_image_id"))
        require(n.get("docker_image_id"), "create lacks persisted node image identity")
        owner_check(inspect("network", n["network_id"]), m)
        for filename, data in (("plurx.toml", config(n, bool(payload))), ("join.token", payload or "")):
            fd = os.open(root / filename, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "w") as stream:
                stream.write(data)
        return {"container_id": command(daemon_args(m, n, os.getuid(), os.getgid())).strip()}
    if action == "cleanup":
        load = n.get("load_process")
        if load:
            current = process_identity(load["pid"])
            if current:
                require(current == load, "load PID/start/executable identity mismatch")
                os.kill(load["pid"], signal.SIGTERM)  # timeout forwards to its owned child
                deadline = time.monotonic() + 6
                while process_identity(load["pid"]) is not None:
                    require(time.monotonic() < deadline, "owned load process did not terminate")
                    time.sleep(0.1)
        if n.get("container_id"):
            item = inspect("container", n["container_id"])
            validate_container(item, m, n)
            if item["State"]["Running"]:
                command(["docker", "stop", "--time=10", n["container_id"]])
            logs = root / "daemon.log"
            require(not logs.exists(), "existing partial logs require reviewed recovery")
            data = command(["docker", "logs", "--timestamps", n["container_id"]],
                           limit=32 * 1024**2, capture_stderr=True)
            fd = os.open(logs, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "w") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            command(["docker", "rm", n["container_id"]])
        if n.get("network_id"):
            network = inspect("network", n["network_id"])
            owner_check(network, m)
            require(not network.get("Containers"), "network has foreign endpoints")
            command(["docker", "network", "rm", n["network_id"]])
        require(production() == n["production"], "production identity changed")
        sockets = command(["ss", "-H", "-ltn"])
        require(not any(re.search(r":" + str(p) + r"\s", sockets) for p in (55420, 55421, 55422, 55423)),
                "owned ports not released")
        return {"stopped": True, "retained_root": str(root),
                "log_sha256": hashlib.sha256((root / "daemon.log").read_bytes()).hexdigest()
                if (root / "daemon.log").exists() else None}
    item = inspect("container", n["container_id"])
    validate_container(item, m, n)
    if action == "start":
        command(["docker", "start", n["container_id"]])
        actual_hash = command(["docker", "exec", n["container_id"], "sha256sum", "/usr/local/bin/plurxd"]).split()[0]
        require(actual_hash == m["artifact"]["binary_sha256"], "executing binary digest mismatch")
        return {"started": inspect("container", n["container_id"])["State"]["StartedAt"]}
    if action == "sample":
        require(item["State"]["Running"] and item["RestartCount"] == 0
                and item["State"]["StartedAt"] == n["started"], "daemon reset")
        size = int(command(["du", "-sb", n["root"]]).split()[0])
        require(size < 1024**3, "state cap exceeded")
        return {"container_id": item["Id"], "started": item["State"]["StartedAt"],
                "state_bytes": size, "capacity": capacity(False),
                "stats": command(["docker", "stats", "--no-stream", "--format", "{{json .}}", n["container_id"]]),
                "discipline": discipline()}
    raise ValueError("unknown worker operation")


def api(n, endpoint, token=None, body=None):
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            raise ValueError("lab HTTP redirect refused")
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request("http://" + n["ip"] + ":55420" + endpoint,
                                     data=None if body is None else json.dumps(body).encode(), headers=headers)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(request, timeout=3) as response:
        data = response.read(512 * 1024 + 1)
    require(len(data) <= 512 * 1024, "HTTP output cap")
    return data.decode()


def sample(m, key, token, runtime=True):
    def one(n):
        facts = remote(key, m, n, "sample") if runtime else None
        metrics = api(n, "/metrics")
        return {"host": n["host"], "monotonic_ns": time.monotonic_ns(),
                "runtime": facts, "metrics": metrics,
                "system": json.loads(api(n, "/api/v1/system", token)),
                "roster": json.loads(api(n, "/api/v1/cluster/nodes", token))}
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        return list(pool.map(one, m["nodes"]))


def check_rows(rows, m):
    for row in rows:
        nodes = row["roster"]["nodes"]
        require(len(nodes) == 4 and all(n["is_voter"] and n["reachable"]
                                        and not n["removal_pending"] for n in nodes), "incomplete voter coverage")
        require(row["system"]["build"] == m["artifact"]["build"], "runtime build mismatch")
        clock = row["clock_metrics"]
        expected = {n["node_id"] for n in nodes} - {row["roster"]["local_node_id"]}
        bounded = set(re.findall(r'plurx_cluster_clock_observation_state\{peer="([^"]+)",state="bounded"\} 1\n', clock + "\n"))
        require(bounded == expected, "incomplete signed observer coverage")
        offsets = dict((peer, float(value)) for peer, value in re.findall(
            r'plurx_cluster_clock_offset_seconds\{peer="([^"]+)"\} ([^\n]+)', clock))
        uncertainties = dict((peer, float(value)) for peer, value in re.findall(
            r'plurx_cluster_clock_offset_uncertainty_seconds\{peer="([^"]+)"\} ([^\n]+)', clock))
        require(set(offsets) == set(uncertainties) == expected, "missing numeric observation")
        require(all(0 <= uncertainties[p] and abs(offsets[p]) + uncertainties[p] < 2 for p in expected),
                "healthy upper bound reached 2,000 ms")


def observe(m, key, token, output, seconds, cadence):
    require(not output.exists(), "never overwrite an observation")
    fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    start = time.monotonic()
    counters, swaps = m.get("clock_totals", {}), {}
    cached = m.get("last_runtime", {})
    pending = {}
    with os.fdopen(fd, "wb") as stream, concurrent.futures.ThreadPoolExecutor(max_workers=4) as runtime_pool:
        for index in range(seconds // cadence + 1):
            delay = start + index * cadence - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            late = time.monotonic() - (start + index * cadence)
            rows = sample(m, key, token, runtime=cadence == 10)
            if cadence == 1:
                for host, future in list(pending.items()):
                    if future.done():
                        cached[host] = {"at": time.monotonic_ns(), "value": future.result()}
                        del pending[host]
                if index % 10 == 0:
                    for n in m["nodes"]:
                        require(n["host"] not in pending, "runtime collection gap")
                        pending[n["host"]] = runtime_pool.submit(remote, key, m, n, "sample")
            for row in rows:
                row["clock_metrics"] = "\n".join(line for line in row.pop("metrics").splitlines()
                                                   if line.startswith("plurx_cluster_clock_"))
                if row["runtime"]:
                    cached[row["host"]] = {"at": time.monotonic_ns(), "value": row["runtime"]}
                else:
                    row["runtime"] = cached.get(row["host"], {}).get("value")
                row["runtime_observed_ns"] = cached.get(row["host"], {}).get("at")
            # Retain the actual failing sample before judging it.
            data = (json.dumps({"index": index, "late_seconds": late, "nodes": rows}) + "\n").encode()
            existing = sum(p.stat().st_size for p in output.parent.glob("*.jsonl"))
            require(existing + len(data) <= RAW_LIMIT, "raw evidence cap")
            stream.write(data)
            stream.flush()
            require(late < 2, "collection gap; retain incomplete evidence")
            check_rows(rows, m)
            for row in rows:
                host = row["host"]
                totals = [float(re.search(k + r" ([^\n]+)", row["clock_metrics"])[1])
                          for k in ("plurx_cluster_clock_discontinuities_total", "plurx_cluster_clock_unknown_rounds_total")]
                require(host not in counters or counters[host] == totals, "clock reset/discontinuity/unknown round")
                counters[host] = totals
                require(row["runtime"] and 0 <= time.monotonic_ns() - row["runtime_observed_ns"] < 15 * 10**9,
                        "runtime facts expired")
                pages = row["runtime"]["capacity"]["swap_pages"]
                require(host not in swaps or swaps[host] == pages, "swap traffic during observation")
                swaps[host] = pages
    m["clock_totals"] = counters
    m["last_runtime"] = cached


def check_load(result, host):
    require("error" not in result and result.get("connected"), "failed or unsolicited load")
    connection = result["connected"][0]
    local, peer = ("192.168.4.14", "192.168.4.8") if host == "m6" else ("192.168.4.8", "192.168.4.14")
    require(connection["local_host"] == local and connection["remote_host"] == peer, "load peer mismatch")
    summary = result["end"]["sum_sent" if host == "m6" else "sum_received"]
    require(59 <= summary["seconds"] <= 62 and 100_000_000 <= summary["bytes"] <= 180_000_000
            and 18_000_000 <= summary["bits_per_second"] <= 22_000_000, "short/unbounded/wrong-rate load")


def prepare_source(repo, output):
    import tarfile
    require(repo.is_absolute() and output.is_absolute() and not output.exists(), "fresh absolute archive output required")
    require(".." not in output.parts and all(not p.is_symlink() for p in output.parents), "unsafe archive output path")
    require(not command(["git", "-C", str(repo), "status", "--porcelain"]).strip(), "clean committed source required")
    source = command(["git", "-C", str(repo), "rev-parse", "HEAD"]).strip()
    tree = command(["git", "-C", str(repo), "rev-parse", "HEAD^{tree}"]).strip()
    output.mkdir(mode=0o700)
    archive = output / "source.tar"
    command(["git", "-C", str(repo), "archive", "--format=tar", "--output=" + str(archive), source])
    archive.chmod(0o600)
    with tarfile.open(archive) as tar:
        total = 0
        for entry in tar:
            parts = Path(entry.name).parts
            require(not entry.name.startswith("/") and ".." not in parts
                    and (entry.isfile() or entry.isdir()), "unsafe source archive member")
            require(not set(parts) & {".git", ".ssh", "forgejo_token", ".ssh-deploy-key", ".env", "id_rsa", "id_ed25519"},
                    "credential/history in archive")
            total += entry.size
            require(total <= 1024**3, "expanded source archive cap")
    receipt = {"source": source, "tree": tree, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
               "compiler": "rustc 1.97.1 (8bab26f4f 2026-07-14)", "build": source,
               "command": "cargo build --offline --locked --release -p plurxd --bin plurxd",
               "state": "source-only-not-built-not-authorized-for-launch"}
    private_write(output / "source-receipt.json", receipt)
    return receipt


def summarize(path, m):
    import math
    result = {"source": m["artifact"], "phase": m["phase"], "qualified": False,
              "observer_roster_reads": {"source_calls_per_round": 2, "measured_counter_available": False},
              "collector_admin_requests_per_snapshot_per_node": 2, "windows": {}}
    for filename, expected in (("idle.jsonl", 361), ("load.jsonl", 61)):
        raw = path.parent / filename
        require(not raw.is_symlink() and raw.stat().st_size <= RAW_LIMIT, "unsafe raw evidence")
        rows = [json.loads(line) for line in raw.read_text().splitlines()]
        window = {"samples": len(rows), "expected_samples": expected, "complete": len(rows) == expected,
                  "raw_sha256": hashlib.sha256(raw.read_bytes()).hexdigest(), "nodes": {}}
        for host, _ in HOSTS:
            samples = [n for r in rows for n in r["nodes"] if n["host"] == host]
            distributions = {"abs_offset_seconds": [], "uncertainty_seconds": [], "abs_upper_seconds": []}
            for sample_row in samples:
                text = sample_row["clock_metrics"]
                offsets = dict((p, abs(float(v))) for p, v in re.findall(
                    r'plurx_cluster_clock_offset_seconds\{peer="([^"]+)"\} ([^\n]+)', text))
                uncertainty = dict((p, float(v)) for p, v in re.findall(
                    r'plurx_cluster_clock_offset_uncertainty_seconds\{peer="([^"]+)"\} ([^\n]+)', text))
                for peer in offsets.keys() & uncertainty.keys():
                    distributions["abs_offset_seconds"].append(offsets[peer])
                    distributions["uncertainty_seconds"].append(uncertainty[peer])
                    distributions["abs_upper_seconds"].append(offsets[peer] + uncertainty[peer])
            stats = {}
            for name, values in distributions.items():
                values.sort()
                stats[name] = {"count": len(values), "min": values[0], "max": values[-1],
                               "p50": values[math.ceil(len(values) * 0.5) - 1],
                               "p95": values[math.ceil(len(values) * 0.95) - 1]} if values else {"count": 0}
            duration = (samples[-1]["monotonic_ns"] - samples[0]["monotonic_ns"]) / 1e9 if len(samples) > 1 else 0
            def counter(row, family):
                match = re.search(family + r" ([^\n]+)", row["clock_metrics"])
                return float(match[1]) if match else None
            first = counter(samples[0], "plurx_cluster_clock_authority_reads_total") if samples else None
            last = counter(samples[-1], "plurx_cluster_clock_authority_reads_total") if samples else None
            window["nodes"][host] = {"distributions": stats, "monotonic_seconds": duration,
                                     "authority_reads_start": first, "authority_reads_end": last,
                                     "authority_reads_per_second": (last - first) / duration
                                     if first is not None and last is not None and duration > 0 and last >= first else None,
                                     "unknown_sample_count": sum('state="unknown"} 1' in s["clock_metrics"] for s in samples),
                                     "actual_samples": len(samples)}
        result["windows"][filename] = window
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("source", "plan", "validate", "report", "launch", "collect", "load", "cleanup", "export", "purge"))
    parser.add_argument("path", type=Path)
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--repo", type=Path)
    parser.add_argument("--ssh-key", type=Path)
    parser.add_argument("--authorized-window", help="separate coordinator authorization receipt; never a product gate")
    args = parser.parse_args()
    if args.action == "source":
        require(args.repo is not None, "owned clone path required")
        print(json.dumps(prepare_source(args.repo, args.path)))
        return
    if args.action == "plan":
        require(args.artifact is not None, "identified artifact receipt required")
        require(args.path.is_absolute() and not args.path.exists() and ".." not in args.path.parts,
                "fresh absolute output required")
        require(all(not p.is_symlink() for p in args.path.parents), "symlink output ancestor")
        args.path.mkdir(mode=0o700)
        owner = secrets.token_hex(32)
        m = {"schema": 1, "owner": owner, "artifact": json.loads(args.artifact.read_text()), "phase": "planned",
             "nodes": [{"host": host, "ip": ip, "root": "/var/tmp/plurx-k06-measure." + owner + "-" + host,
                        "name": "plurx-k06-measure." + owner + "-" + host,
                        "network_name": "plurx-k06-measure." + owner + "-" + host + "-net"}
                       for host, ip in HOSTS]}
        validate_manifest(m)
        fd = os.open(args.path / ".owner", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as stream:
            stream.write(owner + "\n")
        private_write(args.path / ".active-cleanup.json", m)
        print("planned only; no remote operation")
        return
    m = load_manifest(args.path)
    if args.action == "validate":
        print("valid owned manifest; no remote operation")
        return
    if args.action == "report":
        private_write(args.path.parent / "measurement-summary.json", summarize(args.path, m))
        print("summary recorded; not qualification or eligibility")
        return
    require(args.authorized_window and args.ssh_key, "separate operational authorization and local SSH key required")
    require(args.ssh_key.is_absolute(), "absolute local key path required")
    m["authorized_window"] = args.authorized_window
    private_write(args.path, m)
    if args.action == "launch":
        require(m["phase"] == "planned", "fresh launch only; recover partial state from manifest")
        # Obtain every baseline before creating any lab object.
        for n in m["nodes"]:
            n.update(remote(args.ssh_key, m, n, "preflight"))
            private_write(args.path, m)
        m["phase"] = "launching"
        private_write(args.path, m)
        token = None
        for index, n in enumerate(m["nodes"]):
            n.update(remote(args.ssh_key, m, n, "claim"))
            private_write(args.path, m)
            n.update(remote(args.ssh_key, m, n, "network"))
            private_write(args.path, m)
            join = None if index == 0 else json.loads(api(m["nodes"][0], "/api/v1/cluster/join-tokens", token,
                                                         {"expires_in_seconds": 600}))["token"]
            n.update(remote(args.ssh_key, m, n, "create", join))
            private_write(args.path, m)
            n.update(remote(args.ssh_key, m, n, "start"))
            private_write(args.path, m)
            deadline = time.monotonic() + 180
            while True:
                try:
                    api(n, "/healthz")
                    break
                except (OSError, ValueError):
                    require(time.monotonic() < deadline, "startup timeout; cleanup manifest retained")
                    time.sleep(2)
            if index == 0:
                token = json.loads(api(n, "/api/v1/setup", body={"username": "k06-lab-admin",
                                                               "password": secrets.token_urlsafe(32)}))["token"]
                private_write(args.path.parent / ".lab-admin.json", {"token": token})
        m["phase"] = "launched"
        private_write(args.path, m)
    elif args.action == "collect":
        require(m["phase"] == "launched", "launched manifest required")
        token = json.loads((args.path.parent / ".lab-admin.json").read_text())["token"]
        observe(m, args.ssh_key, token, args.path.parent / "idle.jsonl", 3600, 10)
        m["phase"] = "idle-collected-load-not-executed"
        private_write(args.path, m)
    elif args.action == "load":
        require(m["phase"] == "idle-collected-load-not-executed", "complete idle window required first")
        token = json.loads((args.path.parent / ".lab-admin.json").read_text())["token"]
        pair = [m["nodes"][2], m["nodes"][1]]
        for n in pair:
            n.update(remote(args.ssh_key, m, n, "load-start"))
            private_write(args.path, m)
        observe(m, args.ssh_key, token, args.path.parent / "load.jsonl", 60, 1)
        for n in pair:
            n.update(remote(args.ssh_key, m, n, "load-result"))
            private_write(args.path, m)
            check_load(n["load_result"], n["host"])
        m["phase"] = "collected-not-qualified"
        private_write(args.path, m)
    else:
        failures = []
        for n in m["nodes"]:
            if n.get("purged"):
                continue
            try:
                if args.action == "cleanup":
                    n.update(remote(args.ssh_key, m, n, "probe-root"))
                    private_write(args.path, m)
                    if not n["claimed"]:
                        continue
                    n.update(remote(args.ssh_key, m, n, "recover"))
                    private_write(args.path, m)
                n.update(remote(args.ssh_key, m, n, args.action))
                private_write(args.path, m)
            except (ValueError, OSError, subprocess.SubprocessError) as error:
                failures.append(n["host"] + ": " + str(error))
        require(not failures, "; ".join(failures) + "; recovery manifest retained")
        m["phase"] = {"cleanup": "stopped-state-retained", "export": "exported-private-state",
                      "purge": "purged-private-exports-retained"}[args.action]
        private_write(args.path, m)
        print(m["phase"] + "; recovery manifest retained")


if __name__ == "__main__":
    try:
        if "--worker" in sys.argv:
            print(json.dumps(worker(json.loads(sys.stdin.readline()))))
        else:
            main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print("k06-owned-lab: " + str(error), file=sys.stderr)
        sys.exit(2)
