package tv.plurx.app.data

import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

/**
 * Every row of the read-after contract (handoff §5.1), mirroring
 * `tests/web/read-after.test.js` case for case and adding the rows web
 * covers only implicitly.
 */
class ReadAfterFloorTest {
    private class Fixture {
        var now = 1_000_000_000L
        val floor = ReadAfterFloor { now }
        val generation get() = floor.generation()
        fun start(method: String = "GET") = method to floor.request(floor.generation())
        fun reply(sent: Pair<String, ReadAfterFloor.Ticket>, index: String? = null) =
            floor.observe(sent.second, sent.first, index)
        fun held(): String? = floor.request(floor.generation()).index
        fun advanceMs(ms: Long) { now += ms * 1_000_000L }
    }

    @Test fun writeFloorPreservesFullU64IndexesAndExpiresAfterSixtySecondsMonotonic() {
        val f = Fixture()
        f.reply(f.start("PUT"), "18446744073709551614")
        assertEquals("18446744073709551614", f.held())
        f.advanceMs(59_999)
        assertEquals("18446744073709551614", f.held())
        f.advanceMs(1)
        assertNull(f.held())
    }

    @Test fun theLargestU64IsAcceptedAndOneMoreIsNot() {
        val f = Fixture()
        f.reply(f.start("PUT"), "18446744073709551615")
        assertEquals("18446744073709551615", f.held())
        f.reply(f.start("PUT"), "18446744073709551616")
        assertNull(f.held())
    }

    @Test fun outOfOrderIndexedRepliesNeverLowerTheFloor() {
        val f = Fixture()
        val first = f.start("PUT")
        val second = f.start("PUT")
        f.reply(second, "42")
        f.reply(first, "41")
        assertEquals("42", f.held())
        // Numeric, not lexicographic: 100 > 99.
        f.reply(f.start("PUT"), "100")
        f.reply(f.start("PUT"), "99")
        assertEquals("100", f.held())
    }

    @Test fun anEqualValueRestartsTheExpiryAndASmallerOneDoesNot() {
        val f = Fixture()
        f.reply(f.start("PUT"), "42")
        f.advanceMs(50_000)
        f.reply(f.start("PUT"), "41")
        f.advanceMs(10_000)
        assertNull("a smaller value must not extend the window", f.held())

        f.reply(f.start("PUT"), "42")
        f.advanceMs(50_000)
        f.reply(f.start("PUT"), "42")
        f.advanceMs(50_000)
        assertEquals("42", f.held())
        f.advanceMs(10_000)
        assertNull(f.held())
    }

    @Test fun unknownInvalidatesReceiptsFromWritesAlreadyInFlight() {
        val f = Fixture()
        val first = f.start("PUT")
        val second = f.start("PUT")
        f.reply(first, "unknown")
        f.reply(second, "43")
        assertNull(f.held())
        f.reply(f.start("PUT"), "44")
        assertEquals("44", f.held())
    }

    @Test fun unindexedAndFailedWritesDiscardAnOlderFloor() {
        for (method in listOf("PUT", "POST", "DELETE", "PATCH")) {
            val unindexed = Fixture()
            unindexed.reply(unindexed.start("PUT"), "10")
            unindexed.reply(unindexed.start(method), null)
            assertNull(method, unindexed.held())

            val failed = Fixture()
            failed.reply(failed.start("PUT"), "10")
            val sent = failed.start(method)
            failed.floor.transportFailed(sent.second, sent.first)
            assertNull(method, failed.held())
        }
    }

    @Test fun unindexedAndFailedReadsKeepTheFloor() {
        for (method in listOf("GET", "HEAD")) {
            val f = Fixture()
            f.reply(f.start("PUT"), "10")
            f.reply(f.start(method), null)
            val sent = f.start(method)
            f.floor.transportFailed(sent.second, sent.first)
            assertEquals(method, "10", f.held())
        }
    }

    @Test fun everyForgetBumpsTheEpochSoALateOlderReplyCannotRestoreAFloor() {
        // Forget by an unindexed write.
        val f = Fixture()
        val slow = f.start("PUT")
        f.reply(f.start("PUT"), null)
        f.reply(slow, "50")
        assertNull(f.held())
        // Forget by a failed write.
        val g = Fixture()
        val late = g.start("PUT")
        val failed = g.start("PUT")
        g.floor.transportFailed(failed.second, failed.first)
        g.reply(late, "51")
        assertNull(g.held())
        // Forget by the signed-in session changing.
        val h = Fixture()
        val old = h.start("PUT")
        h.floor.signedInSessionChanged()
        h.reply(old, "52")
        assertNull(h.held())
    }

    @Test fun aMalformedReplyFromAStaleEpochStillForgets() {
        val f = Fixture()
        val stale = f.start("PUT")
        f.reply(f.start("PUT"), "unknown")
        f.reply(f.start("PUT"), "20")
        assertEquals("20", f.held())
        f.reply(stale, "garbage")
        assertNull(f.held())

        val g = Fixture()
        val staleUnknown = g.start("GET")
        g.reply(g.start("PUT"), null)
        g.reply(g.start("PUT"), "30")
        g.reply(staleUnknown, "unknown")
        assertNull(g.held())
    }

    @Test fun authenticationChangesDiscardThePriorUsersFloorAndReplies() {
        val f = Fixture()
        f.reply(f.start("PUT"), "12")
        val late = f.start("PUT")
        val lateFailure = f.start("POST")
        val lateUnindexed = f.start("POST")
        val previous = f.generation
        f.floor.signedInSessionChanged()
        assertEquals(previous + 1, f.generation)
        assertNull(f.held())
        f.reply(late, "99")
        assertNull(f.held())

        f.reply(f.start("PUT"), "5")
        // The old session's failures and unindexed writes are not this one's.
        f.floor.transportFailed(lateFailure.second, lateFailure.first)
        f.reply(lateUnindexed, null)
        f.reply(late, "garbage")
        assertEquals("5", f.held())
    }

