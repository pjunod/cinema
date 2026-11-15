# Linux player session control

Owner Codex (Sol implementation). [Ledger](../docs/clients/LINUX-PLAYER-CINEMA-UI.md#session-control-sequence-repair-051). Session replacement reused a run-wide counter despite the fresh-session sequence-1 contract, causing stale controls and lease expiry. Source repaired and independently reviewed; final affected validation recorded in the PR. DV reset interpretation, deployment and physical acceptance remain distinct.
