package tv.plurx.app.data

import okhttp3.Call
import okhttp3.Connection
import okhttp3.Handshake
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Route
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.net.Socket
import java.util.Collections
import kotlin.concurrent.thread

class NetCallDiagnosticsTest {
    private val client = OkHttpClient()
    private fun call(path: String): Call = client.newCall(
        Request.Builder().url("https://synthetic.invalid$path").build(),
    )
    private val connection = object : Connection {
        override fun route(): Route = error("The diagnostic must not inspect the connection")
        override fun socket(): Socket = error("The diagnostic must not inspect the connection")
        override fun handshake(): Handshake? = error("The diagnostic must not inspect the connection")
        override fun protocol(): Protocol = error("The diagnostic must not inspect the connection")
    }

    @Test fun routeAndMethodHaveOnlyFixedCategories() {
        assertEquals(NetCallDiagnostics.Route.Images, NetCallDiagnostics.route("/api/v1/images/synthetic"))
        assertEquals(NetCallDiagnostics.Route.Decision, NetCallDiagnostics.route("/api/v1/files/17/decision"))
        listOf("/api/v1/files/synthetic/decision", "/api/v1/files/17/decision/extra", "/private/capability").forEach {
            assertEquals(NetCallDiagnostics.Route.Other, NetCallDiagnostics.route(it))
        }
        assertEquals(NetCallDiagnostics.Method.Get, NetCallDiagnostics.method("GET"))
        assertEquals(NetCallDiagnostics.Method.Post, NetCallDiagnostics.method("POST"))
        assertEquals(NetCallDiagnostics.Method.Other, NetCallDiagnostics.method("SYNTHETIC_PRIVATE_METHOD"))
    }

    @Test fun decisionTraceMeasuresGapBesideFiveOutstandingImages() {
        var time = 1_000_000_000L
        val records = mutableListOf<NetCallDiagnostics.Record>()
        val factory = NetCallDiagnostics.factory(clock = { time }, sink = records::add)
        val images = (1..5).map { call("/api/v1/images/$it") }.map { it to factory.create(it) }
        images.forEach { (call, listener) -> listener.callStart(call) }
        val decision = call("/api/v1/files/17/decision")
        val listener = factory.create(decision)
        listener.callStart(decision)
        time += 125_000_000L
        listener.connectionAcquired(decision, connection)
        time += 25_000_000L
        listener.responseHeadersStart(decision)
        val phases = records.filter { it.route == NetCallDiagnostics.Route.Decision }
        assertEquals(listOf(0L, 125_000L, 150_000L), phases.map { it.elapsedUs })
        assertTrue(phases.all { it.outstandingImages == 5 })
        images.forEach { (call, image) -> image.callEnd(call) }
        listener.callEnd(decision)
        assertEquals(0, records.last().outstandingImages)
    }

    @Test fun successFailureAndCancellationRetireEachImageExactlyOnce() {
        val records = mutableListOf<NetCallDiagnostics.Record>()
        val factory = NetCallDiagnostics.factory(clock = { 1_000L }, sink = records::add)
        val calls = (1..3).map { call("/api/v1/images/$it") }.map { it to factory.create(it) }
        calls.forEach { (call, listener) -> listener.callStart(call); listener.callStart(call) }
        val (success, first) = calls[0]
        first.callEnd(success)
        first.callFailed(success, IOException("synthetic sensitive failure"))
        val (failed, second) = calls[1]
        second.callFailed(failed, IOException("synthetic sensitive failure"))
        second.callEnd(failed)
        val (cancelled, third) = calls[2]
        cancelled.cancel()
        third.callFailed(cancelled, IOException("synthetic sensitive failure"))
        third.callFailed(cancelled, IOException("synthetic sensitive failure"))
        val terminal = records.filter { it.phase in setOf(NetCallDiagnostics.Phase.CallEnd,
            NetCallDiagnostics.Phase.CallFailed, NetCallDiagnostics.Phase.Cancelled) }
        assertEquals(listOf(2, 1, 0), terminal.map { it.outstandingImages })
        assertEquals(listOf(NetCallDiagnostics.Phase.CallEnd, NetCallDiagnostics.Phase.CallFailed,
            NetCallDiagnostics.Phase.Cancelled), terminal.map { it.phase })
        assertEquals(6, records.size)
        assertFalse(records.any { it.line().contains("sensitive") })
    }

