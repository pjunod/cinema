# Client fix rows — one file per pull request

A pull request that adds a `tests/client-fixes.toml` row puts it in a file of
its own here instead, `tests/client-fixes.d/<id>.toml`, holding one or more
`[[fixes]]` tables in the ledger's own shape (no `version` or
`enforce_after`). `validation/history.py` reads the ledger and this folder as
one, so a row here is enforced exactly like a row in the ledger, and two pull
requests adding rows never edit the same lines.

`scripts/fold` moves these rows into the ledger and deletes the files once the
folder holds more than its bound (`scripts/fold --list`), and at every release
cut. Edit an existing row where it is; only new rows start here. Paperwork
refuses a pull request that adds a `[[fixes]]` row to the ledger itself.
