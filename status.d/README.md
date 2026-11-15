# Efforts in flight, one file each

`scripts/status` lists these newest first; `scripts/status --full` prints
their text. Each file is one effort, owned by the session working on it.
Because nobody else edits that file, a status update never conflicts with
another pull request.

```markdown
# CM4 trim fidelity

Owner Claude (claude-opus-5-5, session 01BCWLA2), PR #1591, draft. Six-vector
knots used a fixed BT.2020 palette; fix built, reference qualification next.
[Ledger](../docs/streaming/DV_HDR_PROCESSING_STATUS.md).
```

- **Name:** `<YYYY-MM-DD>-<slug>.md`, the date the effort started; lower-case
  letters, digits and `-`.
- **First line:** `# <Title>`. Then who owns it, where it stands, and a link
  to the ledger that holds the detail. Links are relative to this directory
  (`../docs/...`).
- **Size:** at most 30 lines and 2,400 bytes. An entry is a pointer; the
  ledger is the record.
- **Update** by editing your own file. **Finish** by deleting it; if the
  effort deserves a permanent row, add one to its folder's
  `docs/<area>/STATUS-HISTORY.md`.

`tests/operations/test_status_page_shape.py` enforces the shape, and
`test_status_pr_claims.py` reads every file here, so an entry may not call a
merged pull request open.
[docs/ci/SHARED-BOOKKEEPING.md](../docs/ci/SHARED-BOOKKEEPING.md) covers every
per-change directory.
