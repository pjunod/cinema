package tv.plurx.app.player

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The playback surface contract, run against the Android presenter.
 *
 * `tests/playback/playback-surface-contract.json` is one table of fault
 * classes and sources plus 54 ORDERED EVENT SEQUENCES, and every client runs
 * the same sequences against its own presenter. No row is skipped: an event
 * Android cannot express literally is MAPPED onto the Android equivalent, with
 * the mapping named beside it in [translate].
 *
 * The table assertions are the other half. The Kotlin `SurfaceClass` and
 * `SURFACE_SOURCES` tables are the fixture transcribed, so a drift between them
 * fails here rather than becoming a third client that quietly disagrees with
 * the web and Apple.
 */
class PlaybackSurfaceReducerTest {

    private val contract: JsonObject = checkNotNull(
        javaClass.classLoader?.getResource("playback-surface-contract.json"),
    ) { "tests/playback/playback-surface-contract.json is not on the JVM test classpath" }
        .readText()
        .let { Json.parseToJsonElement(it).jsonObject }

    // ------------------------------------------------------ the table itself

    @Test
    fun classesAreTheFixtureVerbatim() {
        val classes = contract.getValue("classes").jsonObject
        assertEquals(
            "the Kotlin class table names different classes than the fixture",
            classes.keys.sorted(),
            SurfaceClass.entries.map { it.wire }.sorted(),
        )
        for ((wire, raw) in classes) {
            val row = raw.jsonObject
            val cls = checkNotNull(SurfaceClass.ofWire(wire))
            assertEquals(wire, row.getValue("severity").jsonPrimitive.content, cls.severity.wire)
            assertEquals(wire, blockingOf(row["blocking"]), cls.blocking)
            assertEquals(
                wire,
                row.getValue("retired_by").jsonArray.map { it.jsonPrimitive.content }.sorted(),
                cls.retiredBy.map { it.wire }.sorted(),
            )
            assertEquals(wire, longOrNull(row["min_ms"]), cls.minMs)
            assertEquals(wire, longOrNull(row["timed_ms"]), cls.timedMs)
            assertEquals(
                wire,
                row["timer_paused_while_actions"]?.jsonPrimitive?.boolean ?: false,
                cls.timerPausedWhileActions,
            )
            assertEquals(
                wire,
                row["requires_player_stopped"]?.jsonPrimitive?.boolean ?: false,
                cls.requiresPlayerStopped,
            )
            assertEquals(wire, stringOrNull(row["title"]), cls.title)
            assertEquals(
                wire,
                row["default_actions"]?.jsonArray?.map { it.jsonPrimitive.content } ?: emptyList<String>(),
                cls.defaultActions.map { it.wire },
            )
            // A class that retires on continuous presentation must name the
            // bound it is measured against, and it must be the fixture's.
            // (`hold` moved from `presenting` to `presenting_after_raise`
            // upstream in 881cf82f; the retired_by comparison above is what
            // catches a client that did not port it.)
            if (cls.retiredBy.contains(SurfaceRetirement.PresentingContinuousMs)) {
                val timings = contract.getValue("timings").jsonObject
                val expected = when (cls) {
                    SurfaceClass.Refused -> timings.getValue("refused_progress_ms").jsonPrimitive.long
                    else -> timings.getValue("disagreement_notice_ms").jsonPrimitive.long
                }
                assertEquals(wire, expected, cls.continuousMs)
            }
        }
    }

