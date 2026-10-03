#!/usr/bin/env python3
"""Qualify archived historical daemons and isolated coordinated-upgrade fixtures."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import queue
import re
import shutil
import signal
import socket
import sqlite3
import subprocess
import tarfile
import threading
import time
import urllib.error
import urllib.request

BASELINE = "971265536a259dea38b0f7a9a8752a5a74e8c025"
ROOT = Path(__file__).resolve().parent.parent
PREFIX = "QUALIFICATION "
TERMINAL_FIXTURE_LEASE = "session:00000000-0000-4000-a000-000000000072"


def archive_source(reference, destination):
    payload = subprocess.check_output(["git", "archive", "--format=tar", reference], cwd=ROOT)
    with tarfile.open(fileobj=io.BytesIO(payload)) as archive:
        if any(".git" in Path(member.name).parts for member in archive.getmembers()):
            raise RuntimeError("source archive contains Git metadata")
        destination.mkdir()
        archive.extractall(destination, filter="data")
    return hashlib.sha256(payload).hexdigest()


def build(source, target, arguments, log):
    env = dict(os.environ, CARGO_TARGET_DIR=str(target.resolve()),
               CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="2")
    command = ["cargo", "build", "--locked", "--offline", *arguments]
    print("BUILD " + " ".join(command), flush=True)
    with log.open("w") as output:
        result = subprocess.run(command, cwd=source, env=env, stdout=output,
                                stderr=subprocess.STDOUT, check=False)
    if result.returncode:
        raise RuntimeError(f"compiler failed; inspect {log}")


def ports(count):
    held = [socket.socket() for _ in range(count)]
    try:
        for listener in held:
            listener.bind(("127.0.0.1", 0))
        return [listener.getsockname()[1] for listener in held]
    finally:
        for listener in held:
            listener.close()


def config(root, number, allocated):
    data = root / f"node-{number}"
    data.mkdir()
    path = root / f"node-{number}.toml"
    http, raft, api = allocated
    path.write_text(f'[server]\nbind="127.0.0.1:{http}"\n'
                    f'[storage]\ndata_dir={json.dumps(str(data))}\n'
                    f'[cluster]\nraft_bind="127.0.0.1:{raft}"\n'
                    f'api_bind="127.0.0.1:{api}"\nadvertise_host="127.0.0.1"\n')
    return {"number": number, "data": data, "config": path,
            "ports": allocated, "base": f"http://127.0.0.1:{http}"}


def request(node, route, payload=None, token=None):
    headers = {}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    operation = urllib.request.Request(node["base"] + route,
                data=None if payload is None else json.dumps(payload).encode(), headers=headers)
    with urllib.request.urlopen(operation, timeout=5) as response:
        body = response.read(1024 * 1024)
        return json.loads(body) if body else None


class Daemon:
    def __init__(self, binary, node, label):
        self.node = node
        self.log = node["config"].parent / f"{label}-node-{node['number']}.log"
        self.output = self.log.open("w")
        self.process = subprocess.Popen([str(binary), "--config", str(node["config"]), "run"],
                        stdin=subprocess.DEVNULL, stdout=self.output, stderr=subprocess.STDOUT)

    def ready(self):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f"daemon exited before readiness; inspect {self.log}")
            try:
                with urllib.request.urlopen(self.node["base"] + "/readyz", timeout=5) as response:
                    if response.status == 200 and response.read(1024) == b"ready\n":
                        return
            except (OSError, ValueError, urllib.error.HTTPError):
                time.sleep(0.1)
        raise RuntimeError(f"daemon readiness timed out; inspect {self.log}")

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)

    def wait(self):
        try:
            status = self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise RuntimeError(f"daemon failed to drain; inspect {self.log}")
        finally:
            self.output.close()
        if status != 0:
            raise RuntimeError(f"daemon shutdown was {status}; inspect {self.log}")

    def kill(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.output.close()


def stop_all(children):
    for child in children:
        child.stop()
    for child in children:
        child.wait()
    children.clear()


class Coordinator:
    def __init__(self, binary, node, label):
        self.log = node["config"].parent / f"{label}-coordinator-{node['number']}.log"
        self.errors = self.log.open("w")
        self.process = subprocess.Popen([str(binary), "node", str(node["config"])],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors, text=True)
        self.responses = queue.Queue()
        def read():
            for line in self.process.stdout:
                if line.startswith(PREFIX):
                    self.responses.put(json.loads(line[len(PREFIX):]))
            self.responses.put(None)
        threading.Thread(target=read, daemon=True).start()

    def response(self):
        value = self.responses.get(timeout=90)
        if value is None:
            raise RuntimeError(f"coordinator exited; inspect {self.log}")
        return value

    def command(self, command):
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()
        return self.response()

    def stop(self):
        self.process.stdin.write("quit\n")
        self.process.stdin.flush()

    def wait(self):
        status = self.process.wait(timeout=60)
        self.errors.close()
        if status:
            raise RuntimeError(f"coordinator drain was {status}; inspect {self.log}")

    def kill(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.errors.close()


def future_sqlite(binary, helper, root, label):
    root.mkdir()
    node = config(root, 1, ports(3))
    database = node["data"] / "plurx.db"
    subprocess.run([str(helper), "sqlite-init", str(database)], check=True, capture_output=True)
    sentinel = node["data"] / "sessions" / "pre-migration" / "keep"
    sentinel.parent.mkdir(parents=True)
    sentinel.write_text("preserve before future-version refusal\n")
    with sqlite3.connect(database) as connection:
        version = connection.execute("PRAGMA user_version").fetchone()[0]
        connection.execute(f"PRAGMA user_version={version + 1}")
        before = connection.execute("SELECT COUNT(*) FROM users").fetchone()[0]
    outcome = subprocess.run([str(binary), "--config", str(node["config"]), "run"],
              capture_output=True, text=True, timeout=30)
    (root / "refusal.log").write_text(outcome.stdout + outcome.stderr)
    expected = f"source database schema is v{version + 1}"
    if outcome.returncode == 0 or expected not in outcome.stderr or "only knows" not in outcome.stderr:
        raise RuntimeError(f"future SQLite refusal was not proven for {label}: {root / 'refusal.log'}")
    if sentinel.read_text() != "preserve before future-version refusal\n" or (node["data"] / "hiqlite").exists():
        raise RuntimeError("future refusal changed pre-migration artifacts")
    with sqlite3.connect(database) as connection:
        assert connection.execute("PRAGMA user_version").fetchone()[0] == version + 1
        assert connection.execute("SELECT COUNT(*) FROM users").fetchone()[0] == before
    for port in node["ports"]:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", port))
    return {"daemon": label, "source_version": version + 1,
            "refused_before_http_and_raft": True, "source_preserved": True}


def future_replicated(binary, helper, root, label):
    root.mkdir()
    node = config(root, 1, ports(3))
    coordinator = Coordinator(helper, node, "future-seed")
    try:
        assert coordinator.response()["ready"]
        version = coordinator.command("future")["future_replicated_version"]
        coordinator.stop()
        coordinator.wait()
    finally:
        coordinator.kill()
    sentinel = node["data"] / "sessions" / "pre-restart" / "keep"
    sentinel.parent.mkdir(parents=True)
    sentinel.write_text("preserve incompatible replicated source\n")
    outcome = subprocess.run([str(binary), "--config", str(node["config"]), "run"],
              capture_output=True, text=True, timeout=45)
    (root / "refusal.log").write_text(outcome.stdout + outcome.stderr)
    output = outcome.stdout + outcome.stderr
    if outcome.returncode == 0 or f"cluster schema {version}" not in output or "voter schema" not in output:
        raise RuntimeError(f"future replicated refusal was not proven: {root / 'refusal.log'}")
    if sentinel.read_text() != "preserve incompatible replicated source\n":
        raise RuntimeError("future replicated refusal removed the source sentinel")
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", node["ports"][0]))
    return {"daemon": label, "source_version": version, "refused_before_http": True,
            "source_sentinel_preserved": True, "raft_startup_required": True}


def compare_retained(before, after, terminal_cleanup=False):
    if terminal_cleanup:
        original = [row for row in before["job_leases"]["rows"]
                    if row["resource"] == TERMINAL_FIXTURE_LEASE]
        surviving = [row for row in after["job_leases"]["rows"]
                     if row["resource"] == TERMINAL_FIXTURE_LEASE]
        if len(original) != 1 or surviving:
            raise RuntimeError("ended fixture session lease cleanup was not proven")
    for table, original in before.items():
        if terminal_cleanup and table == "job_leases":
            # Other resources belong to daemon workers. Their lease clocks,
            # revisions and expirations are mutable runtime state; both full
            # inventories remain in the receipt artifacts for inspection.
            continue
        columns = original["columns"]
        retained = [{column: row[column] for column in columns} for row in after[table]["rows"]]
        if sorted(map(lambda row: json.dumps(row, sort_keys=True), retained)) != sorted(
                map(lambda row: json.dumps(row, sort_keys=True), original["rows"])):
            raise RuntimeError(f"retained row difference: {table}")


def cluster_fixture(old_binary, new_binary, helper, root):
    root.mkdir()
    allocated = ports(9)
    nodes = [config(root, number + 1, allocated[number * 3:number * 3 + 3]) for number in range(3)]
    children, coordinators = [], []
    try:
        children.append(Daemon(old_binary, nodes[0], "historical-bootstrap"))
        children[0].ready()
        owner = request(nodes[0], "/api/v1/setup", {"username": "owner", "password": "coordinated-fixture-only"})["token"]
        for node in nodes[1:]:
            bearer = request(nodes[0], "/api/v1/cluster/join-tokens", {"expires_in_seconds": 600}, owner)["token"]
            join = root / f"node-{node['number']}.join"
            join.write_text(bearer + "\n")
            join.chmod(0o600)
            with node["config"].open("a") as output:
                output.write(f"join_token_file={json.dumps(str(join))}\n")
            children.append(Daemon(old_binary, node, "historical-bootstrap"))
            children[-1].ready()
        roster = request(nodes[0], "/api/v1/cluster/nodes", token=owner)
        if len([node for node in roster["nodes"] if node["role"] == "voter" and node["is_voter"]]) != 3:
            raise RuntimeError("historical daemon fixture never formed three voters")
        stop_all(children)
        coordinators = [Coordinator(helper, node, "retained-seed") for node in nodes]
        for process in coordinators:
            assert process.response()["ready"]
        assert coordinators[0].command("seed")["seeded"]
        before = coordinators[0].command("snapshot")["snapshot"]
        (root / "retained-before.json").write_text(json.dumps(before, indent=2, sort_keys=True) + "\n")
        stop_all(coordinators)
        # All writers are stopped before copying a whole topology. No live DB,
        # WAL, or Raft log is copied independently of the other stopped voters.
        backup = root / "pre-upgrade-backup"
        backup.mkdir()
        for node in nodes:
            shutil.copytree(node["data"], backup / node["data"].name)
        coordinators = [Coordinator(helper, node, "candidate-install") for node in nodes]
        for process in coordinators:
            assert process.response()["ready"]
        assert coordinators[0].command("factory")["voters"] == 3
        assert coordinators[0].command("rebuild")["capabilities_advertised"] is False
        rebuilt = coordinators[0].command("snapshot")["snapshot"]
        (root / "retained-rebuilt.json").write_text(json.dumps(rebuilt, indent=2, sort_keys=True) + "\n")
        compare_retained(before, rebuilt)
        stop_all(coordinators)
        children = [Daemon(new_binary, node, "candidate-restart") for node in nodes]
        for child in children:
            child.ready()
        stop_all(children)
        coordinators = [Coordinator(helper, node, "candidate-retention") for node in nodes]
        for process in coordinators:
            assert process.response()["ready"]
        restarted = coordinators[0].command("snapshot")["snapshot"]
        (root / "retained-restarted.json").write_text(json.dumps(restarted, indent=2, sort_keys=True) + "\n")
        compare_retained(before, restarted, terminal_cleanup=True)
        stop_all(coordinators)
        parked = root / "parked-candidate"
        parked.mkdir()
        for node in nodes:
            node["data"].rename(parked / node["data"].name)
            shutil.copytree(backup / node["data"].name, node["data"])
        children = [Daemon(old_binary, node, "historical-restored") for node in nodes]
        for child in children:
            child.ready()
        restored_owner = request(nodes[0], "/api/v1/auth/login", {"username": "owner", "password": "coordinated-fixture-only"})["token"]
        restored_roster = request(nodes[0], "/api/v1/cluster/nodes", token=restored_owner)
        assert len([node for node in restored_roster["nodes"] if node["role"] == "voter" and node["is_voter"]]) == 3
        stop_all(children)
        coordinators = [Coordinator(helper, node, "restored-retention") for node in nodes]
        for process in coordinators:
            assert process.response()["ready"]
        restored = coordinators[0].command("snapshot")["snapshot"]
        (root / "retained-restored.json").write_text(json.dumps(restored, indent=2, sort_keys=True) + "\n")
        compare_retained(before, restored, terminal_cleanup=True)
        stop_all(coordinators)
        return {"historical_daemon_voters": 3, "current_daemon_restart": True,
                "factory_before_principal_marker": True, "retained_tables": len(before),
                "closed_topology_backup_restore": True,
                "full_lease_inventory_retained_across_rebuild": True,
                "ended_session_lease_cleanup_after_restart_and_restore": True,
                "background_leases": "mutable runtime state; full snapshots recorded",
                "active_media_drain": "not qualified", "shared_writes": "not admitted"}
    finally:
        for child in children + coordinators:
            child.kill()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True, help="new source and fixture directory")
    parser.add_argument("--target-dir", type=Path, required=True, help="dedicated warm compiler directory")
    args = parser.parse_args()
    args.source_dir = args.source_dir.resolve()
    if args.source_dir.exists():
        parser.error("source-dir must not exist; never overlay user data or source")
    version = subprocess.check_output(["rustc", "--version"], text=True).strip()
    if not version.startswith("rustc 1.97.1 "):
        parser.error(f"repository-pinned Rust 1.97.1 required; got {version}")
    candidate = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    args.source_dir.mkdir(parents=True)
    baseline_source, candidate_source = args.source_dir / "historical", args.source_dir / "candidate"
    receipt = {"baseline_source": BASELINE, "candidate_source": candidate, "rustc": version,
        "historical_archive_sha256": archive_source(BASELINE, baseline_source),
        "candidate_archive_sha256": archive_source(candidate, candidate_source),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    # The runner itself, helper and frozen DDL must all come from committed HEAD.
    if Path(__file__).read_bytes() != (candidate_source / "scripts" / Path(__file__).name).read_bytes():
        raise RuntimeError("working runner differs from committed candidate source")
    targets = [args.target_dir / "historical", args.target_dir / "candidate"]
    build(baseline_source, targets[0], ["-p", "plurxd", "--bin", "plurxd"], args.source_dir / "historical-build.log")
    build(candidate_source, targets[1], ["-p", "plurx-core", "--features", "hiqlite-store", "--example", "qualify_sharing_upgrade"], args.source_dir / "helper-build.log")
    build(candidate_source, targets[1], ["-p", "plurxd", "--bin", "plurxd"], args.source_dir / "candidate-build.log")
    old_binary, new_binary = targets[0].resolve() / "debug/plurxd", targets[1].resolve() / "debug/plurxd"
    helper = targets[1].resolve() / "debug/examples/qualify_sharing_upgrade"
    receipt["binary_sha256"] = {"historical": hashlib.sha256(old_binary.read_bytes()).hexdigest(),
                               "candidate": hashlib.sha256(new_binary.read_bytes()).hexdigest()}
    receipt["future_sqlite"] = [future_sqlite(binary, helper, args.source_dir / label, label)
                 for binary, label in [(old_binary, "historical-future"), (new_binary, "candidate-future")]]
    print("FUTURE SQLITE " + json.dumps(receipt["future_sqlite"], sort_keys=True), flush=True)
    receipt["future_replicated"] = [future_replicated(binary, helper, args.source_dir / label, label)
                 for binary, label in [(old_binary, "historical-replicated-future"), (new_binary, "candidate-replicated-future")]]
    print("FUTURE REPLICATED " + json.dumps(receipt["future_replicated"], sort_keys=True), flush=True)
    receipt["three_voter"] = cluster_fixture(old_binary, new_binary, helper, args.source_dir / "three-voter")
    (args.source_dir / "qualification-receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    print("COORDINATED FIXTURE " + json.dumps(receipt, sort_keys=True), flush=True)


if __name__ == "__main__":
    main()
