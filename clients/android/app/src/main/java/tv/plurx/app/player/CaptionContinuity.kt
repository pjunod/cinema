package tv.plurx.app.player

import androidx.media3.common.Player
import tv.plurx.app.data.CaptionReceipt
import java.util.UUID

/** Source traversal identity, independent of heartbeat CAS generations and renderer ownership. */
internal class CaptionContinuity(private val newId: () -> String = { UUID.randomUUID().toString() }) {
    var id: String = newId(); private set
    var discontinuitySerial: Long = 0; private set
    private var traversal: Pair<Long, Long>? = null
    private var ownedMutationDepth = 0
    private var viewerDestinationPending = false
    fun bind(fileId: Long, audioOrdinal: Long) {
        val next = fileId to audioOrdinal
        if (traversal != null && traversal != next) discontinuity()
        traversal = next
    }
    fun discontinuity() { id = newId(); discontinuitySerial++ }
    fun viewerSeekRequested() { viewerDestinationPending = true; discontinuity() }
    fun executionAttached(): Boolean {
        if (!viewerDestinationPending) return false
        viewerDestinationPending = false
        discontinuity()
        return true
    }
    fun endTraversal() { traversal = null; viewerDestinationPending = false; discontinuity() }
    fun rendererDiscontinuity(reason: Int) {
        if (ownedMutationDepth > 0) return
        // Playlist replacement is the Controller's continuous attachment path;
        // source/audio changes have their own explicit traversal transition.
        if (reason == Player.DISCONTINUITY_REASON_REMOVE) return
        discontinuity()
    }
    fun <T> ownedMediaMutation(block: () -> T): T {
        ownedMutationDepth++
        try { return block() } finally { ownedMutationDepth-- }
    }
}

/** An old request may resolve its CAS generation, but its UUID never replaces current intent. */
internal fun captionReceiptContinuity(receipt: CaptionReceipt, sent: String?) {
    require(receipt.continuity_id == sent) { "Caption continuity receipt changed" }
    if (sent != null) require(runCatching {
        val uuid = UUID.fromString(sent)
        uuid.toString() == sent && uuid != UUID(0, 0)
    }.getOrDefault(false)) { "Invalid caption continuity" }
}

/** One bounded lease capsule belongs to the logical playback, across a planned owner handoff. */
internal class CaptionProducerOwnership(private val newId: () -> String = { UUID.randomUUID().toString() }) {
    var playbackId: String = newId(); private set
    private var owner: Any? = null
    private var transferExpected = false
    private var releaseDemand: (() -> Unit)? = null
    fun claim(): Any = Any().also { owner = it; transferExpected = false }
    fun owns(token: Any): Boolean = owner === token
    fun retain(token: Any, release: () -> Unit) { if (owns(token)) releaseDemand = release }
    fun prepareTransfer() { if (owner != null) transferExpected = true }
    fun retireDemand() { val release = releaseDemand; releaseDemand = null; release?.invoke() }
    fun release(token: Any) {
        if (!owns(token)) return
        owner = null
        if (!transferExpected) retireDemand()
    }
    fun newTraversal() { transferExpected = false; retireDemand(); playbackId = newId() }
    fun retire() { owner = null; newTraversal() }
}
