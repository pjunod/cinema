# Client fix anchors, one file per change

A corrective client commit records its source and test anchors here, in a
file of its own, instead of appending to `tests/client-fixes.toml`. That file
keeps the rows recorded before 2026-11-15 and the ledger's `version` and
`enforce_after`; it gains no new rows.

```toml
# tests/client-fixes.d/android-reader-canonical-origin.toml
[[fixes]]
id = "android.reader-canonical-server-origin"
commits = ["0e946dcfee51"]
source = "clients/android/app/src/main/java/tv/plurx/app/ui/ReaderScreen.kt"
source_anchor = "val canonical = readerOrigin(origin) ?: return null"
test = "clients/android/app/src/test/java/tv/plurx/app/ui/ReaderPolicyTest.kt"
test_anchor = "fun shellUsesTheSameCanonicalServerIdentityAsTheSession()"
```

A file holds one or more `[[fixes]]` tables and nothing else; a supplementary
row may name a primary in the ledger or in any fragment. Ids are unique across
the ledger and every fragment. `validation.history.load_client_fixes` reads the
ledger, then the fragments in file-name order.
[docs/ci/SHARED-BOOKKEEPING.md](../../docs/ci/SHARED-BOOKKEEPING.md) covers
every per-change directory.
