"""Thin command-line wiring for the bounded knowledge pilot queries."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import stat
import sys

MAX_BODY_BYTES = 1_048_576


class _Parser(argparse.ArgumentParser):
    def error(self, message):
        _refusal("invalid-arguments", message)
        raise SystemExit(2)


def _refusal(code: str, message: str) -> None:
    print(json.dumps({"error": code, "message": message}, ensure_ascii=False), file=sys.stderr)


def _positive(value: str) -> int:
    try:
        result = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("must be an integer") from None
    if result <= 0 or result > 1_048_576:
        raise argparse.ArgumentTypeError("must be between 1 and 1048576")
    return result


def _depth(value: str) -> int:
    try:
        result = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("must be an integer") from None
    if not 0 <= result <= 8:
        raise argparse.ArgumentTypeError("must be between 0 and 8")
    return result


def _parser() -> argparse.ArgumentParser:
    parser = _Parser(prog="knowledge", description="Query the local engineering knowledge graph")
    parser.add_argument("--repo", type=Path, help=argparse.SUPPRESS)
    commands = parser.add_subparsers(dest="command", required=True)

    index = commands.add_parser("index")
    index.add_argument("--snapshot", choices=("worktree",), required=True)

    for name in ("context", "obligations", "evidence", "why", "conflicts"):
        query = commands.add_parser(name)
        query.add_argument("--max-depth", type=_depth, default=6)
        query.add_argument("--max-bytes", type=_positive, default=8192)
        query.add_argument("--format", choices=("json", "text"), default="json")
        query.add_argument("--continuation")
        if name == "context":
            query.add_argument("--path", required=True)
            query.add_argument("--symbol")
        elif name == "obligations":
            mode = query.add_mutually_exclusive_group(required=True)
            mode.add_argument("--staged", action="store_true")
            mode.add_argument("--base")
            query.add_argument("--worktree", action="store_true")
        elif name == "evidence":
            query.add_argument("identity")
            query.add_argument("--at", dest="revision")
        elif name == "why":
            query.add_argument("identity")
        else:
            query.add_argument("--scope", dest="cohort")

    lint = commands.add_parser("lint")
    lint.add_argument("--scope", choices=("pilot",), default="pilot")
    body = lint.add_mutually_exclusive_group()
    body.add_argument("--body-file", type=Path)
    body.add_argument("--body-env", action="store_true")
    lint.add_argument("--base")
    return parser


def _body(args) -> str | None:
    if args.body_env:
        body = os.environ.get("PR_BODY")
        if body is not None and len(body.encode("utf-8")) > MAX_BODY_BYTES:
            raise ValueError("PR_BODY exceeds 1048576 bytes")
        return body
    if args.body_file is None:
        return None
    path = args.body_file
    try:
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise ValueError("body file must be a regular, non-symlink file")
        if info.st_size > MAX_BODY_BYTES:
            raise ValueError("body file exceeds 1048576 bytes")
        raw = path.read_bytes()
        if len(raw) > MAX_BODY_BYTES:
            raise ValueError("body file exceeds 1048576 bytes")
        return raw.decode("utf-8")
    except (OSError, UnicodeDecodeError, ValueError):
        raise ValueError("body file could not be read as bounded UTF-8 text") from None


def _run(args, root: Path) -> int:
    from . import index

    if args.command == "lint":
        from . import lint
        result = lint.run(root, body=_body(args), base=args.base)
        payload = json.dumps(result, ensure_ascii=False, sort_keys=True)
        if len(payload.encode("utf-8")) + 1 > 8192:
            _refusal("budget-too-small", "lint result exceeds the 8192-byte output limit")
            return 2
        print(payload)
        return 1 if result.get("errors") else 0

    if args.command == "index":
        indexed = index.build(root)
        meta = indexed.snapshot.metadata()
        database = indexed.database
        try:
            db_name = database.resolve().relative_to(root.resolve()).as_posix()
        except ValueError:
            db_name = "target/knowledge"
        result = {key: meta.get(key) for key in
                  ("key", "head", "tree", "worktree_id", "input_count", "repository_dirty")}
        result.update(database=db_name, node_count=len(indexed.graph.nodes),
                      edge_count=len(indexed.graph.edges))
        print(json.dumps(result, ensure_ascii=False, sort_keys=True))
        return 0

    indexed = index.load(root)
    from . import queries
    paths, symbol, identity, cohort = (), None, None, None
    assessment = None
    if args.command == "context":
        from .model import safe_relative
        safe_path = safe_relative(args.path)
        paths, symbol = (safe_path,), args.symbol
    elif args.command == "obligations":
        from . import capture
        paths = capture.changed_paths(root, indexed, base=args.base,
                                      staged=args.staged, worktree=args.worktree)
    elif args.command in ("why", "evidence"):
        identity = args.identity
        if args.command == "evidence":
            receipt = next((row for row in indexed.graph.receipts if row.id == identity), None)
            check = next((row for row in indexed.graph.nodes if row.id == identity), None)
            if receipt is not None:
                from .evidence import assess
                assessment = assess(receipt, indexed.snapshot, root,
                                    target_revision=args.revision or indexed.snapshot.head)
            elif check is not None and check.kind == "check":
                from .evidence import assess
                assessment = assess(check, indexed.snapshot, root,
                                    target_revision=args.revision or indexed.snapshot.head)
            else:
                raise ValueError("evidence identity is absent from the captured graph")
    else:
        cohort = args.cohort
    metadata = indexed.snapshot.metadata()
    metadata["knowledge_view"] = "worktree"
    if args.command == "obligations":
        metadata["change_view"] = "staged paths only" if args.staged else "full proposed worktree diff"
    result = queries.answer(indexed.graph, metadata, command=args.command,
                            paths=paths, symbol=symbol, identity=identity, cohort=cohort,
                            max_depth=args.max_depth, max_bytes=args.max_bytes,
                            continuation=args.continuation,
                            evidence_assessment=assessment)
    print(queries.render(result, format=args.format, max_bytes=args.max_bytes), end="")
    return 0


def main(argv=None, *, root: Path | None = None) -> int:
    args = _parser().parse_args(argv)
    repo = args.repo or (Path(root) if root is not None else Path.cwd())
    try:
        if args.command == "obligations":
            if args.base and not args.worktree:
                raise ValueError("--base requires --worktree")
            if args.staged and args.worktree:
                raise ValueError("--staged and --worktree cannot be combined")
        repo = repo.resolve(strict=True)
        if not repo.is_dir():
            raise ValueError("repository root is not a directory")
        return _run(args, repo)
    except SystemExit:
        raise
    except Exception as error:
        # Keep refusals useful without echoing caller paths or source content.
        _refusal("refused", f"{type(error).__name__}: knowledge command could not be completed")
        return 1
