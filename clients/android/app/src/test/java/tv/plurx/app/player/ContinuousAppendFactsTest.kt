package tv.plurx.app.player

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ContinuousAppendFactsTest {
    private fun hex(n: Int) = n.toString(16).padStart(64, '0')
    private fun pin(n: Int) = buildJsonObject {
        put("artifact_id", hex(n)); put("rendition_id", "c".repeat(64))
        put("timescale", 24); put("from_tick", n * 48L); put("through_tick", (n + 1) * 48L); put("byte_length", 1000)
    }
    private fun tx(id: String, reserved: List<JsonObject>, appended: List<JsonObject> = emptyList(), disposed: List<Int> = emptyList()) =
        buildJsonObject {
            put("transaction_id", id); put("reserved", JsonArray(reserved)); put("appended", JsonArray(appended))
            put("disposed", JsonArray(disposed.map { JsonPrimitive(hex(it)) }))
        }

    @Test fun onlyOwnersThatStillReserveAndHaveNotDisposedOrAppendedAreOwed() {
        val append = ContinuousQueueOwnership.Appended(pin(3), setOf("live", "gone", "unreserved", "disposed", "done"), video = true)
        val ledger = listOf(
            tx("live", reserved = listOf(pin(3))),
            // "gone" was authorized at load time and no longer exists; this
            // used to throw and abort the whole pass every second.
            tx("unreserved", reserved = listOf(pin(4))),
            // A seek forward disposed the artifact before its append was
            // reported; the owner refuses the fact on every attempt.
            tx("disposed", reserved = listOf(pin(3)), disposed = listOf(3)),
            tx("done", reserved = listOf(pin(3)), appended = listOf(pin(3))),
        )
        val owed = ContinuousAppendFacts.owed(append, ledger)
        assertEquals(listOf("live"), owed.map { it.transaction.text("transaction_id") })
        assertEquals(pin(3), owed.single().interval)
    }

    @Test fun anAppendNoOwnerCanAcceptIsOwedNothingAndAudioIsNeverReported() {
        val stale = ContinuousQueueOwnership.Appended(pin(5), setOf("gone"), video = true)
        assertTrue(ContinuousAppendFacts.owed(stale, listOf(tx("other", reserved = listOf(pin(5))))).isEmpty())
        val audio = ContinuousQueueOwnership.Appended(pin(5), setOf("live"), video = false)
        assertTrue(ContinuousAppendFacts.owed(audio, listOf(tx("live", reserved = listOf(pin(5))))).isEmpty())
    }

    @Test fun aFailedStageDoesNotStopLaterStagesAndItsFailureIsReportedOnce() = runBlocking {
        val stages = ContinuousFlushStages()
        val ran = mutableListOf<String>()
        stages.stage { ran += "appended"; throw java.io.IOException("refused") }
        stages.stage { ran += "credited"; throw IllegalStateException("second") }
        stages.stage { ran += "retire" }
        assertEquals(listOf("appended", "credited", "retire"), ran)
        val failure = runCatching { stages.finish() }.exceptionOrNull()
        assertEquals("refused", failure?.message)
        stages.finish() // Reported once; the next pass starts clean.
    }

    @Test fun cancellationIsNeverAbsorbedByAStage() = runBlocking {
        val stages = ContinuousFlushStages()
        val result = runCatching { stages.stage { throw CancellationException("stop") } }
        assertTrue(result.exceptionOrNull() is CancellationException)
    }
}
