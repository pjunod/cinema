package tv.plurx.app.data

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

class ServerLabelTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun theServerLabelIsTheServersDisplayNameNotTheSharedDefault() {
        val info = json.decodeFromString<Server>("""{"name":"plurx","hostname":"lab6","display_name":"lab6"}""")
        assertEquals("lab6", info.label)
        assertEquals("lab6", serverSettingsLabel(info.label, info.hostname))
    }

    @Test
    fun anOlderServerWithoutADisplayNameStillShowsItsName() {
        val info = json.decodeFromString<Server>("""{"name":"plurx"}""")
        assertEquals("plurx", info.label)
        assertEquals("plurx", serverSettingsLabel(info.label, info.hostname))
    }

    @Test
    fun aChosenNameKeepsTheAnsweringNodeVisible() {
        assertEquals("Living Room · lab6", serverSettingsLabel("Living Room", "lab6"))
        assertEquals("lab6", serverSettingsLabel(null, "lab6"))
        assertEquals("Living Room", serverSettingsLabel("Living Room", null))
    }
}
