"""Atomic SQLite projection of a verified, frozen knowledge corpus."""
from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile

from . import model
from .model import GraphData, KnowledgeError, canonical_json, require
from .snapshot import Snapshot, SnapshotChanged, capture, checked_path, selection

VERSION = 1


@dataclass(frozen=True)
class IndexedGraph:
    graph: GraphData
    snapshot: Snapshot
    database: Path


def _cache(root: Path, supplied: Path | None) -> Path:
    base = root / "target" / "knowledge"
    path = Path(supplied) if supplied is not None else base
    if not path.is_absolute():
        path = root / path
    require(path.is_relative_to(base), "cache must be beneath repository target/knowledge")
    checked_path(root, path.relative_to(root).as_posix())
    path.mkdir(parents=True, exist_ok=True)
    require(path.resolve().is_relative_to(base.resolve()), "cache escape refused")
    return path


def _write(database: Path, graph: GraphData, snapshot: Snapshot, corpus: str) -> None:
    raw = graph.to_dict()
    # Validate the canonical interchange format before committing any cache.
    GraphData.from_dict(raw)
    with sqlite3.connect(database) as connection:
        connection.execute("PRAGMA journal_mode=DELETE")
        connection.execute("CREATE TABLE meta (name TEXT PRIMARY KEY, value TEXT NOT NULL)")
        for name, value in {"snapshot": snapshot.metadata(), "graph": raw, "corpus": corpus,
                            "snapshot_key": snapshot.key, "index_version": VERSION}.items():
            connection.execute("INSERT INTO meta VALUES (?, ?)", (name, canonical_json(value)))
        for table, collection in (("nodes", "nodes"), ("edges", "edges"), ("claims", "claims"),
                                  ("source_anchors", "anchors"), ("receipts", "receipts"),
                                  ("coverage", "coverage"), ("conflicts", "conflicts")):
            connection.execute(f"CREATE TABLE {table} (ordinal INTEGER PRIMARY KEY, snapshot_key TEXT NOT NULL, payload TEXT NOT NULL)")
            connection.executemany(f"INSERT INTO {table} VALUES (?, ?, ?)",
                ((ordinal, snapshot.key, canonical_json(row)) for ordinal, row in enumerate(raw[collection])))
        connection.commit()
    with database.open("rb") as stream:
        os.fsync(stream.fileno())


def _prune(cache: Path, current: Path) -> None:
    """Retain the current and prior published bundles; never touch build dirs."""
    bundles = [p for p in cache.iterdir() if p.name.startswith("bundle-")
               and not p.is_symlink() and p.is_dir()]
    bundles.sort(key=lambda p: p.stat().st_mtime_ns, reverse=True)
    retained = {current, *bundles[:2]}
    for bundle in bundles:
        if bundle not in retained:
            # Only our fixed two-file bundle shape is eligible for cleanup.
            names = {p.name for p in bundle.iterdir()}
            if names == {"corpus", "graph.sqlite"} and not any(p.is_symlink() for p in bundle.iterdir()):
                shutil.rmtree(bundle)


def build(root: Path, cache_dir: Path | None = None) -> IndexedGraph:
    root = Path(root).resolve()
    cache = _cache(root, cache_dir)
    for attempt in range(2):
        snapshot = capture(root)
        worktree_cache = cache / snapshot.worktree_id.removeprefix("sha256:")
        checked_path(root, worktree_cache.relative_to(root).as_posix())
        worktree_cache.mkdir(exist_ok=True)
        stage = Path(tempfile.mkdtemp(prefix=".build-", dir=worktree_cache))
        published = None
        try:
            corpus = stage / "corpus"
            corpus.mkdir()
            snapshot.materialize(corpus)
            # All source reads below refer to captured bytes, never the live tree.
            try:
                graph = model.load_graph(corpus, revision=snapshot.head)
            except (KnowledgeError, OSError) as error:
                if snapshot.verify(root):
                    raise KnowledgeError("frozen corpus cannot resolve every discovered source") from error
                raise SnapshotChanged("snapshot-changed: frozen discovery changed") from error
            if selection(graph) != snapshot.patterns:
                raise SnapshotChanged("snapshot-changed: frozen discovery requires different sources")
            database = stage / "graph.sqlite"
            _write(database, graph, snapshot, "corpus")
            if not snapshot.verify(root):
                raise SnapshotChanged("snapshot-changed: selected sources changed before publication")
            # The entire immutable bundle is published with one rename. Its
            # database is never visible before its matching corpus exists.
            published = worktree_cache / ("bundle-" + snapshot.key + "-" + stage.name.removeprefix(".build-"))
            checked_path(root, published.relative_to(root).as_posix())
            os.replace(stage, published)
            _prune(worktree_cache, published)
            return IndexedGraph(graph, snapshot, published / "graph.sqlite")
        except SnapshotChanged:
            if attempt:
                raise
        finally:
            if stage.exists():
                shutil.rmtree(stage)
    raise SnapshotChanged("snapshot-changed")


def load(root: Path, cache_dir: Path | None = None) -> IndexedGraph:
    """Revalidate the live selection before returning a graph, including hits.

    Parsing the bounded corpus again intentionally avoids trusting edited cache
    contents or silently reusing a graph after stat-only source changes.
    """
    return build(root, cache_dir)
