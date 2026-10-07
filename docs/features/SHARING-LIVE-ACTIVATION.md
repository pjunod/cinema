# Sharing activation on a running cluster

**Status:** open · **Updated:** 2026-10-07

## What failed

Saving Sharing succeeded, but rolling restarts left the deployed cluster at
replicated schema 81. Each process reported Source activation pending. The
startup coordinator required all admitted members to advertise fresh startup
attempts within the same twenty-second window, while no Local sessions,
preparations, requests or worker leases remained. Ordinary rolling maintenance
could not meet that requirement.

That requirement is rejected. Enabling Sharing must not require stopping the
whole cluster. The saved choice remains independent of readiness, and readiness
must describe current state instead of an immutable observation from boot.

## Why removing the check is unsafe

The Source migration changes seven playback table families. Its transaction
preserves Local rows and their ownership, leases, prepared successors and
foreign-key children. The data copy does not inherently require ending playback.

Running processes, however, cache their SQL layout. A Local operation may build
legacy SQL before migration and submit it after migration; a legacy read may
misinterpret a newly created Shared row. Removing the startup barrier without
fixing these races would expose failed writes and incorrect principal decoding.

## Decision

Keep the existing Raft transaction format and make session SQL dispatch safe
across the single, monotone schema 81 to 82 transition.

Every layout-dependent mutation carries an exact expected-version assertion as
its first statement. The assertion uses an additive, verified guard table and
has no effect when the expected layout is present. If it fails, the SQLite
writer may certify refusal only after proving that the first assertion failed
and explicit rollback succeeded. Only that certificate permits regenerating
that individual proposal once against the verified installed layout. A timeout,
lost response, later statement error or uncertain rollback never permits replay.
Earlier reads and earlier committed steps of a larger operation are not replayed.

Session SQL uses closed internal layout fragments. The adapter resolves every
fragment with one captured layout before submission; values remain parameters.
Prepending the assertion adjusts statement-output references and removing its
result preserves the caller's result indices.

Reads retain their result, including empty results and decoding errors, until a
consistent post-read layout check. A proven transition discards that old-layout
result and permits one read against the verified installed layout. A late legacy
observation cannot downgrade an installed-layout cache. Unknown or incomplete
layouts remain errors.

New processes provision and verify the guard before serving and advertise the
compatible-dispatch capability only with that implementation active. The existing
membership coordinator may install Source layout after it proves the complete
current member roster supports safe dispatch. Its transaction retains exact
schema, generation, selected-master, membership-transition and admission checks.
It preserves active playback records and worker leases. No new watchdog, polling
owner, wire enum or simultaneous restart is introduced.

Existing schema 82 is verified without rebuilding it. The Developer card reports
current verified Source activation; historical boot evidence remains historical.

## Work and evidence

| Work | Owner | State |
|---|---|---|
| Exact first-assertion rollback certificate in vendored SQLite writer | Sol compiler/runtime lane | implemented; qualification pending |
| Proposal dispatch and read transition safety | Sol session-store lane | implementing |
| Compatible member proof and live Source migration | Sol cluster lane | implementing |
| Current Developer readiness | Sol compiler/runtime lane | implementing |
| Integration, independent final review, retained qualification and merge | Root | pending |

The final regression set must cover a proposal constructed before migration and
submitted afterward, valid same-layout operations, failed rollback and later
statement errors, principal-safe reads crossing migration, preservation of live
Local ownership and children, incompatible member refusal, and current readiness
without restarting a process. Tests run after the independent final review under
the user's promotion-only policy; passing cases are retained. Windows and broad
unit-suite reruns are excluded.

No cluster stop, live database mutation or fleet deployment was performed during
the diagnosis. Implementation and deployment evidence will be recorded here when
available; a proposed design is not completion evidence.
