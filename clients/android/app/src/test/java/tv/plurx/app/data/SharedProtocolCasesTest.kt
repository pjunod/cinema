package tv.plurx.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.player.ControlAction
import tv.plurx.app.player.ControlAnswer
import tv.plurx.app.player.PlaybackDemand
import tv.plurx.app.player.PreparedOfferWait
import tv.plurx.app.player.RenderState

/**
 * `tests/sharing/protocol-cases.json`, read row for row. The same rows drive the
 * Rust `sharing_protocol_fixture_*` tests and `tests/web/sharing-protocol-cases.test.js`;
 * a row's `layer` says who validates it, and `client`/`both` rows are this
 * client's. Every group a Kotlin path parses is asserted here through the
 * shipped decoder, not a copy of its rules.
 */
class SharedProtocolCasesTest {
    private val fixture: JsonObject = Json.parseToJsonElement(
        checkNotNull(javaClass.classLoader?.getResource("protocol-cases.json")) {
            "tests/sharing/protocol-cases.json is not on the JVM test classpath"
        }.readText(),
    ).jsonObject

    private fun JsonElement.string(key: String) = jsonObject.getValue(key).jsonPrimitive.content
    private fun clientRows(rows: JsonArray) = rows.map { it.jsonObject }.filter { it.string("layer") in setOf("client", "both") }
    private fun accepts(block: () -> Unit): Boolean = runCatching(block).isSuccess

    /** A copy of [base] with one row's change at its path; `{session}` names [session]. */
    private fun mutated(base: JsonObject, row: JsonObject, session: String): JsonObject {
        val path = row.getValue("path").jsonArray.map { it.jsonPrimitive.content }
        fun apply(node: JsonObject, depth: Int): JsonObject {
            val key = path[depth]
            if (depth < path.size - 1) return JsonObject(node + (key to apply(node.getValue(key).jsonObject, depth + 1)))
            if (row["op"]?.jsonPrimitive?.content == "remove") return JsonObject(node - key)
            val value = row.getValue("value").let { v ->
                if (v is JsonPrimitive && v.isString) JsonPrimitive(v.content.replace("{session}", session)) else v
            }
            return JsonObject(node + (key to value))
        }
        return apply(base, 0)
    }

