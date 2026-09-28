package tv.plurx.app.data

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class OpticalModelsTest {
    @Test
    fun opticalProgressEncodesTheExactBurnedRendition() {
        val progress = OpticalProgressRequest(
            drive_id = "node-a:drive-a",
            media_generation = "generation-a",
            session_id = "session-a",
            angle = 1,
            position_ms = 12_345,
            duration_ms = 90_000,
            audio = 7,
            subtitle = OpticalProgressSubtitleSelection(index = 5, burned = true),
            recorded_at_ms = 1_800_000_000_000,
        )
        val document = Json.parseToJsonElement(Json.encodeToString(progress)).jsonObject

        assertEquals("node-a:drive-a", document.getValue("drive_id").jsonPrimitive.content)
        assertEquals("generation-a", document.getValue("media_generation").jsonPrimitive.content)
        assertEquals(7L, document.getValue("audio").jsonPrimitive.content.toLong())
        val subtitle = document.getValue("subtitle").jsonObject
        assertEquals(5L, subtitle.getValue("index").jsonPrimitive.content.toLong())
        assertTrue(subtitle.getValue("burned").jsonPrimitive.content.toBoolean())
    }
}
