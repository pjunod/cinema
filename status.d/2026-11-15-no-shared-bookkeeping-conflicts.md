# No pull request conflicts on shared bookkeeping

Owner Claude (claude-fable-5-2, session 01BCWL9z), branch
`claude/no-shared-bookkeeping-conflicts`, PR #1648 (draft); Ansible companion
`pjunod/ansible#18` (draft, pushed). Changelog entries, client fix anchors,
count changes, governed paths, vendored hiqlite patches and status entries
are one file per change; store build numbers are assigned at release, every
build path takes its tree from `validation/mobile_build.py`, and the
published-build floor lives in the forge. Review rounds 1 to 3 addressed
(round 3: forward-only leased ref push, ledger line before the push, one
`publish.lock` for both upload paths, release-cut folds before it writes)
except two left for Paul's ruling: narrowing the whole-file pins in
`verification/*/mapping.json`, and deriving `main-preflight-inputs.json`
(invalidates every unit receipt once). Lands in the five steps under
"Landing order" in the document below: Ansible #18 first, then this with
`train:solo`.
[Layout, reasons and what an open pull request does](../docs/ci/SHARED-BOOKKEEPING.md).
