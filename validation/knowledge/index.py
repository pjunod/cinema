"""Atomic SQLite projection of a verified, frozen knowledge corpus."""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import hmac
import json
import os
from pathlib import Path
import shutil
import stat
import sqlite3
import tempfile
import uuid

from . import model
from .model import GraphData, KnowledgeError, canonical_json, require
from .snapshot import Snapshot, SnapshotChanged, _identity, capture, checked_path, recapture, selection

VERSION = 2
_MAX_DATABASE_BYTES = 32 * 1024 * 1024
_KEY_NAME = ".cache-key"


@dataclass(frozen=True)
class IndexedGraph:
    graph: GraphData
    snapshot: Snapshot
    database: Path


def _directory(root: Path, path: Path, *, create=False):
    relative = path.relative_to(root).as_posix()
    model.safe_relative(relative)
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in relative.split("/"):
            try:
                child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            except FileNotFoundError:
                if not create:
                    raise
                try:
                    os.mkdir(component, 0o700, dir_fd=descriptor)
                except FileExistsError:
                    pass
                child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _cache(root: Path, supplied: Path | None) -> Path:
    base = root / "target" / "knowledge"
    path = Path(supplied) if supplied is not None else base
    if not path.is_absolute():
        path = root / path
    require(path.is_relative_to(base), "cache must be beneath repository target/knowledge")
    checked_path(root, path.relative_to(root).as_posix())
    descriptor = _directory(root, path, create=True)
    try:
        os.fchmod(descriptor, 0o700)
    finally:
        os.close(descriptor)
    return path


def _read_owned(root, path, maximum):
    parent = _directory(root, path.parent)
    try:
        descriptor = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_uid == os.getuid()
                    and before.st_size <= maximum, "unsafe cache file")
            raw = stream.read(maximum + 1)
            after = os.fstat(stream.fileno())
        current = os.stat(path.name, dir_fd=parent, follow_symlinks=False)
    finally:
        os.close(parent)
    def identity(info):
        return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
    require(len(raw) <= maximum and identity(before) == identity(after) == identity(current), "cache file changed")
    return raw, before


def _cache_key(root, cache, *, create):
    """One atomic local-integrity key, never an execution/origin credential.

    The owned cache/host is trusted; this does not defend against a same-user
    attacker who can steal/replace the key. A signature is never receipt auth.
    """
    path = cache / _KEY_NAME
    try:
        raw, info = _read_owned(root, path, 32)
    except FileNotFoundError:
        if not create:
            return None
        parent = _directory(root, cache)
        temporary = ".cache-key-" + uuid.uuid4().hex
        created = False
        try:
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
            created = True
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(os.urandom(32))
                stream.flush()
                os.fsync(stream.fileno())
            # link is an atomic create-if-absent: concurrent producers always
            # choose the same fully written key, never an exposed empty file.
            try:
                os.link(temporary, _KEY_NAME, src_dir_fd=parent, dst_dir_fd=parent, follow_symlinks=False)
            except FileExistsError:
                pass
            os.fsync(parent)
        finally:
            if created:
                os.unlink(temporary, dir_fd=parent)
            os.close(parent)
        raw, info = _read_owned(root, path, 32)
    require(len(raw) == 32 and stat.S_IMODE(info.st_mode) == 0o600, "unsafe cache integrity key")
    return raw


def _signed_payload(graph, recipe):
    return {"index_version": VERSION, "graph": graph, "snapshot_recipe": recipe}


def _signature(key, payload):
    return hmac.new(key, canonical_json(payload).encode(), hashlib.sha256).hexdigest()


def _write(database: Path, graph: GraphData, snapshot: Snapshot, corpus: str, key: bytes) -> None:
    raw = graph.to_dict()
    # Validate the canonical interchange format before committing any cache.
    GraphData.from_dict(raw)
    recipe = snapshot.recipe()
    signature = _signature(key, _signed_payload(raw, recipe))
    with sqlite3.connect(database) as connection:
        connection.execute("PRAGMA journal_mode=DELETE")
        connection.execute("CREATE TABLE meta (name TEXT PRIMARY KEY, value TEXT NOT NULL)")
        for name, value in {"snapshot": snapshot.metadata(), "graph": raw, "corpus": corpus,
                            "snapshot_key": snapshot.key, "index_version": VERSION,
                            "snapshot_recipe": recipe, "cache_signature": signature}.items():
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
    previous = [bundle for bundle in bundles if bundle != current]
    retained = {current, *previous[:1]}
    for bundle in bundles:
        if bundle not in retained:
            # Only our fixed two-file bundle shape is eligible for cleanup.
            names = {p.name for p in bundle.iterdir()}
            if names == {"corpus", "graph.sqlite"} and not any(p.is_symlink() for p in bundle.iterdir()):
                shutil.rmtree(bundle)


