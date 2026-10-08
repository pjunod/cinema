package tv.plurx.app.remote

import java.util.UUID

/** UI-owner admission consumes before synchronous effect and records ACK after. */
internal class RemoteReceiverGuard {
    data class Context(val grantId: String, val target: RemoteTarget, val controlEpoch: String, val contextRevision: Long, val focusRevision: Long, val textNonce: String?)
    data class Ack(val control_epoch: String, val sequence: Long, val outcome: String)
    private data class Credit(val wire: RemoteCredit, val deadline: Long)
    private data class Result(val ack: Ack, val deadline: Long)
    private var context: Context? = null
    private var active = false
    private var highWater = 0L
    private var lastNow: Long? = null
    private val credits = ArrayDeque<Credit>()
    private val results = ArrayDeque<Result>()
    val currentCredits get() = credits.map { it.wire }
    fun invalidate() { credits.clear() }
    fun deactivate() { active = false; invalidate() }
    fun setContext(next: Context): RemoteOutcome? {
        val previous = context
        val sameControl = previous != null && previous.target == next.target && previous.controlEpoch == next.controlEpoch
        if (sameControl && (!active || previous?.grantId != next.grantId)) return RemoteOutcome.StaleControl
        if (!sameControl) { highWater = 0; results.clear(); invalidate() }
        else if (previous?.contextRevision != next.contextRevision || previous?.textNonce != next.textNonce) invalidate()
        context = next; active = true; return null
    }
    private fun observe(now: Long): Boolean {
        if (now < 0 || lastNow?.let { now < it } == true) { invalidate(); return false }
        lastNow = now
        credits.removeAll { now >= it.deadline }
        results.removeAll { now >= it.deadline }
        return true
    }
    fun mint(kind: RemoteCreditKind, now: Long): RemoteCredit? {
        if (!observe(now) || !active || now > Long.MAX_VALUE - kind.ttl) return null
        val value = RemoteCredit(UUID.randomUUID().toString(), kind.wire)
        if (credits.size == 16) credits.removeFirst()
        credits.addLast(Credit(value, now + kind.ttl)); return value
    }
    fun apply(command: RemoteCommand, now: Long, semantic: RemoteOutcome? = null, effect: () -> RemoteOutcome): RemoteOutcome {
        if (!observe(now) || runCatching { command.validate() }.isFailure) return RemoteOutcome.Invalid
        val current = context ?: return RemoteOutcome.Unavailable
        if (!active) return RemoteOutcome.Unavailable
        if (command.target != current.target) return RemoteOutcome.StaleTarget
        if (command.grantId != current.grantId) return RemoteOutcome.Unauthorized
        if (command.controlEpoch != current.controlEpoch) return RemoteOutcome.StaleControl
        if (command.sequence <= highWater) return RemoteOutcome.Duplicate
        val credit = credits.firstOrNull { it.wire.nonce == command.credit } ?: return RemoteOutcome.Expired
        if (credit.wire.kind != command.action.creditKind.wire) return RemoteOutcome.Invalid
        if (command.contextRevision != current.contextRevision) return RemoteOutcome.StaleContext
        if (command.action.type == "select" && command.focusRevision != current.focusRevision) return RemoteOutcome.StaleFocus
        if (command.action.type == "text_replace" && command.action.text("text_nonce") != current.textNonce) return RemoteOutcome.StaleContext
        if (semantic != null) return if (semantic == RemoteOutcome.Applied) RemoteOutcome.Invalid else semantic
        if (now > Long.MAX_VALUE - 10_000) return RemoteOutcome.Invalid
        highWater = command.sequence
        val outcome = effect()
        if (results.size == 64) results.removeFirst()
        results.addLast(Result(Ack(command.controlEpoch, command.sequence, outcome.wire), now + 10_000))
        return outcome
    }
    fun result(epoch: String, sequence: Long, now: Long): Ack? {
        if (!observe(now)) return null
        return results.firstOrNull { it.ack.control_epoch == epoch && it.ack.sequence == sequence }?.ack
    }
}
internal class RemoteControlSequence {
    private data class Key(val target: RemoteTarget, val grant: String, val epoch: String)
    private val values = linkedMapOf<Key, Long>()
    fun knows(target: RemoteTarget, grant: String, epoch: String) = values.containsKey(Key(target, grant, epoch))
    fun acquired(target: RemoteTarget, grant: String, epoch: String) {
        val key = Key(target, grant, epoch)
        if (key in values) return
        if (values.size == 32) values.remove(values.keys.first())
        values[key] = 0
    }
    fun next(target: RemoteTarget, grant: String, epoch: String): Long? {
        val key = Key(target, grant, epoch)
        val previous = values[key] ?: return null
        if (previous >= REMOTE_MAX_INTEGER) return null
        return (previous + 1).also { values[key] = it }
    }
}
internal class RemoteSceneEligibility {
    var eligible = false; private set
    var foregroundId = UUID.randomUUID().toString(); private set
    private var backgrounded = false
    fun transition(resumed: Boolean, background: Boolean) {
        if (background) backgrounded = true
        if (resumed && backgrounded) { foregroundId = UUID.randomUUID().toString(); backgrounded = false }
        eligible = resumed
    }
}
internal class RemotePreviewOwnership {
    private var position: Long? = null
    fun claim(before: Long?, after: Long?) { if (after != null && before != after) position = after }
    fun owns(current: Long?) = position != null && position == current
    fun clear() { position = null }
    fun cancel(current: Long?): Long? { val next = if (owns(current)) null else current; clear(); return next }
}

