# Effort web checks — source lint, not behavioral qualification

**Status:** open — implementation on 2026-10-01; independent review and effort gate pending.

The blocking effort web lane runs `make effort-web-static-check` against
the current source. It does not execute a unit test, evaluate the application
in a VM, or launch Chromium. Passing this lane is not web behavior acceptance.
The [development pipeline](../DEVELOPMENT_PIPELINE.md) still requires the
smallest focused regression for changed behavior, with once-per-PR success
receipts. Successful units are not rerun merely because a base or head moves.

## What runs automatically

| Command | Current-source obligation |
|---|---|
| `scripts/js-check` | Embedded and split JavaScript syntax. |
| `node scripts/web-jsconfig --check` | Generated checker configuration is fresh. |
| `scripts/web-types` | Pinned TypeScript checks every configured served row against the existing exact diagnostic ratchet. |
| `node scripts/web-shape-check` | Shell tags, Rust asset table, documentation, disk inventory, load-time dependency graph, shared contract data/embeds, player markup and Player typedef agree. |
| `scripts/contrast-check` (Makefile arguments) | Every shipped theme's foreground/background pairs meet the existing contrast rule and allowlist. |

The shape validator only reads source and JSON data. Its AST dependency walk
does not execute callbacks. Its Player parameter check annotates actual
source in memory for TypeScript; it neither emits files nor compiles synthetic
misspelling probes. Imported helpers are parsers/readers/renderers, not test
files or production reducers. Missing, malformed, duplicated or stale source
contracts fail closed.

Asset checks retain the [shell layout](../clients/WEB-SHELL-LAYOUT.md)
obligations: route/include path equality; tag order and head/body/style kind;
all seven sidecars; reader stylesheet before app styles; head scripts after
styles; body scripts after sidecars; numbered documentation and resolving
links; useful descriptions and old-line provenance; no unserved shell files;
strict prologues, duplicate globals and load-time forward references.

Shared input/live routing tables retain complete declared dimensions and
defined outcomes. Surface classes/sources retain declared severity, contexts,
actions, unique case/source names and case coverage. Generated documentation
and served JSON embeds must match byte-for-byte, with exactly one ordered pair
of markers. The markup retains the shared transport/bar ordering, single
timeline tab stop, dialog semantics and menu accessibility. The Player typedef
still covers actual writes and reads through typed parameters.

## What remains explicit

`make web-check` is unchanged: it includes behavioral reducers, mutation
fixtures, VM load smoke, synthetic typedef probes and Chromium startup and
subtitle checks. It remains available for manual or final qualification under
the applicable once-per-PR execution policy; the effort lane must not invoke
it automatically or retry its already-passing units.

Behavior changes still owe focused local unit evidence and a named
`Regression-Test` field. The static lane cannot establish playback behavior,
browser startup, accessibility interaction, media correctness or fleet
qualification. It needs no Playwright installation, so only this effort job's
browser provisioning is removed. Main validation, browser helpers and the
full manual target are not changed.

Validator refusal regressions live in
`tests/validation/test_effort_web_static.py`; they run separately as units and
are not hidden inside the source lint. See
[Python PR receipts](PYTHON-UNIT-PR-RECEIPTS.md) for retaining their original
source and first successful execution.
