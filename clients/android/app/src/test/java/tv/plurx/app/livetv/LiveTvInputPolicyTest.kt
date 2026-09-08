package tv.plurx.app.livetv

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
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

/**
 * The routing table is only worth transcribing if something calls it. Nothing
 * did: `LiveTvInputPolicy.route` had one call site in the whole app and it was
 * this test file. These pin the wiring in the screen that turns the table into
 * behaviour — the source is read rather than composed, because a Compose
 * screen needs an instrumented device and these run on the JVM.
 */
class LiveTvScreenWiringTest {
    private val screen = java.io.File(
        "src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt",
    ).readText()

    @Test
    fun `every press goes through the shared table`() {
        assertTrue(screen.contains("onPreviewKeyEvent"))
        assertTrue(screen.contains("LiveTvInputPolicy.route(surface, inputState(), input)"))
        // And a hidden overlay is `Hidden`, which is the state whose direction
        // rows say `reveal` and nothing else.
        assertTrue(screen.contains("else -> LiveTvInputState.Hidden"))
    }

    @Test
    fun `the overlay hides itself after the contract's four seconds`() {
        assertTrue(screen.contains("delay(LiveTvInputPolicy.HIDE_AFTER_MS)"))
        assertTrue(screen.contains("overlayVisible = false"))
    }

    @Test
    fun `a television is fullscreen without anyone pressing a button`() {
        assertTrue(screen.contains("if (television) fullscreen = state.playing"))
    }

    @Test
    fun `a refused picture-in-picture never latches the retention flag`() {
        // Setting `retain` before the call and discarding the Boolean disabled
        // every lifecycle release path for the life of the process whenever the
        // system refused PiP — a keepalive loop holding the household's only
        // tuner against a screen nobody was on.
        assertTrue(screen.contains("val entered = target.enterPictureInPictureMode("))
        assertTrue(screen.contains("controller.setRetained(entered)"))
        assertFalse(screen.contains("controller.setRetained(true)\n        target.enterPictureInPictureMode"))
    }

    @Test
    fun `leaving picture-in-picture brings the chrome back`() {
        assertTrue(screen.contains("overlayVisible = !info.isInPictureInPictureMode"))
    }

    @Test
    fun `a channel key routes through the coalescer rather than straight to a tuner`() {
        assertTrue(screen.contains("controller.requestChannel(next)"))
    }
}
