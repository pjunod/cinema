package tv.plurx.app.player

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.double
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

class AutoQualityDefaultsAssetTest {
    @Test fun adapterConsumesTheSharedPolicyArtifactRatherThanPrivateDefaults() {
        val source = javaClass.classLoader!!.getResource("auto-quality-policy.json")!!.readText()
        val shared = Json.parseToJsonElement(source).jsonObject.getValue("defaults").jsonObject
        val actual = autoQualityDefaults(source)
        assertEquals(shared.getValue("sampleMs").jsonPrimitive.double, actual.sampleMs)
        assertEquals(shared.getValue("causeMaxAgeMs").jsonPrimitive.double, actual.causeMaxAgeMs)
        assertEquals(shared.getValue("cooldownMs").jsonPrimitive.double, actual.cooldownMs)
        assertEquals(shared.getValue("dwellMs").jsonPrimitive.double, actual.dwellMs)
        assertEquals(shared.getValue("restartCostSeconds").jsonPrimitive.double, actual.restartCostSeconds)
        assertEquals(shared.getValue("upgradeHeadroom").jsonPrimitive.double, actual.upgradeHeadroom)
        assertEquals(shared.getValue("recentSampleMaxAgeMs").jsonPrimitive.double, actual.recentSampleMaxAgeMs)
    }
}