    @Test fun retriesAreBoundedAndRequestAuthorityNeverAppearsInRecords() {
        val records = mutableListOf<NetCallDiagnostics.Record>()
        val factory = NetCallDiagnostics.factory(clock = { 1_000L }, sink = records::add)
        val request = Request.Builder()
            .url("https://synthetic.invalid/api/v1/files/17/decision?token=synthetic-query")
            .header("Authorization", "Bearer synthetic-header").build()
        val call = client.newCall(request)
        val listener = factory.create(call)
        listener.callStart(call)
        repeat(10) {
            listener.connectionAcquired(call, connection)
            listener.responseHeadersStart(call)
        }
        listener.callEnd(call)
        listener.responseHeadersStart(call)
        assertEquals(4, records.size)
        records.forEach {
            val line = it.line()
            listOf("synthetic", "invalid", "/", "token", "Bearer", "Authorization", "17").forEach { privateValue ->
                assertFalse("Unexpected request data in diagnostic record", line.contains(privateValue))
            }
        }
    }

    @Test fun concurrentImageCompletionsLeaveNoOutstandingCalls() {
        val records = Collections.synchronizedList(mutableListOf<NetCallDiagnostics.Record>())
        val factory = NetCallDiagnostics.factory(clock = { 1_000L }, sink = records::add)
        val calls = (1..32).map { call("/api/v1/images/$it") }.map { it to factory.create(it) }
        calls.map { (call, listener) -> thread { listener.callStart(call) } }.forEach { it.join() }
        calls.map { (call, listener) -> thread { listener.callFailed(call, IOException("synthetic")) } }.forEach { it.join() }
        val check = call("/api/v1/files/17/decision")
        factory.create(check).callStart(check)
        assertEquals(0, records.last().outstandingImages)
        assertEquals(32, records.count { it.phase == NetCallDiagnostics.Phase.CallFailed })
        assertTrue(records.all { it.outstandingImages in 0..32 })
    }

    @Test fun imagesFromAnotherOriginDoNotImplyDecisionContention() {
        val records = mutableListOf<NetCallDiagnostics.Record>()
        val factory = NetCallDiagnostics.factory(clock = { 1_000L }, sink = records::add)
        val images = (1..5).map {
            client.newCall(Request.Builder().url("https://other.synthetic.invalid/api/v1/images/$it").build())
        }.map { it to factory.create(it) }
        images.forEach { (call, listener) -> listener.callStart(call) }
        val decision = call("/api/v1/files/17/decision")
        factory.create(decision).callStart(decision)
        assertEquals(0, records.last().outstandingImages)
        assertFalse(records.any { it.line().contains("synthetic") })
        images.forEach { (call, listener) -> listener.callEnd(call) }
    }

    @Test fun loggingFailureCannotFailTheCallOrLeakAnImageCount() {
        var fail = true
        val records = mutableListOf<NetCallDiagnostics.Record>()
        val factory = NetCallDiagnostics.factory(clock = { 1_000L }, sink = {
            if (fail) error("synthetic logger failure") else records.add(it)
        })
        val image = call("/api/v1/images/synthetic")
        val listener = factory.create(image)
        listener.callStart(image)
        listener.callFailed(image, IOException("synthetic"))
        fail = false
        val check = call("/api/v1/files/17/decision")
        factory.create(check).callStart(check)
        assertEquals(0, records.single().outstandingImages)
    }
}
