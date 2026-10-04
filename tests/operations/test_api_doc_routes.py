"""`docs/API.md` names every route the router registers, and invents none.

The native API has no OpenAPI description — `crates/plurxd/src/http/mod.rs`
says one "will be generated from these routes as they stabilize" and nothing
generates it — so API.md is the specification five client platforms work
from. A hand-written specification of 165 routes rots in two directions, and
both are worse than having no document: a route added without a table row
leaves a caller reading Rust to find it, and a route removed without deleting
its row leaves a caller writing against an endpoint that returns the app
shell with a 200.

So the router is parsed, not grepped for prose. Route literals come from every
named `Router::new()` chain in `router()` and from any named subrouter nested
or merged inside those local groups. Constants are resolved to their declared
values, nested prefixes are composed, and the `/api/v1` nest is applied to the
native deadline groups.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ROUTER = ROOT / "crates" / "plurxd" / "src" / "http" / "mod.rs"
DOC = ROOT / "docs" / "API.md"
CRATES = ROOT / "crates"

# Paths API.md names that are deliberately NOT routes. Every entry is either a
# Plex endpoint §23 documents as unimplemented — the reason that section
# exists is that an unrouted path answers with the app shell and a 200, so a
# client author needs to be told rather than left to infer — or prose: an
# example path, a prefix, or a route spelled without its section's stated
# prefix. Fix by writing the route or the reference, not by widening this set.
KNOWN_NON_ROUTES = {
    "/video/:/transcode/universal/decision",  # §23: named by an older
    "/video/:/transcode/universal/start.m3u8",  # ARCHITECTURE §5 draft,
    "/:/progress",                            # never registered
    "/playlists",
    "/library/recentlyAdded",                 # §23: advertised by /library
    "/hubs",                                  # §23: advertised by /
    "/api/v1",                                # the nest, not a route
    "/api/v1/cluster/*",                      # a family, in prose
    "/media/kids",                            # §6.3 longest-root example
    "/media/movies",
    "/media/tv",
    "/media/tv/Show/Season 01",
    "/downloads/x",
    "/auto/v",                                # the HDHomeRun's own path
    "/subs/0",                                # §11, the two spellings of
    "/subs/0.vtt",                            # one path segment
    "/oauth/device/code",                     # Trakt's endpoints, §18.1
    "/oauth/device/token",
    "/oauth/token",
    "/oauth/revoke",
    "/sync/history",
    "/sync/history/remove",
    "/sync/last_activities",
    "/sync/playback",
    "/sync/watched/movies",
    "/sync/watched/shows",
    "/users/settings",
    "/api/v1/calendar",                       # Curator's endpoints, §18.2
    "/api/v1/system/status",
    "/api/v1/webhooks/plurx",
    "/t/p/w500",                              # a TMDB artwork path
}


def _router_body() -> str:
    src = ROUTER.read_text(encoding="utf-8")
    start = src.index("pub fn router(state: AppState) -> Router {")
    end = src.index("const LEARNER_ROUTE_INELIGIBLE_JSON")
    return src[start:end]


def _route_arguments(segment: str) -> list[str]:
    pattern = r'\.route\(\s*("(?:[^"\\]|\\.)*"|[A-Za-z_][A-Za-z0-9_:]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)'
    return [m.group(1) for m in re.finditer(pattern, segment)]


def _resolve_constant(name: str) -> str:
    """A route path held in a constant is still a route path.

    Seventeen of them are, and they are the whole internal control plane, so
    resolving them is not optional. The module qualifier is used rather than
    the bare name because `SNAPSHOT_PATH` is declared in two modules with two
    different values.
    """
    parts = name.split("::")
    short = parts[-1]
    module = parts[-2] if len(parts) > 1 else None
    declaration = re.compile(r'const\s+%s\s*:\s*&str\s*=\s*"([^"]+)"' % re.escape(short))
    candidates: list[tuple[bool, str]] = []
    for path in CRATES.rglob("*.rs"):
        if "/target/" in str(path) or "/build/" in str(path):
            continue
        match = declaration.search(path.read_text(encoding="utf-8", errors="ignore"))
        if match:
            stem = path.stem if path.stem != "mod" else path.parent.name
            candidates.append((stem == module, match.group(1)))
    if not candidates:
        raise AssertionError("no declaration found for route constant %s" % name)
    for qualified, value in candidates:
        if qualified:
            return value
    return candidates[0][1]


def _nested_router_routes(segment: str) -> set[str]:
    """Expand `.nest("/prefix", module::router())` without inventing paths.

    Axum subrouters keep a bounded route family and its middleware together.
    The inventory therefore reads the named module's router chain and composes
    the literal prefix exactly as Axum does.
    """
    routes: set[str] = set()
    nested = re.compile(
        r'\.nest\(\s*"([^"]+)"\s*,\s*([A-Za-z_]\w*)::router\(\)'
    )
    for prefix, module in nested.findall(segment):
        source = (ROUTER.parent / f"{module}.rs").read_text(encoding="utf-8")
        router = re.search(
            r"(?:pub(?:\(crate\))?\s+)?fn\s+router\([^)]*\)[^{]*\{(?P<body>.*?)^\}",
            source,
            re.MULTILINE | re.DOTALL,
        )
        if router is None:
            raise AssertionError(f"no router() body found in module {module}")
        for argument in _route_arguments(router.group("body")):
            literal = (
                argument.strip('"')
                if argument.startswith('"')
                else _resolve_constant(argument)
            )
            routes.add(prefix + literal)
        routes.update(prefix + path for path in _merged_router_routes(router.group("body")))
    return routes


def _module_router_calls(segment: str) -> list[tuple[str, str]]:
    """Read a module call inside an actual merge, including state arguments.

    The caller supplies only a registered router chain. Balanced arguments are
    skipped rather than searched for route declarations or unrelated modules.
    Limits keep malformed input from silently expanding this inventory scan.
    """
    calls: list[tuple[str, str]] = []
    start = re.compile(r"\.merge\(\s*((?:(?:crate|super|self)::)?[A-Za-z_]\w*)::([A-Za-z_]\w*)\(")
    for match in start.finditer(segment):
        depth = 1
        quoted = False
        escaped = False
        end = None
        for cursor in range(match.end(), min(len(segment), match.end() + 8192)):
            char = segment[cursor]
            if quoted:
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    quoted = False
                continue
            if char == '"':
                quoted = True
            elif char == "(":
                depth += 1
                if depth > 64:
                    raise AssertionError("module router argument nesting exceeds inventory bound")
            elif char == ")":
                depth -= 1
                if depth == 0:
                    end = cursor + 1
                    break
        if end is None:
            raise AssertionError("unbalanced or oversized module router arguments")
        tail = end
        while tail < len(segment) and segment[tail].isspace():
            tail += 1
        if tail >= len(segment) or segment[tail] != ")":
            raise AssertionError("module router call does not close its merge")
        calls.append((match.group(1), match.group(2)))
    return calls


def _merged_router_routes(segment: str) -> set[str]:
    """Expand actual module router merges at the current prefix."""
    routes: set[str] = set()
    for module, function in _module_router_calls(segment):
        base = ROUTER.parent.parent if module.startswith("crate::") else ROUTER.parent
        source = (base / f"{module.split('::')[-1]}.rs").read_text(encoding="utf-8")
        subrouter = re.search(
            rf"(?:pub(?:\(crate\))?\s+)?fn\s+{re.escape(function)}\([^)]*\)[^{{]*\{{(?P<body>.*?)^\}}",
            source,
            re.MULTILINE | re.DOTALL,
        )
        if subrouter is None:
            raise AssertionError(f"no {function}() body found in module {module}")
        for argument in _route_arguments(subrouter.group("body")):
            routes.add(
                argument.strip('"')
                if argument.startswith('"')
                else _resolve_constant(argument)
            )
        routes.update(_merged_router_routes(subrouter.group("body")))
    return routes


def _local_router_segments(body: str) -> dict[str, str]:
    """Return each `let name = Router::new()` chain in source order."""
    declarations = list(
        re.finditer(r"^\s*let\s+([A-Za-z_]\w*)\s*=\s*Router::new\(\)", body, re.MULTILINE)
    )
    root_at = body.index("Router::new()\n        // Also opted out")
    segments: dict[str, str] = {}
    for index, declaration in enumerate(declarations):
        end = declarations[index + 1].start() if index + 1 < len(declarations) else root_at
        segments[declaration.group(1)] = body[declaration.start():end]
    return segments


def _local_router_routes(
    name: str,
    segments: dict[str, str],
    visiting: frozenset[str] = frozenset(),
) -> set[str]:
    """Expand one local router group, including local and module merges."""
    if name in visiting:
        raise AssertionError(f"local router merge cycle through {name}")
    segment = segments[name]
    routes = {
        argument.strip('"') if argument.startswith('"') else _resolve_constant(argument)
        for argument in _route_arguments(segment)
    }
    routes.update(_nested_router_routes(segment))
    routes.update(_merged_router_routes(segment))
    local_merges = re.findall(r"\.merge\(\s*([A-Za-z_]\w*)\s*\)", segment)
    for merged in local_merges:
        if merged not in segments:
            raise AssertionError(f"no local Router::new() chain found for {merged}")
        routes.update(_local_router_routes(merged, segments, visiting | {name}))
    return routes


def registered_routes() -> set[str]:
    body = _router_body()
    root_at = body.index("Router::new()\n        // Also opted out")
    segments = _local_router_segments(body)

    routes = {"/api/v1" + path for path in _local_router_routes("api", segments)}
    root_segment = body[root_at:]
    for merged in re.findall(r"\.merge\(\s*([A-Za-z_]\w*)\s*\)", root_segment):
        if merged != "api":
            routes.update(_local_router_routes(merged, segments))
    return routes


def _expand(path: str) -> list[str]:
    """`{a,b}` in a table cell is one row covering two routes.

    Combining them is how the document stays readable where a pair differs by
    one word — `/cluster/join/{redeem,finalize}` — so the sweep expands what
    the reader collapses.
    """
    match = re.search(r"\{([a-z0-9\-]+(?:,[a-z0-9\-]+)+)\}", path)
    if not match:
        return [path]
    out: list[str] = []
    for alternative in match.group(1).split(","):
        out.extend(_expand(path[: match.start()] + alternative + path[match.end() :]))
    return out


def _paths_in(text: str) -> set[str]:
    spelled: set[str] = set()
    for literal in re.findall(r"`(/[A-Za-z0-9_/{},*.:\-]*)`", text):
        for candidate in _expand(literal):
            spelled.add(candidate)
            # §7-§18 state their section's prefix once and then omit it.
            spelled.add("/api/v1" + candidate)
    return spelled


def documented_paths() -> set[str]:
    """Every path the document spells anywhere, prose included."""
    return _paths_in(DOC.read_text(encoding="utf-8"))


def tabulated_paths() -> set[str]:
    """Only the paths in a table row — the document's actual inventory.

    Prose cites route families (`/files/`, `/hls/`) and other applications'
    endpoints (Trakt's `/oauth/token`, Curator's `/api/v1/calendar`), so
    sweeping prose for invented routes would mostly find sentences. The tables
    are the claim that something exists here; they are what gets checked.
    """
    rows = [
        line for line in DOC.read_text(encoding="utf-8").splitlines()
        if line.startswith("|")
    ]
    return _paths_in("\n".join(rows))


class ApiDocRoutesTest(unittest.TestCase):
    def test_module_merges_preserve_zero_args_and_expand_balanced_state_args_only(self) -> None:
        zero = _merged_router_routes(".merge(sharing::admin_router())")
        state = _merged_router_routes(".merge(sharing::admin_router(state.clone()))")
        nested = _merged_router_routes('.merge(sharing::admin_router(choose(state.clone(), "literal ) (")))')
        self.assertEqual(zero, state)
        self.assertEqual(state, nested)
        self.assertIn("/sharing/settings", state)
        self.assertEqual(len(state), 16)
        self.assertIn("/sharing/imports/{import}/libraries", state)
        self.assertEqual(_merged_router_routes(".merge(super::shared_library::admin_library_router(state))"), {"/sharing/imports/{import}/libraries"})
        self.assertFalse(any(path.startswith("/sharing/v1/") for path in state),
                         "unregistered private peer routers are not swept from the module")
        self.assertEqual(_module_router_calls("let candidate = sharing::peer_router(state.clone());"), [])
        self.assertEqual(_module_router_calls(".merge(local_router)"), [])
        for malformed in [".merge(sharing::admin_router(state.clone())", ".merge(sharing::admin_router(" + "(" * 65]:
            with self.assertRaises(AssertionError):
                _module_router_calls(malformed)

    def test_every_registered_route_is_documented(self) -> None:
        """A route nobody can find is a route that gets reimplemented."""
        documented = documented_paths()
        missing = sorted(r for r in registered_routes() if r not in documented)
        self.assertEqual(
            missing,
            [],
            "routes registered in http/mod.rs with no entry in docs/API.md:\n  "
            + "\n  ".join(missing),
        )

    def test_no_documented_path_is_invented(self) -> None:
        """The reverse rot: a row for an endpoint that does not exist.

        This is the more expensive direction, because the fallback serves the
        app shell with a 200 for any unmatched non-`/api` path — so a client
        written against an invented route fails as an unparseable body rather
        than as a 404.
        """
        routes = registered_routes()
        invented = []
        for path in tabulated_paths():
            if path.startswith("/api/v1/api/v1") or path.count("/") < 2:
                continue
            if path in KNOWN_NON_ROUTES or path in routes:
                continue
            if "/api/v1" + path in routes or path.replace("/api/v1", "", 1) in routes:
                continue
            if path.replace("/api/v1", "", 1) in KNOWN_NON_ROUTES:
                continue
            invented.append(path)
        self.assertEqual(
            sorted(invented),
            [],
            "paths named in docs/API.md that are neither routes nor listed as "
            "deliberately absent:\n  " + "\n  ".join(sorted(invented)),
        )

    def test_the_route_count_the_document_claims_is_the_count_there_is(self) -> None:
        """API.md opens by claiming a number. Numbers in prose rot silently."""
        text = DOC.read_text(encoding="utf-8")
        claimed = re.search(r"plurx has (\d+)\s+routes", text)
        if claimed is None:
            self.skipTest("the document no longer claims a route count")
        self.assertEqual(int(claimed.group(1)), len(registered_routes()))


if __name__ == "__main__":
    unittest.main()
