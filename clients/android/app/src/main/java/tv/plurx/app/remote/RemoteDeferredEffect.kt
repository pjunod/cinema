package tv.plurx.app.remote

/** All identity comes from the current typed UI owner, never remote payload IDs. */
internal data class RemoteDeferredBinding(val view: String, val controller: String, val fullReference: String)
internal class RemoteEffectPermit internal constructor(
    val command: RemoteCommand,
    val context: RemoteReceiverGuard.Context,
    val binding: RemoteDeferredBinding,
    val deadline: Long,
    val resultDeadline: Long,
    val invalidation: Long,
) {
    var retired = false; private set
    fun retire() { retired = true }
}
internal sealed interface RemoteReservation {
    data class Refused(val outcome: RemoteOutcome): RemoteReservation
    data class Admitted(val permit: RemoteEffectPermit): RemoteReservation
}
/** Deferred effect and owned UI completion are separate. run never navigates
 * before the guard records its actual terminal result. */
internal data class RemoteDeferredEffect(
    val binding: RemoteDeferredBinding,
    val stillOwned: () -> Boolean,
    val run: suspend (check: () -> Boolean) -> RemoteOutcome,
    val didComplete: (RemoteOutcome) -> Unit = {},
    val dispose: suspend () -> Unit = {},
)
