package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PlayerInputPolicyTest {
    private val fixture = checkNotNull(
        javaClass.classLoader?.getResource("player-input-contract.json"),
    ) { "tests/playback/player-input-contract.json is not on the JVM test classpath" }
        .readText()
        .let { Json.parseToJsonElement(it).jsonObject }

    @Test
    fun everySharedRoutingCellMatches() {
        val routing = fixture.getValue("routing").jsonObject
        PlayerInputSurface.entries.forEach { surface ->
            val states = routing.getValue(surface.contractName).jsonObject
            PlayerInputState.entries.forEach { state ->
                val inputs = states.getValue(state.contractName).jsonObject
                PlayerContractInput.entries.forEach { input ->
                    assertEquals(
                        "${surface.contractName}/${state.contractName}/${input.contractName}",
                        inputs.getValue(input.contractName).jsonPrimitive.content,
                        PlayerInputPolicy.route(surface, state, input).contractName,
                    )
                }
            }
        }
    }

    @Test
    fun theCloseControlLeavesThePlayerFromEveryState() {
        // The phone's back arrow is `close`, not the BACK key: routed through
        // `Back` it answered `Hide` in `Transport`, the state it is tapped
        // from. Every row of `close_control` ends in `Exit`.
        val close = fixture.getValue("close_control").jsonObject
        assertEquals(
            PlayerInputState.entries.map { it.contractName }.toSet(),
            close.keys.filter { it != "notes" }.toSet(),
        )
        PlayerInputState.entries.forEach { state ->
            val steps = PlayerInputPolicy.closeSteps(state)
            assertEquals(
                state.contractName,
                close.getValue(state.contractName).jsonArray.map { it.jsonPrimitive.content },
                steps.map { it.contractName },
            )
            assertEquals(state.contractName, PlayerInputOutcome.Exit, steps.last())
            assertFalse("${state.contractName} hides — that is the defect", PlayerInputOutcome.Hide in steps)
        }
    }

    @Test
    fun theCloseArrowWalksTheCloseControlAndNeverTheBackKey() {
        // The call site, not just the table. Gradle runs unit tests from the
        // module directory; walk up in case a runner does not.
        val module = generateSequence(java.io.File("").absoluteFile) { it.parentFile }
            .first { java.io.File(it, "src/main/java/tv/plurx/app/player/PlayerScreen.kt").isFile }
        for (screen in listOf("PlayerScreen.kt", "OfflinePlayerScreen.kt")) {
            val source = java.io.File(module, "src/main/java/tv/plurx/app/player/$screen").readText()
            val start = source.indexOf("onClose = {")
            assertTrue("$screen has no onClose call site", start >= 0)
            val body = source.substring(start, source.indexOf("onPlayPause", start))
            assertTrue("$screen: the arrow must walk closeSteps", "PlayerInputPolicy.closeSteps(inputState())" in body)
            assertFalse("$screen: the arrow must not be the BACK key", "PlayerContractInput.Back" in body)
            assertFalse("$screen: the arrow must not be routed as a key", "PlayerInputPolicy.route(" in body)
        }
    }

    @Test
    fun theContractsOwnNumbersAreTheOnesTheClientUses() {
        // `hide_after_ms` and `skip_seconds` were prose in JSON clothing: no
        // client read them, and the web's, Apple's and Android's constants
        // happened to agree. The web reads them from the generated embed and
        // Apple asserts them; this is Android's half.
        assertEquals(
            fixture.getValue("timings").jsonObject
                .getValue("hide_after_ms").jsonPrimitive.long,
            PlayerInputPolicy.HIDE_AFTER_MS,
        )
        assertEquals(
            fixture.getValue("steps").jsonObject
                .getValue("skip_seconds").jsonPrimitive.long * 1_000L,
            PlayerInputPolicy.SKIP_STEP_MS,
        )
    }

    @Test
    fun previewAccelerationMatchesTheSharedLadder() {
        val ladder = fixture.getValue("steps").jsonObject
            .getValue("preview_acceleration").jsonArray
            .map { row -> row.jsonObject }
        for (repeatCount in 0..14) {
            val expected = ladder
                .last { it.getValue("from_repeat").jsonPrimitive.content.toInt() <= repeatCount }
                .getValue("step_seconds").jsonPrimitive.content.toLong() * 1_000
            assertEquals(expected, PlayerInputPolicy.previewStepMs(repeatCount))
        }
    }
}
