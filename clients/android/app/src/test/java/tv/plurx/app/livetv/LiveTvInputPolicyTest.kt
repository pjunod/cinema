package tv.plurx.app.livetv

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * The `live` section of the shared input contract. Kotlin cannot read the
 * fixture at runtime, so `LiveTvInputPolicy` transcribes it — and this is the
 * half that keeps the transcription honest.
 */
class LiveTvInputPolicyTest {
    private val live = checkNotNull(
        javaClass.classLoader?.getResource("player-input-contract.json"),
    ) { "tests/playback/player-input-contract.json is not on the JVM test classpath" }
        .readText()
        .let { Json.parseToJsonElement(it).jsonObject }
        .getValue("live").jsonObject

    @Test
    fun everySharedLiveRoutingCellMatches() {
        val routing = live.getValue("routing").jsonObject
        LiveTvInputSurface.entries.forEach { surface ->
            val states = routing.getValue(surface.contractName).jsonObject
            LiveTvInputState.entries.forEach { state ->
                val inputs = states.getValue(state.contractName).jsonObject
                LiveTvContractInput.entries.forEach { input ->
                    assertEquals(
                        "${surface.contractName}/${state.contractName}/${input.contractName}",
                        inputs.getValue(input.contractName).jsonPrimitive.content,
                        LiveTvInputPolicy.route(surface, state, input).contractName,
                    )
                }
            }
        }
    }

    @Test
    fun theEnumsNameTheSameSetTheFixtureDoes() {
        assertEquals(
            live.getValue("inputs").jsonArray.map { it.jsonPrimitive.content }.toSet(),
            LiveTvContractInput.entries.map { it.contractName }.toSet(),
        )
        // Android transcribes two of the fixture's three surfaces; `desktop` is
        // the web's. Every surface this client claims must exist in the
        // fixture, which is the direction that matters.
        val surfaces = live.getValue("routing").jsonObject.keys
        LiveTvInputSurface.entries.forEach {
            assertEquals("$it is in the fixture", true, surfaces.contains(it.contractName))
        }
    }

    @Test
    fun theRulingsOf20260902SurviveOnTheLiveSurface() {
        // A direction on a hidden overlay only reveals it — it never changes
        // channel behind a picture nobody can see.
        listOf(
            LiveTvContractInput.Left, LiveTvContractInput.Right,
            LiveTvContractInput.Up, LiveTvContractInput.Down,
        ).forEach {
            assertEquals(
                LiveTvInputOutcome.Reveal,
                LiveTvInputPolicy.route(LiveTvInputSurface.TenFoot, LiveTvInputState.Hidden, it),
            )
        }
        // The channel list is preview-then-commit.
        assertEquals(
            LiveTvInputOutcome.FocusRow,
            LiveTvInputPolicy.route(LiveTvInputSurface.TenFoot, LiveTvInputState.Overlay, LiveTvContractInput.Up),
        )
        assertEquals(
            LiveTvInputOutcome.Activate,
            LiveTvInputPolicy.route(LiveTvInputSurface.TenFoot, LiveTvInputState.Overlay, LiveTvContractInput.Select),
        )
        // Nothing on this surface seeks: the outcomes do not exist.
        assertFalse(LiveTvInputOutcome.entries.any { it.contractName in setOf("skip", "preview", "commit") })
    }

    @Test
    fun theTimingsAreTheFixturesTimings() {
        val timings = live.getValue("timings").jsonObject
        assertEquals(timings.getValue("hide_after_ms").jsonPrimitive.int.toLong(), LiveTvInputPolicy.HIDE_AFTER_MS)
        assertEquals(
            timings.getValue("channel_coalesce_ms").jsonPrimitive.int.toLong(),
            LiveTvInputPolicy.CHANNEL_COALESCE_MS,
        )
    }
}
