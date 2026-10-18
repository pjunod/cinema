# macOS: cold parser compile exceeds the identity deadline

**Status:** paused (Paul, 2026-10-17) · **Backlog:** P28 · **Verified against:** v0.7.0 `9241ae064` · **Blocks:** the v0.7.1 GitHub release

## Summary

On macOS, startup compiles the packaged source parser (`plurx-source-parser.wasm`, about 15 MB) inside the 10 s FFprobe identity deadline. The compile is cold and runs on one thread. When it misses the deadline, plurxd logs `FFprobe identity is unavailable … bound FFprobe exceeded its deadline` and runs without explicit decoder facts until it restarts.

## Evidence

- **GitHub release, v0.7.0:** the `macOS universal archive` install check failed twice. In both runs the warning came about 10 s after launch, and readyz took 55 to 75 s.
- **macOS lab host:** `~/Library/Logs/plurxd.log` has the same warning on 2026-10-13 at 20:42, 10.3 s after `listening`, while the host was under CI load. Other starts that day succeeded, so this is a race against load and single-thread speed, not a slow-runner artefact.
- **2026-10-11 dry runs passed** only because they never reached this path. They shipped no parser and didn't grep the log.

## Root cause (code at v0.7.0)

- `decode_facts.rs:994-998` runs `snapshot_executable` under `run_bounded_identity_task(IDENTITY_DEADLINE = 10 s)` (`:73`, `:3360-3416`).
- On macOS, `snapshot_executable_in` (`:3150-3220`) copies the image, re-reads it, hashes it, then calls `macos_wasm::Parser::new` (`:3215`).
- `Parser::new` (`decode_facts/macos_wasm.rs:56-99`) runs `Engine::new` and `Module::new`: a cold Cranelift compile with no cache.
- `crates/plurxd/Cargo.toml:98` enables wasmtime `std`, `runtime` and `cranelift` but not `parallel-compilation`. The whole module therefore compiles on one blocking thread, whatever the core count.
- The deadline exists to bound blocked file reads. It also covers CPU-bound compile time, which scales with the machine.
- There is no competition inside plurxd. Identity discovery runs alone, before `detect_encoders` (`main.rs:2880-2893`). Its 1-permit `identity_gate()` (`:108`) has nothing else holding it at startup.
- The timing points to the snapshot+compile step: about 10.2 s after launch. The 5 s version deadline would have fired earlier.
- Discovery runs once. `main.rs:2886-2889` stores `None`, `with_decode_probe` (`transcode/manager/construct.rs:247`) is set once at boot, and nothing retries.

## Impact when the identity is `None`

- `transcode/manager/plan.rs:477`: no fresh descriptor-bound source probe.
- `plan.rs:645`: catalogue-only `resolve_movie_plan`. Geometry requests fail with `candidate_geometry_unavailable`.
- `plan.rs:514`: prepared-source resolution refuses with `vod_decoder_plan_refused`.
- `candidates.rs:893`: HEVC interlace refinement is skipped.
- `create.rs:1501/1552`: clock reporting falls back to the native `ffprobe` capture instead of the sealed parser.
- `http/developer.rs:1398`: "Measured FFmpeg and FFprobe" shows unmet.

## What changed since 2026-10-11

- `684f46583`, `22bde79a8`: the installer ships the parser, and the smoke test greps for the warning.
- `ff1c54b8f` (2026-10-12): `cranelift_opt_level(None)`. Its comment notes that optimizing "consumed most of the cold identity budget", so the margin was already known to be thin.
- `d52d1a7d5` / #1117 (2026-10-15): the module grew about 180 KB.
- The design dates from `35f7a8cf6` (2026-10-08).

## Proposed fix

- **Recommended:** keep the deadline on the file reads and hashing only. Run the compile afterwards on a blocking thread with its own explicit budget, reported as a named startup phase. Also enable wasmtime's `parallel-compilation` feature.
- **Not the fix:** a longer timeout or a retry loop. Either one just moves the cliff.
- **Worth a separate look:** an identity that missed its deadline at startup should be able to come back without a restart.

## Progress and what's left

- [x] Root cause traced in code, with both CI failures and the lab-host occurrence.
- [ ] Measure a cold `Module::new` on an idle macOS lab host, before and after `parallel-compilation`.
- [ ] Build the fix, with a test that fails when the compile is put back under the read deadline.
- [ ] Cut v0.7.1, which needs this and #1163 (the arm64 subtitle-engine build times out under QEMU).

## Release context (2026-10-17)

- v0.7.0 is tagged on `9241ae064` and its release commit is on main (#1154).
- Its local image publication is blocked by #1163. The publisher builds from the tag's sources, so a fix on main can't rescue 0.7.0.
- Its GitHub release is blocked by this bug.
- #1162 and #1155 fixed the publisher's changelog-heading check, a SIGPIPE under pipefail.
