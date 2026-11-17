# PR #1592: landing and qualification evidence

**Status:** open — historical evidence audited; positive coverage closure belongs to PR #1640 · **Updated:** 2026-11-16

PR #1592 landed through a direct API merge with the train off after its final
source run failed. The account recorded by Forgejo identifies the credential,
not the person or agent that used it. No rationale for overriding this failed
gate was found in the preserved PR discussion. A later history repair recorded
the landing accurately but expressly supplied no test execution evidence.

## 1. Exact source and landing

| Record | Identity |
|---|---|
| Final source | `8ca0c378fd5def0c8ca3e35559116e1ab91c06dd` |
| Landing, 2026-11-15 13:37:02 UTC | `22ec9dc3b39edffa31e170a9305c9653f3bb028b` |
| Source and landing tree | `c6a25b158ab41929fce749a85f7d9abcbe188353` |
| Final source run | internal 7916 / public 7697 |
| Retrospective history record | `4f87170cba861f15ef941d169ccbd4339774a07f`, PR #1654 |

Preflight failed at 13:35:00 UTC because corrective commit
`64fc34d63292dde9c24feae68d010bf3edda1a02` had duplicate primary claims.
The inherited main trailer gaps were explicitly nonfailing diagnostics.
Promotion failed at 13:35:04 UTC. The merge followed at 13:37:02 UTC.

Run 7916 published four artifacts, with no Rust selection, plan, reuse
provenance, shard outcomes, or verdict. Its final preflight outcome map was
empty and its Python receipt was incomplete with no passes. The final
artifact ZIP digests are:

| Artifact | SHA-256 |
|---|---|
| 8606, final preflight | `7644f61988c988a9890ccb07d31dac094c2d9a00fd0ed812062739e649735d0c` |
| 8605, final Python | `597dcb7ef4d6b5955a46a0b03f2505a1b46e9d011691673f8a6d7c61fb211b43` |
| 8603, preflight start | `b5542bd668b928c6a38e99f76940a78315ce969ed02cf54685e79a04f60a867b` |
| 8602, Python start | `ec50e1ae8da38b4fec47ab7653b7d45e7419d4c1d6e3e8261d97c68223ee7806` |

The abandoned train #3 used a different source and tree. Its 8,211 passes
and 40 failures cannot be substituted for the final landing's coverage.
The retained controller journal starts after this landing, so it cannot
establish an exhaustive historical absence of every earlier train record.
The direct merge and the final failed run are positively established.

## 2. Applicable later evidence

Run 7943 / public 7724 published nine authenticated receipts after the merge.
Authentication checked source keys, publisher uploads and actual shard log
outcomes using `validation.rust_units.Authenticator`.

- The exact landed daemon key has 5,229 required IDs: 5,186 passed and 43
  failed. All eight declared daemon regressions have positive evidence in
  this later run. These results cannot authorize the earlier landing.
- Eight other exact landed-key crates have 233 passes and no failures.
- That run's core key differs from the landed core key. Its results cannot
  fill the missing landed-core evidence.
- The PR declared 27 regressions: 12 core, eight daemon, three Python, two
  Android, and two browser cases. The other 19 declarations still lack
  applicable positive evidence established by this audit.

The final source run never published a complete Rust plan. An exact full
required population for that tree cannot be invented from the older plan.
The 43 later daemon failures describe that exact key, not today's source.

## 3. Closing coverage without replaying valid passes

Use the final candidate's content keys and actual listing. Preserve every
applicable authenticated PASS and run only the failed, missing, or
invalidated cases. Verify declared regressions against their real producer:
Rust nextest IDs, Python receipts, Android unit results and browser tests.
A syntax check cannot establish a browser behavior result.

PR #1640 makes every due Rust case require a positive pass, keeps baseline
classification diagnostic, rejects incomplete producer outputs and prevents
a skipped selected Rust stage from satisfying main promotion. Its harness
changes invalidate all Rust crate keys under the existing input contract;
previous-key passes remain historical evidence. Subsequent continuations
must retain valid new-key passes. See [the execution and reuse contract](RUST-UNIT-FANOUT.md).
