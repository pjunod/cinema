# No pull request conflicts on shared bookkeeping

Owner Claude (claude-fable-5-2, session 01BCWL9z), branch
`claude/no-shared-bookkeeping-conflicts`, PR #1648 (draft). Changelog entries,
client fix anchors, count changes, governed paths, vendored hiqlite patches
and status entries are one file per change; store build numbers are assigned
at release, and every build path takes its tree from
`validation/mobile_build.py`. Review items 1-7 addressed; the Ansible
companion (`pjunod/ansible`, branch `claude/cinema-release-stamp`) is
committed locally and not yet pushed. The first TestFlight/Play publish after
it lands is the acceptance of the release stamp.
[Layout, reasons and what an open pull request does](../docs/ci/SHARED-BOOKKEEPING.md).