    /** The fixture's Shared file context, minted only through the authenticated detail path. */
    private suspend fun base(): PlaybackFileContext {
        Session.origin = "https://b.test"; Session.token = "protocol-bearer"
        val c = fixture.getValue("context").jsonObject
        val reference = Json.decodeFromJsonElement<SharedPlaybackReference>(c.getValue("reference"))
        val lifecycle = c.getValue("lifecycle_generation").jsonPrimitive.long
        val detail = buildJsonObject {
            put("lifecycle_generation", lifecycle)
            put("files", buildJsonArray { add(buildJsonObject {
                put("file_id", c.string("file_id")); put("revision", c.string("revision")); put("file_base", c.string("file_base"))
                put("reference", buildJsonObject {
                    put("item", c.getValue("reference")); put("file_id", c.string("file_id")); put("revision", c.string("revision")); put("lifecycle_generation", lifecycle)
                })
            }) })
        }.toString()
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            okhttp3.Response.Builder().request(chain.request()).protocol(okhttp3.Protocol.HTTP_1_1).code(200).message("fixture")
                .body(detail.toResponseBody(null)).build()
        }.build()
        return PlaybackFileContext.authenticatedDetailForTest(reference, c.string("file_id"), transport)
    }

    private fun hlsRequest() = CreateSessionReq(playback_id = "player-fixture", request_id = "e0e0e0e0-e0e0-40e0-80e0-e0e0e0e0e0e0", presentation = "vod")
    private fun directRequest() = CreateSessionReq(playback_id = "player-fixture", request_id = "e0e0e0e0-e0e0-40e0-80e0-e0e0e0e0e0e0", presentation = "direct")

    @Test fun sourceIdsKeepTheExactDecimalWireGrammar() {
        val rows = fixture.getValue("cases").jsonArray
        assertTrue(rows.size >= 12)
        for (row in rows) {
            val id = row.jsonObject.getValue("input").string("source_id")
            assertEquals(row.string("id"), row.string("expected") == "accepted", PlaybackFileContext.canonicalId(id))
        }
    }

    @Test fun statusTokensAndTheBoundSharedStatusGrammar(): Unit = runBlocking {
        val tokens = fixture.getValue("status_tokens").jsonArray.map { it.jsonObject }
        for (row in tokens) assertEquals(row.string("id"), row.string("expected") == "accepted", SharedPlaybackStatus.isToken(row.string("token")))
        val status = fixture.getValue("shared_status").jsonObject
        val words = status.getValue("word_fields").jsonArray.map { it.jsonPrimitive.content }.toSet()
        assertEquals(words, SharedPlaybackStatus.requiredWords + SharedPlaybackStatus.optionalWords)
        val playback = SharedStart.decode(fixture.getValue("hls_start").jsonObject.getValue("public").toString()).bindInitial(base(), hlsRequest())
        val accepted = status.getValue("accepted").jsonObject
        fun decodes(wire: JsonObject) = accepts { SharedPlaybackStatus.decode(wire.toString().toByteArray(), playback) }
        assertTrue(decodes(accepted))
        assertEquals(accepted.string("session_id"), SharedPlaybackStatus.decode(accepted.toString().toByteArray(), playback).sessionId)
        for (field in words) for (row in tokens) {
            val reply = JsonObject(accepted + ("status" to JsonObject(accepted.getValue("status").jsonObject + (field to JsonPrimitive(row.string("token"))))))
            assertEquals("${row.string("id")} in $field", row.string("expected") == "accepted", decodes(reply))
        }
        val rows = clientRows(status.getValue("mutations").jsonArray)
        assertTrue(rows.size >= 10)
        for (row in rows) assertEquals(row.string("id"), row.string("expected") == "accepted", decodes(mutated(accepted, row, accepted.string("session_id"))))
    }

    @Test fun sharedHlsStartRepliesBindOnlyBsVodSessionAndControlTuple(): Unit = runBlocking {
        val start = fixture.getValue("hls_start").jsonObject
        val public = start.getValue("public").jsonObject
        val session = start.getValue("receiver").string("session_id")
        val base = base()
        val bound = SharedStart.decode(public.toString()).bindInitial(base, hlsRequest())
        assertEquals(session, bound.sessionId)
        assertEquals(start.getValue("receiver").string("incarnation_id"), bound.start.response.control!!.generation)
        val rows = clientRows(start.getValue("mutations").jsonArray)
        assertTrue(rows.size >= 15)
        for (row in rows) assertEquals(row.string("id"), row.string("expected") == "accepted",
            accepts { SharedStart.decode(mutated(public, row, session).toString()).bindInitial(base, hlsRequest()) })
    }

    @Test fun directStartRepliesMimeSetAndUrlBinding(): Unit = runBlocking {
        val direct = fixture.getValue("direct").jsonObject
        assertEquals(direct.getValue("mimes").jsonArray.map { it.jsonPrimitive.content }, SharedDirectMimes.toList())
        val public = direct.getValue("public").jsonObject
        val session = direct.string("b_session")
        val base = base()
        assertEquals(session, SharedStartedDirect.decode(public.toString(), base, directRequest()).sessionId)
        val rows = clientRows(direct.getValue("mutations").jsonArray)
        assertTrue(rows.size >= 12)
        for (row in rows) assertEquals(row.string("id"), row.string("expected") == "accepted",
            accepts { SharedStartedDirect.decode(mutated(public, row, session).toString(), base, directRequest()) })
        val fileBase = fixture.getValue("context").string("file_base")
        for (row in fixture.getValue("direct_session_query").jsonArray) {
            val reply = JsonObject(public + ("url" to JsonPrimitive("$fileBase/direct?${row.string("query")}")))
            assertEquals(row.string("id"), row.string("expected") == "accepted", accepts { SharedStartedDirect.decode(reply.toString(), base, directRequest()) })
        }
    }

    @Test fun sharedFileSuffixesFollowTheClosedSharedGrammar(): Unit = runBlocking {
        val rows = fixture.getValue("file_suffixes").jsonArray
        assertTrue(rows.size >= 20)
        // Bound to B's session, so the session-only routes are not what refuses a row.
        val bound = base().withSession(fixture.getValue("direct").string("b_session"))
        for (row in rows) {
            val suffix = row.string("suffix")
            assertEquals(suffix, row.string("expected") == "accepted", accepts { bound.path(suffix) })
            assertEquals(suffix, row.string("expected") == "accepted", PlaybackFileContext.sharedFileSuffix(suffix))
        }
    }

    @Test fun presessionAssetsComposeBoundAndUnboundPaths(): Unit = runBlocking {
        val assets = fixture.getValue("presession_assets").jsonObject
        val base = base()
        val session = assets.string("bound_query").removePrefix("session=")
        val bound = base.withSession(session)
        val fileBase = fixture.getValue("context").string("file_base")
        for (suffix in assets.getValue("suffixes").jsonArray.map { it.jsonPrimitive.content }) {
            assertEquals("$fileBase/$suffix", base.path(suffix))
            assertEquals("$fileBase/$suffix?${assets.string("bound_query")}", bound.path(suffix))
        }
    }

    @Test fun bControlRefusalsReachTheSharedChannelAsRetryOrStop(): Unit = runBlocking {
        val refusals = fixture.getValue("control_refusals").jsonObject
        val rows = refusals.getValue("source").jsonArray.map { it.jsonObject }.filter { it.getValue("valid").jsonPrimitive.boolean } +
            refusals.getValue("b_precheck").jsonArray.map { it.jsonObject }.filter { it["b"] !is JsonNull }
        assertTrue(rows.size >= 10)
        for (row in rows) {
            val b = row.getValue("b").jsonObject
            val f = SharedFixture("301"); f.login()
            val client = SharedDecisionClient.forTest(f.transport())
            val playback = client.start(f.context(), f.plan(f.context()).request)
            val reply = buildJsonObject {
                put("code", b.string("code")); put("message", "fixture")
                b["retry_after_ms"]?.takeUnless { it is JsonNull }?.let { put("retry_after_ms", it) }
                b["invalid_field"]?.let { put("invalid_field", it) }
            }.toString()
            f.control = { n, body -> if (n == 1) b.getValue("status").jsonPrimitive.int to reply else 200 to f.accepted(body) }
            val channel = SharedControlChannel(client, playback, "dddddddd-dddd-4ddd-8ddd-dddddddddddd", sharedControlCapabilities(f.caps()))
            val outcome = channel.exchange(SharedControlState(PlaybackDemand.ACTIVE, 1_000, 11_000, RenderState.RENDERING), f.plan(f.context()).frozenControlSelection())
            val sent = f.bodies.filter { it.first.startsWith("control") }.map { it.second }
            val observed = if (sent.size > 1) "retry" else "stop"
            assertEquals(row.string("id"), row.string("client"), observed)
            if (observed == "retry") {
                // A deferred exchange is the same request again, byte for byte, and then accepted.
                assertEquals(row.string("id"), 1, sent.toSet().size)
                assertTrue(row.string("id"), outcome is SharedControlOutcome.Accepted)
            } else assertTrue(row.string("id"), outcome is SharedControlOutcome.Refused || outcome is SharedControlOutcome.Ended)
        }
    }

    @Test fun preparationAnswersNoneDeclinesAtOnceAbsenceStaysArmed() {
        for (row in fixture.getValue("control_preparation").jsonArray.map { it.jsonObject }) {
            val preparation = row["b"]?.takeUnless { it is JsonNull }?.jsonPrimitive?.content
            val wait = PreparedOfferWait(tappedAtMs = 0, floorSequence = 1, noneOnTheAskDeclines = true)
            val step = wait.observe(ControlAnswer(1, ControlAction(type = "none"), preparation), nowMs = 0)
            val outcome = if (step is PreparedOfferWait.Step.Reopen) "declined" else "armed"
            assertEquals(row.string("id"), row.string("client"), outcome)
        }
    }
}
