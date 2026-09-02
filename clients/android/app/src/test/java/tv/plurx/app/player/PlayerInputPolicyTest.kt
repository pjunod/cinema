package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
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
