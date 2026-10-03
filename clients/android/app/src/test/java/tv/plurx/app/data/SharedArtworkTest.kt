package tv.plurx.app.data

import kotlinx.coroutines.*
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.io.IOException
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okhttp3.MediaType.Companion.toMediaType
import org.junit.Assert.*
import org.junit.After
import org.junit.Before
import org.junit.Test

class SharedArtworkTest {
    private val source = SharedLibraryIdentity("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", "33333333-3333-4333-8333-333333333333", "9007199254740993")
    private val reference get() = source.reference("9223372036854775807")
    private val url get() = "/api/v1/shared/imports/${source.import_id}/art/" + "A".repeat(272)
    @Before fun setup() { Session.origin = "https://b.test"; Session.token = "art-bearer" }
    @After fun teardown() { Session.token = null; Session.origin = "" }
    @Test fun closedArtworkGrammarAndAliasesRejectForeignAndLegacyUrls() {
        val valid = SharedArtworkDescriptor("poster", "w300", url)
        valid.validate(reference)
        SharedArtworkDescriptor.validate(listOf(valid), url, null, reference)
        listOf("https://a.test$url", "$url?token=x", "$url#fragment", url.replace("A", "%41"), url.dropLast(129), url.replace(source.import_id, "44444444-4444-4444-8444-444444444444")).forEach {
            assertThrows(IllegalArgumentException::class.java) { valid.copy(url = it).validate(reference) }
        }
        assertThrows(IllegalArgumentException::class.java) { SharedArtworkDescriptor.validate(listOf(valid, valid), url, null, reference) }
        assertThrows(IllegalArgumentException::class.java) { SharedArtworkDescriptor.validate(listOf(valid), null, url, reference) }
        assertThrows(IllegalArgumentException::class.java) { SharedArtworkDescriptor.validate(emptyList(), url, null, reference) }
    }
    @Test fun nonqueuedCreditsRemainChargedUntilActualOwnerRelease() {
        val budget = SharedArtworkBudget(64, 4)
        val leases = List(4) { budget.acquire(15) }
        assertEquals(60L, budget.retainedBytes())
        assertThrows(IllegalArgumentException::class.java) { budget.acquire(1) }
        leases[0].close(); leases[0].close()
        val replacement = budget.acquire(19)
        assertEquals(64L, budget.retainedBytes())
        assertThrows(IllegalArgumentException::class.java) { budget.acquire(1) }
        assertFalse(SharedArtworkBudget.Lease(budget, 19).validFor(budget, 19))
        replacement.close(); leases.drop(1).forEach { it.close() }; assertEquals(0L, budget.retainedBytes())
    }
    private fun detail(): JsonObject = buildJsonObject {
        put("item", buildJsonObject {
            put("source", "shared"); put("reference", Json.encodeToJsonElement(reference)); put("title", "Source A"); put("kind", "movie"); put("genres", buildJsonArray {})
            put("art", buildJsonArray { add(buildJsonObject { put("kind", "poster"); put("variant", "w300"); put("url", url) }) }); put("poster_url", url)
        }); put("files", buildJsonArray {}); put("delivery_status", "unavailable")
    }
    @Test fun authenticatedMetadataAloneCreatesBOnlyArtworkAndRetiresOldAccounts(): Unit = runBlocking {
        var artReads = 0
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            val request = chain.request()
            assertEquals("b.test", request.url.host); assertEquals("Bearer art-bearer", request.header("Authorization"))
            val art = request.url.encodedPath == url
            val body = if (art) { artReads++; byteArrayOf(1, 2, 3).toResponseBody("image/png".toMediaType()) } else detail().toString().toResponseBody("application/json".toMediaType())
            Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("fixture").body(body).header("Content-Type", if (art) "image/png" else "application/json").build()
        }.build()
        val item = SharedLibraryClient.forTest(transport).detail(reference).item
        (item.art as? MutableList)?.clear()
        val subject = requireNotNull(item.artworkSubject); val descriptor = requireNotNull(subject.descriptor(false))
        val before = SharedArtworkBudget.compressed.retainedBytes()
        subject.read(descriptor).use { assertEquals(3, it.length); assertEquals("image/png", it.mime); assertEquals(before + SharedArtworkBudget.ASSET_LIMIT, SharedArtworkBudget.compressed.retainedBytes()) }
        assertEquals(before, SharedArtworkBudget.compressed.retainedBytes()); assertEquals(1, artReads)
        assertNotNull(runCatching { subject.read(descriptor.copy(url = url.dropLast(1) + "B")) }.exceptionOrNull())
        Session.token = "replacement"
        assertNotNull(runCatching { subject.read(descriptor) }.exceptionOrNull()); assertEquals(1, artReads)
    }

    @Test fun actualInFlightCancellationCancelsTransportAndReturnsAdmission(): Unit = runBlocking {
        val started = CountDownLatch(1)
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            val request = chain.request()
            if (request.url.encodedPath == url) {
                started.countDown()
                while (!chain.call().isCanceled()) Thread.sleep(5)
                throw IOException("cancelled actual call")
            }
            Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("fixture").body(detail().toString().toResponseBody()).build()
        }.build()
        val subject = requireNotNull(SharedLibraryClient.forTest(transport).detail(reference).item.artworkSubject)
        val descriptor = requireNotNull(subject.descriptor(false)); val before = SharedArtworkBudget.compressed.retainedBytes()
        val task = launch(Dispatchers.IO) { subject.read(descriptor).close() }
        assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
        withTimeout(5000) { task.cancelAndJoin() }
        assertEquals(before, SharedArtworkBudget.compressed.retainedBytes())
    }
    @Test fun invalidMimeAndAccountChangeDuringObjectFetchNeverPublish(): Unit = runBlocking {
        var mode = 0
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            val request = chain.request(); val art = request.url.encodedPath == url
            if (art && mode == 1) Session.token = "changed-during-object"
            Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("fixture")
                .header("Content-Type", if (art) "text/html" else "application/json")
                .body(if (art) "foreign page".toResponseBody() else detail().toString().toResponseBody()).build()
        }.build()
        val subject = requireNotNull(SharedLibraryClient.forTest(transport).detail(reference).item.artworkSubject)
        val descriptor = requireNotNull(subject.descriptor(false)); val before = SharedArtworkBudget.compressed.retainedBytes()
        assertNotNull(runCatching { subject.read(descriptor) }.exceptionOrNull())
        assertEquals(before, SharedArtworkBudget.compressed.retainedBytes())
        mode = 1
        assertNotNull(runCatching { subject.read(descriptor) }.exceptionOrNull())
        assertEquals(before, SharedArtworkBudget.compressed.retainedBytes())
    }

    @Test fun declaredAndUnknownLengthOversizeBodiesReturnAllAdmission(): Unit = runBlocking {
        var declared = true
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            val request = chain.request(); val art = request.url.encodedPath == url
            val body = if (!art) detail().toString().toResponseBody() else object : okhttp3.ResponseBody() {
                private val stream = okio.Buffer().apply { if (!declared) write(ByteArray(SharedArtworkBudget.ASSET_LIMIT + 1)) }
                override fun contentType() = "image/png".toMediaType()
                override fun contentLength() = if (declared) SharedArtworkBudget.ASSET_LIMIT.toLong() + 1 else -1L
                override fun source(): okio.BufferedSource = stream
            }
            Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("fixture").header("Content-Type", if (art) "image/png" else "application/json").body(body).build()
        }.build()
        val subject = requireNotNull(SharedLibraryClient.forTest(transport).detail(reference).item.artworkSubject)
        val descriptor = requireNotNull(subject.descriptor(false)); val before = SharedArtworkBudget.compressed.retainedBytes()
        assertNotNull(runCatching { subject.read(descriptor) }.exceptionOrNull())
        assertEquals(before, SharedArtworkBudget.compressed.retainedBytes())
        declared = false
        assertNotNull(runCatching { subject.read(descriptor) }.exceptionOrNull())
        assertEquals(before, SharedArtworkBudget.compressed.retainedBytes())
    }

    @Test fun arbitraryPackageLocalPlanCannotBypassAuthenticatedFactory(): Unit = runBlocking {
        var attempted = false
        val rejected = runCatching {
            val plan = java.lang.reflect.Proxy.newProxyInstance(SharedArtworkReadPlan::class.java.classLoader,
                arrayOf(SharedArtworkReadPlan::class.java)) { _, method, _ ->
                when (method.name) {
                    "getRequest" -> okhttp3.Request.Builder().url("https://a.test/foreign").build()
                    "getTransport" -> { attempted = true; OkHttpClient() }
                    "getGeneration" -> Session.playbackAuthorization().generation
                    else -> Unit
                }
            } as SharedArtworkReadPlan
            SharedArtworkPayload.fetch(plan)
        }
        assertNotNull(rejected.exceptionOrNull())
        assertFalse(attempted)
    }
}
