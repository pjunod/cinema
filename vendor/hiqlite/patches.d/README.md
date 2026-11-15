# Patches carried after 2026-11-15, one file each

The numbered rows in [PLURX-PATCH.md](../PLURX-PATCH.md) are closed: a new
patch does not take the next number, extend that table or restate a count.
It is a file here, named after the patch, so two changes that each add a
patch write two paths and cannot conflict.

```toml
# patches.d/lease-idle-raft-clients.toml
patch = "Lease idle Raft client connections instead of dialing per `new_client`"
kind = "generic bug"        # "generic bug" · "plurx policy" · "dependency-only"
upstream = "pending M6"     # generic bug: `pending M6` or a public URL; otherwise "—"
drop = "Upstream release stops opening a connection per `RaftNetworkFactory::new_client`."
description = '''
What the patch does and why Plurx needs it, as the bullets in PLURX-PATCH.md do.
Files: `src/network/raft_client.rs`.
'''
```

The rules are the table's: a `plurx policy` drops `Never; …`, a
`dependency-only` patch names the release that retires it, and `description`
names at least one `src/...` or Cargo file that exists. `PLURX-FILES.toml`
still classifies every source, and a file named here counts as owned by a
row. The number of patches is derived — `python3 -m validation.vendor_patches`
prints it — and `tests/operations/test_hiqlite_patch_ledger.py` checks every
file. [docs/ci/SHARED-BOOKKEEPING.md](../../../docs/ci/SHARED-BOOKKEEPING.md)
covers every per-change directory.
