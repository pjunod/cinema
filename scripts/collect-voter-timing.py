#!/usr/bin/env python3
"""Bounded read-only C4 timing collection; no fleet changes or fault injection."""
import argparse
import datetime as dt
import json
import math
from pathlib import Path
import re
import time
import urllib.parse
import urllib.request

MAX_TARGETS = 16
MAX_BODY = 2 * 1024 * 1024
MAX_BYTES = 256 * 1024 * 1024
FAMILIES = ("plurx_cluster_append_observation_", "plurx_cluster_serving_monitor_wake_")
SCALARS = {"plurx_cluster_timing_uptime_seconds", "plurx_cluster_replicated", "plurx_cluster_local_is_voter", "plurx_cluster_membership_sample_valid", "plurx_cluster_membership_sample_age_seconds", "plurx_raft_current_term"}

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

def validate_nodes(config):
    nodes = config.get("nodes")
    if not isinstance(nodes, list) or not 1 <= len(nodes) <= MAX_TARGETS:
        raise ValueError("config requires 1..16 explicit discovered targets")
    names = set()
    for node in nodes:
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,40}", node.get("name", "")) or node["name"] in names:
            raise ValueError("unique bounded neutral node aliases required")
        names.add(node["name"])
        url = urllib.parse.urlsplit(node.get("url", ""))
        if url.scheme not in {"http", "https"} or not url.hostname or url.username or url.password or url.query or url.fragment or url.path not in {"", "/"}:
            raise ValueError("explicit bare HTTP(S) node origin required; no credentials")
        if not re.fullmatch(r"[0-9a-f]{40}", node.get("expected_source", "")) or not node.get("expected_build"):
            raise ValueError("full source and exact expected build required")
        if node.get("expected_role") not in {"voter", "learner"}:
            raise ValueError("expected_role must name voter or learner")
    return nodes

def parse_metrics(body, node):
    values = {}
    series = []
    build = None
    epoch = None
    for line in body.splitlines():
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r"([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{([^}]*)\})?\s+([^ ]+)(?:\s+[^ ]+)?", line)
        if not match:
            continue
        name, labels_text, raw = match.groups()
        if name not in SCALARS and not name.startswith(FAMILIES) and name not in {"plurx_build_info", "plurx_cluster_timing_epoch_info"}:
            continue
        labels = {key: json.loads('"' + value + '"') for key, value in re.findall(r'(\w+)="((?:[^"\\]|\\.)*)"', labels_text or "")}
        if name == "plurx_build_info":
            build = labels.get("build")
            continue
        if name == "plurx_cluster_timing_epoch_info":
            epoch = labels.get("epoch")
            continue
        value = float(raw)
        if not math.isfinite(value):
            raise ValueError("non-finite timing metric")
        if name in SCALARS:
            values[name] = value
        else:
            if len(series) >= 1024 or any(len(str(v)) > 80 for v in labels.values()):
                raise ValueError("timing cardinality or label bound exceeded")
            if not set(labels) <= {"group", "stage", "local", "remote", "source", "term", "endpoint"}:
                raise ValueError("unexpected timing label")
            series.append([name, labels, value])
    reasons = []
    if build != node["expected_build"]:
        reasons.append("source_build_mismatch")
    short = re.search(r"-g([0-9a-f]{9,40})(?:-|$)", build or "")
    if not short or not node["expected_source"].startswith(short[1]):
        reasons.append("source_identity_unproven")
    if not re.fullmatch(r"[0-9a-f]{16}", epoch or ""):
        reasons.append("timing_instrumentation_unavailable")
    if values.get("plurx_cluster_replicated") != 1:
        reasons.append("replication_unavailable")
    voter = values.get("plurx_cluster_local_is_voter")
    if voter != (1 if node["expected_role"] == "voter" else 0):
        reasons.append("role_mismatch_or_unavailable")
    if values.get("plurx_cluster_membership_sample_valid") != 1 or not 0 <= values.get("plurx_cluster_membership_sample_age_seconds", math.inf) <= 10:
        reasons.append("membership_sample_unavailable_or_old")
    if "plurx_raft_current_term" not in values:
        reasons.append("term_unavailable")
    return {"node": node["name"], "expected_source": node["expected_source"], "build": build, "process_epoch": epoch, "values": values, "series": series, "valid": not reasons, "limitations": reasons}

def fetch(node, opener):
    url = node["url"].rstrip("/") + "/metrics"
    with opener.open(url, timeout=0.75) as response:
        if response.status != 200 or response.geturl() != url:
            raise ValueError("direct metrics response required")
        data = response.read(MAX_BODY + 1)
        if len(data) > MAX_BODY:
            raise ValueError("metrics body bound exceeded")
    return parse_metrics(data.decode("utf-8"), node)

def write_record(output, record, limit):
    payload = (json.dumps(record, separators=(",", ":"), allow_nan=False) + "\n").encode()
    if output.tell() + len(payload) > limit:
        return False
    output.write(payload)
    output.flush()
    return True

def mark_cohort(record, prior):
    cohort = (record["build"], record["process_epoch"])
    record["cohort_changed"] = record["node"] in prior and prior[record["node"]] != cohort
    prior[record["node"]] = cohort

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--duration-seconds", type=int, default=3600)
    parser.add_argument("--max-bytes", type=int, default=64 * 1024 * 1024)
    args = parser.parse_args()
    if not 1 <= args.duration_seconds <= 86400 or not 1024 * 1024 <= args.max_bytes <= MAX_BYTES:
        parser.error("duration 1..86400 and capacity 1..256 MiB required")
    nodes = validate_nodes(json.loads(args.config.read_text()))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect)
    started = time.monotonic()
    prior = {}
    # Exclusive create prevents overwriting an earlier observation receipt.
    with args.output.open("xb") as output:
        write_record(output, {"kind": "start", "duration_seconds": args.duration_seconds, "max_bytes": args.max_bytes, "interval_seconds": 1, "semantics": "RPC attempts/receiver handlers and monitor wake lateness; not delivery or scheduler causation"}, args.max_bytes)
        tick = 0
        while time.monotonic() - started < args.duration_seconds:
            for node in nodes:
                begun = time.monotonic()
                try:
                    record = fetch(node, opener)
                    mark_cohort(record, prior)
                except Exception as error:
                    # Exception class only: no response bodies, media, tokens or URLs.
                    record = {"node": node["name"], "valid": False, "error": type(error).__name__}
                record.update(kind="sample", utc=dt.datetime.now(dt.timezone.utc).isoformat(), elapsed_seconds=time.monotonic()-started, scrape_seconds=time.monotonic()-begun, tick=tick, poll_lateness_seconds=max(0, begun-started-tick))
                if not write_record(output, record, args.max_bytes - 512):
                    write_record(output, {"kind": "end", "reason": "capacity", "elapsed_seconds": time.monotonic()-started, "coverage_complete": False}, args.max_bytes)
                    return 2
            tick += 1
            time.sleep(max(0, started + tick - time.monotonic()))
        write_record(output, {"kind": "end", "reason": "duration", "elapsed_seconds": time.monotonic()-started, "duration_complete": True, "coverage_requires_sample_review": True}, args.max_bytes)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
