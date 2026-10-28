package tv.plurx.app.player

import kotlinx.coroutines.CancellationException
import tv.plurx.app.data.CaptionReceipt

/** One uncertain server mutation survives renderer replacement with its exact wire body. */
internal class CaptionProducerMutations<T>(private val ownership: CaptionProducerOwnership) {
    internal class Pending<T>(val request: T, val explicitResume: Boolean, val stopRevision: Long,
        val authorized: () -> Boolean, val send: suspend () -> CaptionReceipt) {
        internal var ticket: Any? = null
        internal var runningOwner: Any? = null
        internal var runningCurrent: (() -> Boolean)? = null
        fun requiresStop(currentStopRevision: Long, stopped: Boolean, resolveOnly: Boolean = false): Boolean =
            resolveOnly || stopRevision != currentStopRevision || stopped && !explicitResume
    }
    internal data class Settled<T>(val pending: Pending<T>, val receipt: CaptionReceipt)
    var pending: Pending<T>? = null; private set
    fun install(owner: Any, value: Pending<T>): Pending<T> {
        check(ownership.owns(owner))
        return pending ?: value.also { pending = it }
    }
    suspend fun settle(owner: Any, current: () -> Boolean): Settled<T>? {
        val value = pending ?: return null
        if (!ownership.owns(owner) || !current()) return null
        check(value.authorized()) { "Caption authority changed; reopen this source." }
        if (value.runningOwner === owner && value.ticket != null && value.runningCurrent?.invoke() == true) return null
        val ticket = Any()
        value.ticket = ticket
        value.runningOwner = owner
        value.runningCurrent = current
        try {
            val receipt = value.send()
            // Even an uncancellable old transport can finish after the replacement's replay.
            if (pending !== value || value.ticket !== ticket || !ownership.owns(owner) || !current()) return null
            check(value.authorized()) { "Caption authority changed; reopen this source." }
            pending = null
            return Settled(value, receipt)
        } catch (e: Exception) {
            if (e !is CancellationException && !captionActionAmbiguous(e) && pending === value &&
                value.ticket === ticket && ownership.owns(owner) && current()) pending = null
            throw e
        } finally {
            if (value.ticket === ticket) { value.ticket = null; value.runningOwner = null; value.runningCurrent = null }
        }
    }
    fun retire() { pending = null }
}

internal data class CaptionProducerRequest(val fileId: Long, val binding: CaptionBinding,
    val create: tv.plurx.app.data.CaptionCreate? = null, val retry: CaptionRetryAttempt? = null) {
    init { require((create == null) != (retry == null)) }
    fun matches(file: Long, chosen: CaptionBinding): Boolean = fileId == file &&
        binding.options.source_size == chosen.options.source_size && binding.options.source_mtime == chosen.options.source_mtime &&
        binding.options.timeline_contract == chosen.options.timeline_contract && binding.options.source_origin_ms == chosen.options.source_origin_ms &&
        binding.audio.audio_ordinal == chosen.audio.audio_ordinal && binding.profile.profile_id == chosen.profile.profile_id &&
        binding.selection.task == chosen.selection.task && binding.selection.spokenLanguage == chosen.selection.spokenLanguage
}
