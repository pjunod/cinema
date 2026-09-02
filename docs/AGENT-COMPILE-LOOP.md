# Agent compile loop — a compiler for a checkout that has none

Companion to [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (how work is
branched, gated and qualified) — this is *how to prove a change before pushing
it at all*, from a session whose checkout has no toolchain.

An agent session working this repository usually has the clone on one machine
and `cargo` on another. Left alone, that turns every Rust change into a guess
verified by CI: fifteen minutes to the fast gate, thirty to forty to the full
fan-out, and one error reported per attempt. `scripts/`-adjacent helpers catch
the cheap mistakes; nothing local catches a type error, a moved field, or a
test that fails on a real constant.

The two machines can be joined in about ten minutes, and then the whole gate
that matters runs locally.

---

## 1. Why the compiler is on the wrong machine

```
  device VM (user's machine)          cloud container (agent's workspace)
  ┌──────────────────────────┐        ┌──────────────────────────────────┐
  │  the clone               │        │  cargo · rustc · rustup          │
  │  git, gh token           │        │  the pinned toolchain            │
  │  NO cargo, NO toolchain  │        │  NO clone (private repo, and the │
  │                          │        │  token belongs on the device)    │
  └──────────────────────────┘        └──────────────────────────────────┘
```

Copying the token into an ephemeral container to clone there is the obvious
move and the wrong one: it widens a credential's blast radius for a
convenience. The way through is that **a compiler needs source, not history.**

## 2. The move

```bash
# on the machine holding the clone
git archive --format=tar.gz -o <shared-path>/_src.tgz HEAD
```

About 25 MB, no `.git`, no credential in it. Transfer it to the container,
extract, and `cargo` has everything it needs. Dependencies resolve without
incident — the awkward ones are vendored (`hiqlite`, `rust_decimal`,
`s3-simple`) and the rest come from the registry.

## 3. What it costs

Measured 2026-09-01 on a two-core container, `plurxd` only:

| step | cold | warm |
|---|---|---|
| `git archive` + transfer | ~40 s | ~40 s |
| `cargo check -p plurxd --all-targets` | 4 min | ~35 s |
| `cargo clippy -p plurxd --all-targets -- -D warnings` | ~3 min | ~3 min |
| `cargo test -p plurxd --bin plurxd` (1504 tests) | ~4 min | ~2 min |
| `cargo fmt --all` | seconds | seconds |

Under ten minutes for the gate that rejects most branches, against fifteen to
forty per CI attempt — and the local run reports *every* error at once instead
of the first one to fail.

## 4. Working against a base that moves

`main` moves while a change is being built, so the verification has to be
re-pointed at the base the branch will actually land on:

1. Archive `main`, extract, build the change in the container.
2. **Run `cargo fmt --all` before generating the patch.** The gate runs
   `rustfmt --check` and the local suite does not, so a tree that passes 1500
   tests and Clippy still fails on a formatting diff — which is a round trip
   for a blank line. Then `diff -u` the changed files against a **pristine**
   extraction of the same archive. That is the patch, and it should contain
   only your edits: `fmt` reformats anything in the tree that was not already
   clean, and those hunks are not yours.
3. On the clone, branch off **current** `main` and `git apply` the patch.
4. Commit, `git archive` *that*, and re-verify in the container. Extract over
   the same tree so the warm `target/` survives; the second pass is minutes.

Step 4 is the one people skip and the one that matters. A verification against
the snapshot you started from is a statement about the snapshot. Only step 4
makes "the suite passes" a statement about the branch.

## 5. What it catches that CI charges for

From one change, in one sitting, each of which is otherwise its own round
trip:

- **A test asserting against a constant it guessed.** A seek-storm test spaced
  its exchanges below the server's own 250 ms floor and came back
  `RateLimited`. Two round trips, and the constant itself turned out to be
  worth knowing.
- **A check placed after a conversion.** `no field control_sequence on type
  SessionRequest` — the body had already become something else by that line.
- **Sixteen struct literals missing a new field, listed in one run.** Blind,
  each is a push.
- **`dead_code` on exactly the const and the method, not the field.** That is
  the compiler saying a design needs a consumer, precisely, rather than by
  argument.

## 6. Two limits

**Check which toolchain you actually got.** The container's default `cargo`
may not be the pin in [`rust-toolchain.toml`](../rust-toolchain.toml). If
`rustup` has the pinned version installed the toolchain file resolves and the
builds are the right compiler — but confirm it, because a lint that exists in
only one of the two is precisely what a local gate is meant to catch.

**Disk is a per-session allowance, not the volume.** An extraction plus a warm
`target/` runs to tens of gigabytes. `df` reports the volume, so "Avail" can
look healthy while writes fail. Delete old extractions rather than
accumulating them.

## 7. Do this first, not when stuck

Ten minutes at the start of a session, and it pays for itself on the first
mistake. It removes the failure this repository's agent sessions keep hitting:
a change *reasoned* to be correct, pushed, and found wrong twenty minutes
later — after which the fix is reasoned too, because the loop is still twenty
minutes long.
