# Native CI receipts — retain passing tests when their inputs match

**Status:** built · **Decision:** Paul, 2026-10-28 · **Scope:** main fast lane

Companion to [Python unit receipts](PYTHON-UNIT-PR-RECEIPTS.md) and
[the development pipeline](../DEVELOPMENT_PIPELINE.md). This describes native
test adoption; the [subtitle completion status](../features/SUBTITLE-COMPLETION-STATUS.md)
records actual qualification and remaining physical-device acceptance.

## Original evidence remains attributable

The Apple job verifies the current pinned tools. `validation.native_receipts`
can retain original build and native-test successes for unchanged inputs, or
leave those commands due when proof is unavailable. It can adopt an original
successful main-fast-lane Apple job from the same repository. A different PR
must have merged from that exact tested source, and both its source and merge
commit must be ancestors of the current candidate. The original ready event,
checkout, job, first attempt and artifact publication are authenticated through
the CI API. Adoption evidence retains those original identities; it never
rewrites the original receipt as a new execution. The retained operations name
the original build and test commands. A TV artifact proves the reviewed job
ran `make apple-build` before its journeys; a desktop XCTest artifact proves
`make desktop-macos-check`, including both architecture builds.

TV navigation requires a clean complete source snapshot, the exact required
and executed journey list, all journeys passed with no failures or skips, and
the recorded pinned Xcode/runtime contract. Desktop adoption requires the
original nonempty successful XCTest summary. An overall workflow failure in
another job does not erase a successful native job.

## Input changes make tests due

Each family compares a conservative source closure, including paths, modes,
blob identities, additions and removals. Reviewed build specifications define
the dependency boundary. Unknown specifications or nonregular source entries
withhold reuse until their dependency boundary is reviewed. Mobile closure
includes the Apple project and its explicit external bundled resources;
desktop closure includes its package, build script and packaging inputs.

Missing, expired, ambiguous, corrupt or incompatible evidence leaves builds
and tests due. Current-run integrity failures fail the job. Artifact reads have byte,
member and expansion bounds; repository credentials never follow redirects.
The `native-adoption-<run>-<attempt>` artifact records provenance and the
matched input digest. Adopted runs emit no original TV artifact; their desktop
log is replaced by explicit adoption text that cannot authenticate as an
original XCTest pass. Main promotion still requires the current Apple job.

Legacy schema-1 receipts bind the pinned Xcode, runner architecture and tvOS
runtime identifier. They do not identify the host OS build, prove physical
Apple TV behavior, or qualify a deployed server binary.
