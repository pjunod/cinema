package tv.plurx.app.player

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject

/** Reservations whose owner can dispose them without any physical retirement
 * proof. The caller holds loader quiescence and the resource barriers. An owner
 * qualifies when its target was cancelled before exposure, or when a newer
 * intent superseded it; a pin qualifies only when no loader ever gave it queue
 * provenance in this attachment, so its bytes never reached the pipeline. */
internal object ContinuousUnexposedPins {
    data class Pin(val transaction: String, val interval: JsonObject)

    fun disposable(
        transactions: List<JsonObject>,
        latestRevision: Long?,
        cancelledBeforeExposure: (String) -> Boolean,
        queued: Collection<JsonObject>,
    ): List<Pin> = transactions.flatMap { tx ->
        val id = tx.text("transaction_id") ?: return@flatMap emptyList()
        val superseded = tx["intent_superseded"]?.wireBoolean() == true && tx.number("intent_revision") != latestRevision
        if (!cancelledBeforeExposure(id) && !superseded) return@flatMap emptyList()
        val disposed = tx.getValue("disposed").jsonArray
        tx.getValue("reserved").jsonArray.map { it.jsonObject }
            .filter { pin -> pin.getValue("artifact_id") !in disposed && pin !in queued }
            .map { Pin(id, it) }
    }
}
