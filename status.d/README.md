# Status entries — one file per effort

`STATUS.md` is generated from this folder by `scripts/fold`; never edit it.
Every effort owns one file here, so two pull requests never touch the same
lines, and the page is rebuilt after each merge by the fold workflow
(`.github/workflows/fold.yml`).

- **Name:** `status.d/<YYYY-MM-DD>-<slug>.md`, dated when the effort started.
  The page lists entries newest first by that date.
- **Content:** one bullet, `- **YYYY-MM-DD · Title:** owner, state, next step,
  [ledger](path)`, or one `## YYYY-MM-DD · Title` section for a longer note.
- **Update** your own file in place as the effort moves; do not edit another
  effort's file.
- **Retire** an effort by moving its text verbatim into its subject folder's
  `STATUS-HISTORY.md`, deleting its file here, and adding a row to
  [INDEX.md](INDEX.md). `tests/operations/test_status_page_shape.py` fails when
  the rendered page passes its length budget.

`scripts/fold` prints whether `STATUS.md` matches this folder; `scripts/fold
--write` rewrites it.