internal class RemotePairingSurfaceLifetime {
    private var generation = UUID.randomUUID().toString()
    fun begin(): String { retire(); return generation }
    fun retire() { generation = UUID.randomUUID().toString() }
    fun accepts(ticket: String) = ticket == generation
}
internal object RemoteStateUpdate {
    fun applying(previous: kotlinx.serialization.json.JsonObject?, reply: kotlinx.serialization.json.JsonElement?): kotlinx.serialization.json.JsonObject? =
        if (reply == null || reply == kotlinx.serialization.json.JsonNull) previous else reply as? kotlinx.serialization.json.JsonObject
}
internal object RemoteEffectEligibility {
    fun sessionAlive(resumed: Boolean) = resumed
    fun semanticEffects(resumed: Boolean, mainWindowFocused: Boolean, ownedWindowFocused: Boolean) = resumed && (mainWindowFocused || ownedWindowFocused)
}

internal object RemotePairingAdmission {
    fun accepts(generation: String, ticket: String, receiver: String?, expectedReceiver: String, target: RemoteTarget?, expectedTarget: RemoteTarget) =
        generation == ticket && receiver == expectedReceiver && target == expectedTarget
}


internal class RemoteRestrictionLifetime {
    private var restricted = false
    fun transition(next: Boolean): Boolean { val changed = next != restricted; restricted = next; return changed }
    fun reset() { restricted = false }
}

internal class RemotePairingDeadline(start: Long, budget: Long = 120000) {
    private val start = start
    private val deadline = start.takeIf { it >= 0 && budget > 0 && it <= Long.MAX_VALUE - budget }?.plus(budget)
    fun admits(now: Long) = deadline != null && now >= start && now < deadline
    fun remaining(now: Long) = if (admits(now)) deadline!! - now else 0
}
internal class RemoteCommandResult {
    private var epoch: String? = null
    private var sequence = 0L
    private var outcome: RemoteOutcome? = null
    fun begin(epoch: String, sequence: Long) { this.epoch = epoch; this.sequence = sequence; outcome = null }
    fun observe(epoch: String, sequence: Long, result: RemoteOutcome): Boolean {
        if (this.epoch != epoch || this.sequence != sequence) return false
        outcome = result; return true
    }
    fun known(epoch: String, sequence: Long) = outcome.takeIf { this.epoch == epoch && this.sequence == sequence }
    fun reset() { epoch = null; sequence = 0; outcome = null }
}