    @Test
    fun sourcesAreTheFixtureVerbatimAndInOrder() {
        val rows = contract.getValue("sources").jsonArray.map { it.jsonObject }
        assertEquals(
            "the Kotlin source table is a different length than the fixture",
            rows.size,
            SURFACE_SOURCES.size,
        )
        rows.forEachIndexed { index, raw ->
            val row = SURFACE_SOURCES[index]
            val id = raw.getValue("id").jsonPrimitive.content
            assertEquals("source $index", id, row.id)
            assertEquals(id, raw.getValue("context").jsonPrimitive.content, row.contextWire)
            assertEquals(id, stringOrNull(raw["class"]), row.cls?.wire)
            assertEquals(
                id,
                raw["requires"]?.jsonObject?.get("player_stopped")?.jsonPrimitive?.boolean ?: false,
                row.requiresPlayerStopped,
            )
            assertEquals(
                id,
                raw["actions"]?.jsonArray?.map { it.jsonPrimitive.content },
                row.actions?.map { it.wire },
            )
            assertEquals(
                id,
                raw["codes"]?.jsonArray?.map { it.jsonPrimitive.content } ?: emptyList<String>(),
                row.codes,
            )
            assertEquals(id, stringOrNull(raw["then_when_stopped"]), row.thenWhenStopped?.wire)
        }
    }

    @Test
    fun timingsAndRanksAreTheFixtureVerbatim() {
        val timings = contract.getValue("timings").jsonObject
        assertEquals(timings.getValue("buffering_min_ms").jsonPrimitive.long, SurfaceTimings.BUFFERING_MIN_MS)
        assertEquals(timings.getValue("hold_notice_ms").jsonPrimitive.long, SurfaceTimings.HOLD_NOTICE_MS)
        assertEquals(timings.getValue("degraded_notice_ms").jsonPrimitive.long, SurfaceTimings.DEGRADED_NOTICE_MS)
        assertEquals(timings.getValue("refused_progress_ms").jsonPrimitive.long, SurfaceTimings.REFUSED_PROGRESS_MS)
        assertEquals(
            timings.getValue("disagreement_notice_ms").jsonPrimitive.long,
            SurfaceTimings.DISAGREEMENT_NOTICE_MS,
        )
        val ranks = contract.getValue("severity_rank").jsonObject
        for (severity in SurfaceSeverity.entries) {
            assertEquals(
                severity.wire,
                ranks.getValue(severity.wire).jsonPrimitive.long,
                severity.rank.toLong(),
            )
        }
        assertEquals(
            "the action vocabulary drifted from the fixture",
            contract.getValue("actions").jsonArray.map { it.jsonPrimitive.content }.sorted(),
            SurfaceAction.entries.map { it.wire }.sorted(),
        )
        assertEquals(
            "the input contract's failed classes drifted from the fixture",
            contract.getValue("input_failed_state").jsonObject
                .getValue("classes").jsonArray.map { it.jsonPrimitive.content }.sorted(),
            SurfaceClass.entries
                .filter { it.blocking == SurfaceBlocking.Always }
                .map { it.wire }
                .sorted(),
        )
    }

    // ------------------------------------------------------------- the cases

    @Test
    fun everyFixtureCaseRuns() {
        val cases = contract.getValue("cases").jsonArray.map { it.jsonObject }
        assertTrue("the fixture has no cases", cases.isNotEmpty())
        for (case in cases) check(case)
        // Nothing is skipped: the count is asserted so a mapping that silently
        // dropped a row would fail rather than pass with fewer cases.
        assertEquals(cases.size, cases.count { it.containsKey("events") })
    }

    @Test
    fun aBlockingSurfaceIsNeverDrawnOverAPlayerNobodyStopped() {
        // The whole point, asserted independently of any one case: replay every
        // sequence and fail if a blocking class is ever drawn from a fault that
        // did not carry the owner's stop.
        for (case in contract.getValue("cases").jsonArray.map { it.jsonObject }) {
            val reducer = PlaybackSurfaceReducer()
            var state = SurfaceState()
            for (raw in case.getValue("events").jsonArray.map { it.jsonObject }) {
                val step = reducer.apply(state, translate(raw), raw.getValue("t").jsonPrimitive.long)
                state = step.state
                val fault = (step.surface as? PlaybackSurface.Blocking)?.fault ?: continue
                if (fault.cls.blocking != SurfaceBlocking.Always) continue
                assertTrue(
                    "${case.getValue("name").jsonPrimitive.content}: ${fault.cls.wire} covered the " +
                        "picture without the owner stopping the player",
                    fault.playerStopped,
                )
            }
        }
    }

