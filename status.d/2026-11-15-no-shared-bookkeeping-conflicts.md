# No pull request conflicts on shared bookkeeping

Owner Claude (claude-fable-5-2, session 01BCWL9z), branch
`claude/no-shared-bookkeeping-conflicts`, PR #1648 (draft); Ansible companion
`pjunod/ansible#18` (draft, pushed). Changelog entries, client fix anchors,
count changes, governed paths, vendored hiqlite patches and status entries
are one file per change; store build numbers are assigned at release, every
build path takes its tree from `validation/mobile_build.py`, and the
published-build floor lives in the forge. Review rounds 1 and 2 addressed
except two left for Paul's ruling: narrowing the whole-file pins in
`verification/*/mapping.json`, and deriving `main-preflight-inputs.json`
(invalidates every unit receipt once). The Ansible change must reach the
controller before or with this one.
[Layout, reasons and what an open pull request does](../docs/ci/SHARED-BOOKKEEPING.md).
