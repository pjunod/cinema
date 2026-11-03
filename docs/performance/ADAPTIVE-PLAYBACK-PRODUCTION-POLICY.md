# Adaptive playback — production policy wiring

**Status:** open · implementation local, qualification pending · updated
2026-11-03. Continuation of draft effort #1248 after the selected
[finite lifecycle and restart qualification](ADAPTIVE-PLAYBACK-DELIVERY.md#finite-lifecycle-and-restart-qualification).
The [resource plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md) remains normative.

## Ordinary admission comes first

Cached S selection and conservative finite admission retain their existing
order. A successful ordinary cold start does not require speculative bounds.
Only a known capacity refusal before a new R reservation attaches a typed
trial-policy refusal to the original adaptive attempt. Invalid identities,
stale epochs and protocol failures are not reclassified as capacity evidence.

The selected software producer still has unknown native memory/task peaks and
unverified startup/cleanup bounds. Its refusal adapter consumes `TrialBudget`
and the same continuous preparation cutoff. Activation A was already reserved
upstream, so the adapter reserves no second activation allowance. An absent,
expired, discontinuous or substituted original cutoff refuses speculation.
Neither this adapter nor growth evidence can return a trial permit.

## Refusal keeps its original owner

The bounded original attempt retains the typed reason through local results,
failed replay, remote wait and repeated Grant. The authenticated worker returns
a bounded versioned refusal body. A policy refusal does not prove that its
original fixed planning work, task or metadata has closed.

Trying another worker first fences that same attempt, then checks the original
identity and worker generation for positive `Closed` under the unchanged
preparation cutoff. The worker publishes that state through its existing
coordinator-join and P/R cleanup barriers. Missing or uncertain closure holds
the request; the control-plane row is not a native refund receipt. Compact
planning metadata may remain charged through its own final retirement.

## Real service feeds bounded class state

Successful durable `Materialized` processing forwards the encoding's original
service sample through its existing borrowed `SoftwareVodOwner`. It does not
reacquire the producer mutex held by the driver. The node owns a fixed inline
16-class registry keyed by the core's runtime epoch and workload identity.
Observation allocates no new map, and exhaustion loses expansion availability
instead of evicting a cooldown.

The original ledger family, generation and effective limit epoch must still
match. Duplicate, stale, mismatched or paced observations supply no positive
stability. Producer replacement resets stability while preserving class
cooldown. Joined/failed production clears positive evidence; generic failures,
intentional pacing and missing evidence do not manufacture capacity failures.
Retained reads cannot advance useful work or refresh sample timestamps.

## Qualification and remaining work

Compilation, focused execution and independent final review are pending for
this local slice. The preceding finite receipts remain source-bound evidence;
they do not qualify these new policy changes.

This implements refusal of speculation and service observation, not production
trial execution or safe expansion. Verified containment/startup/cleanup bounds,
the complete affected-work inventory and an attempt-owned pre-Winner driver
remain prerequisites. No `TrialMonitor` is instantiated without that owner.
M2, V1–V5 and Main promotion remain open; no deployment is claimed.
