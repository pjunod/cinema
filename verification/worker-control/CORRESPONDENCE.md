# Controller correspondence — emitted effects and real driver operations

**Status:** implementation integrated; Fable final review delivered and amendments implemented; five selected runtime schedules passed. Complete proof qualification and landing are tracked in [PR #1260](http://forge.lan:3000/noirr/plurx/pulls/1260).

The production worker and proof harness call the identical `library_lifecycle::step` function. The driver owns the real guards and asynchronous operations. The [machine-readable table](correspondence.json) names related actions in the unchanged worker model. This descriptive relation is not a trace translator, enabledness equivalence or fair simulation proof.

| Controller effect | Existing driver operation | Related model actions |
|---|---|---|
| `AcquireDomain` | `self.acquire_job(format!("scan:library:{library_id}"))` | `DomainSend`, `StoreApply`, `StoreReply`, `CallerReturn` |
| `Bind` | `fence.bind_library(...)` | `WorkerTokenIntent`, `WorkerTokenGrant`, `AuthorityReturn`, `WorkerPubIntent`, `WorkerPubGrant`, `BindSend` |
| `Process` | `self.process_library_requests(...)` | `ScanStart`, `WalkReturn`, `ScanFinish`, `ItemSend`, `CompleteSend`, `Reconcile` |
| `RevokeDomain` | `lease.revoke()` | `Cancel`, `QueueLoss`, `ReleaseJoin` |
| `CancelWork` | `cancellation.cancel()` | `Cancel`, `QueueLoss` |
| `AwaitWorkJoin` | `work.as_mut().await or selected completed result` | `WalkReturn`, `ScanFinish` |
| `DropPublisher` | `drop(publisher.take()...)` | No dedicated action |
| `ReleaseDomain` | `lease.take().release().await` | `ReleaseJoin`, `ReleaseSend`, `ReleaseFailure`, `ReleaseRetry`, `CallerReturn` |
| `Settle` | `settle_library(...)` | `SettleSend`, `CallerReturn` |
| `RetireQueue` | `active.take().finish().await` | `FinishJoin`, `FinishRetire` |
| `DropAdmission` | `drop(admission.take()...)` | `DropAdmission` |

Initialization starts only after confirmed queue claim and retained physical admission. Unsupported payload rejection remains before controller entry. Outcome events describe returned acquisition/bind/work results; `DomainReleased` describes completion of a local release attempt, including an ambiguous failure. Its timestamp is sampled after that attempt, preserving the original signed `i64` policy. The controller proves neither successful durable release nor SQL guard correctness.

The biased select, 250 ms pressure observation, queue/domain loss signals, token-mutex → publication-read-lock order, heartbeat joins and processing internals remain in their existing layer. Selected fault-point schedules check driver ordering; they do not establish every possible asynchronous trace. Dropping a publisher handle is distinct from releasing the publication RwLock read guard inside a fenced call.

The old model permits overlapping same-domain claims that the durable source-I/O gate can refuse. Its liveness premises cannot automatically transfer to this driver. Retained model receipts keep their original source identity; new source-hash mappings describe this extraction and do not relabel old Rust executions. No runtime watchdog, feature switch or test replacement is introduced.