    @Test fun aRequestBoundToAnEndedSessionSendsNothingAndCapturesNothing() {
        val f = Fixture()
        val old = f.generation
        f.floor.signedInSessionChanged()
        f.reply(f.start("PUT"), "8")
        val bound = f.floor.request(old)
        assertNull(bound.index)
        f.floor.observe(bound, "PUT", "900")
        f.floor.observe(f.floor.request(old), "PUT", null)
        assertEquals("8", f.held())
    }

    @Test fun malformedAndOverflowingReceiptsNeverBecomeReadHeadersAndForget() {
        for (index in listOf("0", "01", "-1", "18446744073709551616", "99999999999999999999",
                "123456789012345678901", "12, 13", "", " 12", "12 ", "+1", "1e3", "0x10", "unknown", "UNKNOWN")) {
            val f = Fixture()
            f.reply(f.start("PUT"), "10")
            f.reply(f.start("GET"), index)
            assertNull("'$index'", f.held())
        }
    }

    @Test fun nothingIsSentUntilAValueIsHeld() {
        val f = Fixture()
        assertNull(f.held())
        f.reply(f.start("GET"), null)
        assertNull(f.held())
        f.reply(f.start("GET"), "7")
        assertEquals("7", f.held())
    }

    @Test fun concurrentRepliesKeepTheLargestValue() {
        val f = Fixture()
        val pool = Executors.newFixedThreadPool(8)
        val done = CountDownLatch(8)
        try {
            repeat(8) { worker ->
                pool.execute {
                    try {
                        for (i in 1..2_000) {
                            val sent = f.start("PUT")
                            f.reply(sent, (worker + 8 * i).toString())
                            f.held()
                        }
                    } finally {
                        done.countDown()
                    }
                }
            }
            assertEquals(true, done.await(30, TimeUnit.SECONDS))
        } finally {
            pool.shutdownNow()
        }
        assertEquals((7 + 8 * 2_000).toString(), f.held())
    }

    // ---- The one interceptor -------------------------------------------------

    private class Wire(val floor: ReadAfterFloor) {
        val sent = mutableListOf<String?>()
        var replyIndex: List<String> = emptyList()
        var fail = false
        val client: OkHttpClient = OkHttpClient.Builder()
            .addInterceptor(ReadAfterInterceptor(floor))
            .addInterceptor(Interceptor { chain ->
                sent += chain.request().header(ReadAfterFloor.READ_AFTER_HEADER)
                if (fail) throw IOException("connection lost")
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .apply { replyIndex.forEach { addHeader(ReadAfterFloor.COMMIT_INDEX_HEADER, it) } }
                    .body("{}".toResponseBody())
                    .build()
            })
            .build()

        fun call(url: String, method: String = "GET", bound: Boolean = true) {
            val request = Request.Builder().url(url)
                .method(method, if (method == "GET" || method == "HEAD") null else ByteArray(0).toRequestBody())
                .apply { if (bound) tag(ReadAfterBinding::class.java, ReadAfterBinding(floor.generation())) }
                .build()
            client.newCall(request).execute().close()
        }
    }

    @Test fun boundCallsEchoAndCaptureWhicheverNodeAnswers() {
        val wire = Wire(ReadAfterFloor())
        wire.replyIndex = listOf("41")
        wire.call("http://node-a.test/api/v1/items/1/progress", "POST")
        wire.replyIndex = emptyList()
        wire.call("http://node-b.test/api/v1/hubs")
        assertEquals(listOf(null, "41"), wire.sent)
    }

    @Test fun unboundCallsNeitherEchoNorCaptureNorForget() {
        val wire = Wire(ReadAfterFloor())
        wire.replyIndex = listOf("41")
        wire.call("http://node.test/api/v1/items/1/progress", "POST")
        wire.replyIndex = listOf("900")
        wire.call("http://node.test/api/v1/images/1/poster", bound = false)
        wire.replyIndex = emptyList()
        wire.call("http://node.test/api/v1/auth/logout", "POST", bound = false)
        wire.fail = true
        assertThrows(IOException::class.java) {
            wire.call("http://node.test/api/v1/offline/packages/x", "DELETE", bound = false)
        }
        wire.fail = false
        wire.call("http://node.test/api/v1/hubs")
        assertEquals(listOf(null, null, null, null, "41"), wire.sent)
    }

    @Test fun aBoundWriteThatFailsInTransportForgetsAndStillThrows() {
        val wire = Wire(ReadAfterFloor())
        wire.replyIndex = listOf("41")
        wire.call("http://node.test/api/v1/items/1/scrobble", "POST")
        wire.fail = true
        assertThrows(IOException::class.java) { wire.call("http://node.test/api/v1/items/1/scrobble", "POST") }
        wire.fail = false
        wire.replyIndex = emptyList()
        wire.call("http://node.test/api/v1/hubs")
        assertEquals(listOf(null, "41", null), wire.sent)
    }

    @Test fun repeatedCommitHeadersAreMalformed() {
        val wire = Wire(ReadAfterFloor())
        wire.replyIndex = listOf("41")
        wire.call("http://node.test/api/v1/items/1/progress", "POST")
        wire.replyIndex = listOf("42", "43")
        wire.call("http://node.test/api/v1/items/1/progress", "POST")
        wire.replyIndex = emptyList()
        wire.call("http://node.test/api/v1/hubs")
        assertEquals(listOf(null, "41", null), wire.sent)
    }
}
