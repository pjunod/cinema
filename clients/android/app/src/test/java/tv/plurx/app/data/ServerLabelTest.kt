package tv.plurx.app.data

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ServerLabelTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun theServerNodeCarriesTheMachineNameAndTheServersLabel() {
        val node = json.decodeFromString<ServerNode>("""{"hostname":"lab6","display_name":"lab6"}""")
        assertEquals("lab6", node.display_name)
        assertEquals("lab6", serverSettingsLabel(node.display_name, node.hostname))
    }

    @Test
    fun aNodeWithoutAMachineNameShowsTheLabelAlone() {
        val node = json.decodeFromString<ServerNode>("""{"display_name":"plurx"}""")
        assertNull(node.hostname)
        assertEquals("plurx", serverSettingsLabel(node.display_name, node.hostname))
    }

    @Test
    fun aChosenNameKeepsTheAnsweringNodeVisible() {
        assertEquals("Living Room · lab6", serverSettingsLabel("Living Room", "lab6"))
        assertEquals("lab6", serverSettingsLabel(null, "lab6"))
        assertEquals("Living Room", serverSettingsLabel("Living Room", null))
    }
}
