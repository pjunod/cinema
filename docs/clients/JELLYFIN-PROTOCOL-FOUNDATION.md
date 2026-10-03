# Jellyfin protocol foundation — wire forms before service integration

**Status:** open · J1 protocol task implemented; integration checks pending ·
**Written:** 2026-10-02.

Companion to [the build contract](JELLYFIN-COMPATIBILITY-BUILD.md) (required
behavior and release evidence) — this records the pure protocol layer and its
validation boundary. The crate performs no I/O, changes no native planner and
mounts no routes. Storage identity allocation, binding lifecycle and shared
auth/watch services remain subsequent J1 work. Physical Infuse qualification
remains required at the end under Paul's explicit test deferral.

## 1. Wire forms — checked values and redacted credentials

| Form | Implemented contract | Focused proof |
|---|---|---|
| `WireId` | Nonzero UUID, accepted in 32-hex or dashed spelling; normalized lowercase 32-hex output; no native integer interpretation | Alternate spelling equality, malformed/nil refusal and serde boundary |
| `Ticks` | Nonnegative signed 64-bit ticks; checked milliseconds × 10,000; conversion to milliseconds discards only sub-millisecond remainder | Zero, negative, maximum runtime, overflow and missing-versus-zero serde |
| `UserToken` | Only explicit observed token carriers; same secret across carriers accepted; conflicts and scoped `plx_` keys refused; diagnostic output redacted | Mixed header/query carriers, duplicate Token conflicts, DeviceId non-authority, malformed quotes and bounds |

Credential input retains duplicate carriers. Headers use their ordinary names;
decoded query values are explicitly tagged `query:api_key` or `query:apikey`,
including observed case variants. The parser accepts MediaBrowser/Emby
authorization attributes and bearer user tokens. It does not collapse a map
before conflict validation or interpret client/device labels as credentials.
Bounds are 32 carriers, 8 KiB total credential bytes, 4 KiB per header,
512 bytes per token and 16 authorization attributes. The application must
still authenticate the returned token through native expiry/revocation and
cache-proof semantics; successful parsing grants no authority.

## 2. Entity replacement guard — deletion must remain observable

The [source guard](../../validation/jellyfin_identity_guard.py) examines Rust
SQL string literals and SQL files beneath `crates/`, including migration
paths. It rejects `INSERT OR REPLACE` and `REPLACE INTO` targeting `users`,
`libraries`, `items` or `files`, because replacement can bypass their planned
DELETE retirement triggers. Quoted/schema-qualified names, SQL comments,
escaped multiline Rust strings and raw strings are covered. Ordinary
`ON CONFLICT ... DO UPDATE` and unrelated settings replacement remain valid.

The [operations regression](../../tests/operations/test_jellyfin_identity_guard.py)
injects forbidden writes into a disposable migration source and checks the
actual repository. This is a literal-source guard, not a runtime SQL parser;
protected writes must stay explicit SQL literals rather than dynamically
assembling replacement operations. No exception is silently allowlisted.
The guard does not claim that retirement triggers or durable opaque mappings
are already installed. Those require both-backend J1 storage contracts.

## 3. Validation — prototype and workspace evidence stay distinct

Four pure Rust tests and three source-guard tests pass on the actual task
workspace with pinned Rust 1.97.1. The new crate is a workspace member, so
workspace unit and compile lanes include it. `jellyfin.compatibility` registers
the implemented paths in the functionality catalog; future service paths
are added when their files exist, never as empty placeholder globs.

```bash
cargo test -p plurx-compat-jellyfin             # Pure wire boundaries
python3 -m unittest discover -s tests/operations \
  -p 'test_jellyfin_identity_guard.py'          # Injected and actual source
```

These checks do not prove an HTTP adapter, catalog identity persistence,
client playback, token replacement/logout, watch durability or final
qualification. Keep those acceptance rows open until their implementation
and focused receipts exist.