def build(root: Path, cache_dir: Path | None = None) -> IndexedGraph:
    root = Path(root).resolve()
    cache = _cache(root, cache_dir)
    key = _cache_key(root, cache, create=True)
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
            _write(database, graph, snapshot, "corpus", key)
            if _cache_key(root, cache, create=False) != key:
                key = _cache_key(root, cache, create=True)
                raise SnapshotChanged("snapshot-changed: cache key rotated")
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


def _cached_graph(root, database, key):
    """Read a bounded SQLite image, then authenticate before using its scopes."""
    raw, _ = _read_owned(root, database, _MAX_DATABASE_BYTES)
    with sqlite3.connect(":memory:") as connection:
        connection.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, model.MAX_BYTES)
        connection.setlimit(sqlite3.SQLITE_LIMIT_SQL_LENGTH, 4096)
        connection.setlimit(sqlite3.SQLITE_LIMIT_ATTACHED, 0)
        connection.deserialize(raw)
        connection.execute("PRAGMA trusted_schema=OFF")
        connection.execute("PRAGMA query_only=ON")
        schema = connection.execute("SELECT type, sql FROM sqlite_schema WHERE name=?", ("meta",)).fetchone()
        require(schema == ("table", "CREATE TABLE meta (name TEXT PRIMARY KEY, value TEXT NOT NULL)"), "invalid cache metadata schema")
        names = ("graph", "snapshot_recipe", "cache_signature", "index_version", "snapshot_key", "snapshot")
        rows = connection.execute("SELECT name, value FROM meta WHERE name IN (?, ?, ?, ?, ?, ?) LIMIT 7", names).fetchall()
    require(len(rows) == len(names), "incomplete cache metadata")
    values = {name: json.loads(value) for name, value in rows}
    require(values["index_version"] == VERSION, "obsolete cache schema")
    payload = _signed_payload(values["graph"], values["snapshot_recipe"])
    signature = values["cache_signature"]
    require(isinstance(signature, str) and hmac.compare_digest(signature, _signature(key, payload)), "cache integrity mismatch")
    # No cached pattern/source path is consulted until after the MAC check.
    graph = GraphData.from_dict(values["graph"])
    recipe = values["snapshot_recipe"]
    require(recipe["key"] == values["snapshot_key"] and selection(graph) == tuple(recipe["patterns"]), "cache selection mismatch")
    return graph, recipe


def load(root: Path, cache_dir: Path | None = None) -> IndexedGraph:
    """Reuse only authenticated graphs whose complete live selection matches.

    Hits re-read selected bytes/modes/membership and repository/code identity;
    source races still retry once. Invalid/old/forged caches rebuild from the
    authoritative frozen corpus. Local cache integrity never authenticates a
    run or changes any receipt's origin/applicability.
    """
    root = Path(root).resolve()
    cache = _cache(root, cache_dir)
    key = _cache_key(root, cache, create=False)
    if key is None:
        return build(root, cache_dir)
    for attempt in range(2):
        identity = _identity(root)
        worktree_cache = cache / identity["worktree_id"].removeprefix("sha256:")
        checked_path(root, worktree_cache.relative_to(root).as_posix())
        if not worktree_cache.exists():
            return build(root, cache_dir)
        bundles = [p for p in worktree_cache.iterdir() if p.name.startswith("bundle-") and not p.is_symlink() and p.is_dir()]
        bundles.sort(key=lambda p: p.stat().st_mtime_ns, reverse=True)
        try:
            for bundle in bundles[:2]:
                try:
                    graph, recipe = _cached_graph(root, bundle / "graph.sqlite", key)
                    snapshot = recapture(root, recipe)
                    if snapshot is not None and _cache_key(root, cache, create=False) == key:
                        return IndexedGraph(graph, snapshot, bundle / "graph.sqlite")
                except SnapshotChanged:
                    raise
                except (KnowledgeError, OSError, sqlite3.Error, ValueError, KeyError, TypeError, RecursionError):
                    continue
            return build(root, cache_dir)
        except SnapshotChanged:
            if attempt:
                raise
    raise SnapshotChanged("snapshot-changed")
