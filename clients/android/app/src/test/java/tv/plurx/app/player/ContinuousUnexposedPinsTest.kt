package tv.plurx.app.player

import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Test

class ContinuousUnexposedPinsTest {
    private fun pin(n: Int) = buildJsonObject {
        put("artifact_id", n.toString(16).padStart(64, '0')); put("rendition_id", "c".repeat(64))
        put("timescale", 24); put("from_tick", n * 48L); put("through_tick", (n + 1) * 48L); put("byte_length", 1000)
    }
    private fun tx(id: String, revision: Long, superseded: Boolean, reserved: List<JsonObject>, disposed: List<Int> = emptyList()) = buildJsonObject {
        put("transaction_id", id); put("intent_revision", revision); put("intent_superseded", superseded)
        put("reserved", JsonArray(reserved)); put("disposed", JsonArray(disposed.map { JsonPrimitive(it.toString(16).padStart(64, '0')) }))
    }

    @Test fun supersededOwnersDisposeOnlyPinsThatNeverReachedTheQueue() {
        // A switch leaves the old owner one pin scheduled ahead of the boundary
        // that no loader ever opened; it previously stayed reserved until End.
        val old = tx("old", 4, superseded = true, reserved = listOf(pin(1), pin(2), pin(3)), disposed = listOf(3))
        val newest = tx("new", 5, superseded = false, reserved = listOf(pin(4), pin(5)))
        val result = ContinuousUnexposedPins.disposable(listOf(old, newest), 5, { false }, queued = listOf(pin(2)))
        assertEquals(listOf(ContinuousUnexposedPins.Pin("old", pin(1))), result)
    }

    @Test fun theNewestIntentKeepsItsPinsEvenWhenMarkedSuperseded() {
        val latest = tx("latest", 7, superseded = true, reserved = listOf(pin(9)))
        assertEquals(emptyList<ContinuousUnexposedPins.Pin>(),
            ContinuousUnexposedPins.disposable(listOf(latest), 7, { false }, queued = emptyList()))
    }

    @Test fun cancelledBeforeExposureStillQualifiesWithoutSupersession() {
        val target = tx("target", 2, superseded = false, reserved = listOf(pin(6), pin(7)))
        assertEquals(listOf(ContinuousUnexposedPins.Pin("target", pin(7))),
            ContinuousUnexposedPins.disposable(listOf(target), 3, { it == "target" }, queued = listOf(pin(6))))
        assertEquals(emptyList<ContinuousUnexposedPins.Pin>(),
            ContinuousUnexposedPins.disposable(listOf(target), 2, { false }, queued = emptyList()))
    }
}