    @Test
    fun replayingACaseTwiceGivesTheSameAnswer() {
        for (case in contract.getValue("cases").jsonArray.map { it.jsonObject }) {
            assertEquals(
                "${case.getValue("name").jsonPrimitive.content} is not deterministic",
                trace(case),
                trace(case),
            )
        }
    }

    // ---------------------------------------------------------------- runner

    private class Slot {
        val log = mutableListOf<SurfaceLog>()
        var surface: PlaybackSurface = PlaybackSurface.None
    }

    private fun run(case: JsonObject): Map<Long, Slot> {
        val reducer = PlaybackSurfaceReducer()
        var state = SurfaceState()
        val byTime = LinkedHashMap<Long, Slot>()
        for (raw in case.getValue("events").jsonArray.map { it.jsonObject }) {
            val at = raw.getValue("t").jsonPrimitive.long
            val step = reducer.apply(state, translate(raw), at)
            state = step.state
            val slot = byTime.getOrPut(at) { Slot() }
            slot.log += step.log
            slot.surface = step.surface
        }
        return byTime
    }

    private fun trace(case: JsonObject): String =
        run(case).entries.joinToString("|") { (at, slot) ->
            "$at:${kindOf(slot.surface)}/${slot.surface.fault?.cls?.wire}"
        }

    private fun check(case: JsonObject) {
        val name = case.getValue("name").jsonPrimitive.content
        val timeline = run(case)
        for (raw in case.getValue("expect").jsonArray.map { it.jsonObject }) {
            val at = raw.getValue("at").jsonPrimitive.long
            val slot = checkNotNull(timeline[at]) { "$name: nothing happened at t=$at" }
            val where = "$name @$at"
            val surface = slot.surface
            val fault = surface.fault
            raw["surface"]?.let {
                assertEquals("$where: surface kind", it.jsonPrimitive.content, kindOf(surface))
            }
            raw["class"]?.let {
                assertEquals("$where: fault class", stringOrNull(it), fault?.cls?.wire)
            }
            raw["source"]?.let {
                assertEquals("$where: fault source", stringOrNull(it), fault?.source)
            }
            raw["actions"]?.let {
                assertEquals(
                    "$where: actions",
                    it.jsonArray.map { action -> action.jsonPrimitive.content },
                    fault?.actions?.map { action -> action.wire } ?: emptyList<String>(),
                )
            }
            raw["title"]?.let { assertEquals("$where: title", stringOrNull(it), fault?.title) }
            raw["detail"]?.let { assertEquals("$where: detail", stringOrNull(it), fault?.detail) }
            if (raw.containsKey("position_ms")) {
                assertEquals("$where: position_ms", longOrNull(raw["position_ms"]), fault?.positionMs)
            }
            raw["input_failed"]?.let {
                assertEquals(
                    "$where: input contract failed state",
                    it.jsonPrimitive.boolean,
                    surface.entersFailedRouting,
                )
            }
            raw["error"]?.let {
                val expected = it.jsonPrimitive.content
                val seen = slot.log.mapNotNull { entry -> entry.error }
                assertTrue("$where: expected fixture error $expected, got $seen", seen.contains(expected))
            }
            raw["log"]?.let {
                assertEquals(
                    "$where: log",
                    it.jsonArray.map { entry -> entry.jsonPrimitive.content },
                    slot.log.map { entry -> entry.event },
                )
            }
        }
    }

    // ----------------------------------------------------------- translation

