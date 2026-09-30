package tv.plurx.app.player

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class AutoQualityPolicyTest {
    private fun fixture(): JsonObject {
        val text = checkNotNull(javaClass.classLoader?.getResource("auto-quality-policy.json")) {
                "tests/playback/auto-quality-policy.json is not on the JVM test classpath"
            }.readText()
        return Json.parseToJsonElement(text).jsonObject
    }
    private fun JsonObject.number(key: String): Double? = get(key)?.jsonPrimitive?.doubleOrNull
    private fun JsonObject.flag(key: String): Boolean = get(key)?.jsonPrimitive?.booleanOrNull ?: false
    private fun JsonObject.text(key: String): String? = get(key)?.jsonPrimitive?.contentOrNull

    private fun defaults(d: JsonObject): AutoQualityPolicy.Defaults {
        fun n(key: String) = checkNotNull(d.number(key)) { "missing numeric default: $key" }
        return AutoQualityPolicy.Defaults(n("sampleMs"), n("decisionMs"), n("safeEstimateFactor"),
            n("severeEstimateRatio"), n("lowRungEmergencyCount"), n("severePeakSafetyFactor"),
            n("mildHeadroom"), n("mildSamples"), n("cooldownMs"), n("upgradeHeadroom"),
            n("upgradeHoldMs"), n("upgradeRunwaySeconds"), n("upgradeAfterCliffMs"),
            n("upgradeSpeedFloor"), n("stallWindowMs"), n("dwellMs"), n("nearEmptyRunwaySeconds"),
            n("restartCostSeconds"), n("causeMaxAgeMs"), n("recentSampleMaxAgeMs"))
    }
    private fun sample(s: JsonObject): AutoQualityPolicy.Sample {
        val cause = s["causeEvidence"]?.takeUnless { it == JsonNull }?.jsonObject
        // Match Number(null)=0, Number(undefined)=NaN for the web age field.
        val age = if (cause?.get("ageMs") == JsonNull) 0.0 else cause?.number("ageMs")
        return AutoQualityPolicy.Sample(currentHeight = checkNotNull(s.number("currentHeight")),
            estimateKbps = s.number("estimateKbps"), recentEstimateKbps = s.number("recentEstimateKbps"),
            recentEstimateAtMs = s.number("recentEstimateAtMs"), recentMediaDeliveryKbps = s.number("recentMediaDeliveryKbps"),
            runwaySeconds = s.number("runwaySeconds"), previousRunwaySeconds = s.number("previousRunwaySeconds"),
            recentSpeed = s.number("recentSpeed"), activeSupplyStall = s.flag("activeSupplyStall"),
            supplyStalls = s.number("supplyStalls") ?: 0.0, decodeStalls = s.number("decodeStalls") ?: 0.0,
            decodeStepConsumed = s.flag("decodeStepConsumed"), lastStallAtMs = s.number("lastStallAtMs"),
            lastSwitchAtMs = s.number("lastSwitchAtMs"), lastCliffAtMs = s.number("lastCliffAtMs"),
            nowMs = s.number("nowMs") ?: 0.0, mildSamples = s.number("mildSamples") ?: 0.0,
            upgradeSinceMs = s.number("upgradeSinceMs"), playerHeight = s.number("playerHeight"),
            blockedHeights = s["blockedHeights"]?.takeUnless { it == JsonNull }?.jsonArray
                ?.map { it.jsonPrimitive.double }?.toSet() ?: emptySet(),
            causeEvidence = cause?.let { AutoQualityPolicy.Cause(it.text("kind"), age) })
    }
    private fun fields(d: AutoQualityPolicy.Decision): JsonObject = buildJsonObject {
        fun number(key: String, value: Double?) { put(key, value?.let(::JsonPrimitive) ?: JsonNull) }
        number("height", d.height)
        put("reason", d.reason?.let(::JsonPrimitive) ?: JsonNull)
        put("emergency", d.emergency)
        number("mildSamples", d.mildSamples)
        number("upgradeSinceMs", d.upgradeSinceMs)
        d.action?.let { put("action", it) }
        d.blockedHeights?.let { put("blockedHeights", JsonArray(it.map(::JsonPrimitive))) }
        d.evidence?.let { e -> put("evidence", buildJsonObject {
            put("kind", e.kind)
            put("age_ms", e.ageMs?.let(::JsonPrimitive) ?: JsonNull)
            put("runway_seconds", e.runwaySeconds?.let(::JsonPrimitive) ?: JsonNull)
            put("throughput_kbps", e.throughputKbps?.let(::JsonPrimitive) ?: JsonNull)
        }) }
    }
    private fun assertValue(name: String, expected: JsonElement, actual: JsonElement?) {
        // JSON distinguishes 720 from 720.0 textually; JS compares both as
        // numbers. Preserve null/string/bool and compare numeric values only.
        if (expected is JsonPrimitive && expected != JsonNull && !expected.isString && expected.doubleOrNull != null) {
            assertEquals(name, expected.double, checkNotNull(actual).jsonPrimitive.double, 0.0)
        } else if (expected is JsonArray) {
            assertEquals(name, expected.size, checkNotNull(actual).jsonArray.size)
            expected.forEachIndexed { i, value -> assertValue("$name[$i]", value, actual.jsonArray[i]) }
        } else assertEquals(name, expected, actual)
    }

    @Test
    fun sharedAutoQualityFixture() {
        val root = fixture()
        assertEquals(1, root.getValue("schema").jsonPrimitive.int)
        val rawDefaults = root.getValue("defaults").jsonObject
        val d = defaults(rawDefaults)
        // No private platform numbers. This file's values are pinned to the
        // browser's AUTO_DEFAULTS by the existing web equality test.
        assertEquals(AutoQualityPolicy.Defaults::class.java.declaredFields.filterNot {
            it.isSynthetic || java.lang.reflect.Modifier.isStatic(it.modifiers)
        }
            .map { it.name }.toSet(), rawDefaults.keys)
        assertFalse(rawDefaults.containsKey("switchBudgetPerHour"))
        assertEquals(6, root.getValue("proposed_defaults").jsonObject.getValue("switchBudgetPerHour").jsonPrimitive.int)
        val ladder = root.getValue("ladder").jsonArray.map { item -> item.jsonObject.let {
            AutoQualityPolicy.Rung(checkNotNull(it.number("height")), checkNotNull(it.number("total_kbps")), it.number("peak_kbps"))
        } }
        val base = root.getValue("sample_defaults").jsonObject
        val cases = root.getValue("cases").jsonArray
        assertTrue(cases.isNotEmpty())
        val names = mutableSetOf<String>()
        val keys = setOf("height", "reason", "emergency", "action", "evidence", "mildSamples", "upgradeSinceMs", "blockedHeights")
        cases.forEach { row ->
            val item = row.jsonObject
            val name = checkNotNull(item.text("name"))
            assertTrue("duplicate case: $name", names.add(name))
            val s = JsonObject(base + item.getValue("sample").jsonObject)
            val decision = fields(AutoQualityPolicy.decideRung(ladder, sample(s), d))
            val expected = item.getValue("expect").jsonObject
            val current = item["web_current"]?.jsonObject
            if (current != null) {
                assertNotEquals(name, expected, current)
                assertTrue("$name: a disagreement needs its finding", (item.text("finding")?.length ?: 0) >= 200)
            }
            (current ?: expected).forEach { (key, value) ->
                if (key !in keys) assertNotNull("$name: $key is not a decision field", current)
                else if (key == "evidence") value.jsonObject.forEach { (field, wanted) ->
                    assertValue("$name: evidence.$field", wanted, decision[key]?.jsonObject?.get(field))
                } else assertValue("$name: $key", value, decision[key])
            }
        }
        root.getValue("causes").jsonArray.forEach { cause ->
            assertTrue("missing cause: $cause", names.any { it.startsWith("${cause.jsonPrimitive.content}: ") })
        }
        // Metadata coverage is not a native controller guard implementation.
        val states = root.getValue("controller_gates").jsonArray.mapNotNull { it.jsonObject.text("viewer_state") }
        listOf("Original", "Manual rung", "Paused", "Background / PiP", "HDR fidelity", "A stall-scoped control verdict")
            .forEach { assertTrue("missing viewer state: $it", it in states) }
    }
}