    /**
     * One fixture event as an Android [SurfaceEvent].
     *
     * Two rows in the fixture's vocabulary have no literal Android spelling and
     * are MAPPED here rather than skipped:
     *
     *  - `hidden` is a browser page becoming hidden. Android's equivalent is
     *    `presentationForeground` going false, which `Controller` feeds through
     *    this same event — so the fixture's `hidden: true` is the app leaving
     *    the foreground, and `hidden: false` is it coming back.
     *  - `event: canplay | playing | isPlayingChanged | timeControlStatus` are
     *    four platform callbacks, of which Media3 has only `isPlayingChanged`.
     *    All four are the same thing to the presenter — a state notification
     *    that is not proof a picture is moving — so all four map onto
     *    [SurfaceEvent.Inert], which is what `Controller.onIsPlayingChanged`
     *    raises.
     *
     * Generations and intents are fixture strings ("g1", "i7"); Android's are
     * `mediaMutationEpoch` and a `PlaybackIntent` sequence, both `Long`. The
     * trailing digits are the mapping and it is one-to-one.
     */
    private fun translate(event: JsonObject): SurfaceEvent = when {
        event.containsKey("attach") -> SurfaceEvent.Attach(id(event.getValue("attach")))
        event.containsKey("retire") -> SurfaceEvent.Retire(id(event.getValue("retire")))
        event.containsKey("event") -> SurfaceEvent.Inert(event.getValue("event").jsonPrimitive.content)
        event.containsKey("presenting") -> SurfaceEvent.Presenting(
            presenting = event.getValue("presenting").jsonPrimitive.boolean,
            attached = event["attached"]?.let { id(it) },
        )
        event.containsKey("raise") -> SurfaceEvent.Raise(
            source = event.getValue("raise").jsonPrimitive.content,
            context = contextOf(event.getValue("context").jsonPrimitive.content),
            attached = id(event.getValue("attached")),
            intent = event["intent"]?.let { id(it) },
            playerStopped = event["player_stopped"]?.jsonPrimitive?.boolean ?: false,
            actions = event["actions"]?.jsonArray?.map { actionOf(it.jsonPrimitive.content) },
            positionMs = longOrNull(event["position_ms"]),
            title = stringOrNull(event["title"]),
            detail = stringOrNull(event["detail"]),
        )
        event.containsKey("intent_settled") ->
            SurfaceEvent.IntentSettled(id(event.getValue("intent_settled")))
        event.containsKey("intent_superseded") ->
            SurfaceEvent.IntentSuperseded(id(event.getValue("intent_superseded")))
        event.containsKey("owner_success") ->
            SurfaceEvent.OwnerSuccess(id(event.getValue("owner_success")))
        event.containsKey("user_action") ->
            SurfaceEvent.UserAction(actionOf(event.getValue("user_action").jsonPrimitive.content))
        event.containsKey("hidden") -> SurfaceEvent.Hidden(event.getValue("hidden").jsonPrimitive.boolean)
        event.containsKey("tick") -> SurfaceEvent.Tick
        else -> throw AssertionError("unmapped fixture event: $event")
    }

    private fun id(element: JsonElement): Long =
        element.jsonPrimitive.content.dropWhile { !it.isDigit() }.toLong()

    private fun contextOf(wire: String): SurfaceContext =
        checkNotNull(SurfaceContext.entries.firstOrNull { it.wire == wire }) { "unknown context $wire" }

    private fun actionOf(wire: String): SurfaceAction =
        checkNotNull(SurfaceAction.entries.firstOrNull { it.wire == wire }) { "unknown action $wire" }

    private fun kindOf(surface: PlaybackSurface): String = when (surface) {
        is PlaybackSurface.None -> "none"
        is PlaybackSurface.Indicator -> "indicator"
        is PlaybackSurface.Banner -> "banner"
        is PlaybackSurface.Blocking -> "blocking"
    }

    private fun blockingOf(element: JsonElement?): SurfaceBlocking {
        if (element == null || element is JsonNull) return SurfaceBlocking.Never
        val primitive = element.jsonPrimitive
        return when {
            primitive.isString -> SurfaceBlocking.WhileNotPresenting
            primitive.boolean -> SurfaceBlocking.Always
            else -> SurfaceBlocking.Never
        }
    }

    private fun stringOrNull(element: JsonElement?): String? {
        if (element == null || element is JsonNull) return null
        return element.jsonPrimitive.content
    }

    private fun longOrNull(element: JsonElement?): Long? {
        if (element == null || element is JsonNull) return null
        return element.jsonPrimitive.long
    }

    @Test
    fun aNullSurfaceHasNoFaultAndEntersNoFailedRouting() {
        assertNull(PlaybackSurface.None.fault)
        assertFalse(PlaybackSurface.None.entersFailedRouting)
    }
}
