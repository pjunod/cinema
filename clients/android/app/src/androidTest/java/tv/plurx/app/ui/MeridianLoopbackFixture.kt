package tv.plurx.app.ui

import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import tv.plurx.app.data.*
import java.net.InetAddress
import java.net.ServerSocket
import java.net.SocketException
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread

/** Bounded synthetic loopback transport for the actual production navigation/screens.
 * meridian-owner-green.mp4 is an original silent 30-second green AVC fixture:
 * ffmpeg -f lavfi -i color=c=green:s=320x180:r=12 -t 30 -an -c:v libx264
 *   -pix_fmt yuv420p -movflags +faststart meridian-owner-green.mp4
 */
internal class MeridianLoopbackFixture : AutoCloseable {
    private val socket = ServerSocket(0, 16, InetAddress.getByName("127.0.0.1"))
    val origin = "http://127.0.0.1:${socket.localPort}"
    val queries = CopyOnWriteArrayList<Pair<String, String?>>()
    val episodeCalls = AtomicInteger()
    val decisionCalls = AtomicInteger()
    val holdEpisode = AtomicBoolean(false)
    val releaseEpisode = CountDownLatch(1)
    private val json = Json { encodeDefaults = true }
    private val identity = SharedLibraryIdentity("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", "33333333-3333-4333-8333-333333333333", "7")
    private val media = InstrumentationRegistry.getInstrumentation().context.assets.open("meridian-owner-green.mp4").use { it.readBytes() }
    private val worker = thread(name = "meridian-loopback", isDaemon = true) {
        while (!socket.isClosed) {
            try { val client = socket.accept(); thread(isDaemon = true) { try { client.use { connection ->
                val input = connection.getInputStream().bufferedReader()
                val request = input.readLine() ?: return@use
                val headers = mutableMapOf<String, String>()
                while (true) { val line = input.readLine() ?: break; if (line.isEmpty()) break; val split = line.indexOf(':'); if (split > 0) headers[line.take(split).lowercase()] = line.substring(split + 1).trim() }
                var remaining = headers["content-length"]?.toIntOrNull() ?: 0
                while (remaining > 0) { val count = input.read(CharArray(remaining.coerceAtMost(8192))); if (count < 0) break; remaining -= count }
                val uri = android.net.Uri.parse(request.split(' ')[1])
                val path = uri.path.orEmpty()
                val payload: ByteArray
                var extra = "Content-Type: application/json\r\n"
                var status = "200 OK"
                if (path == "/media.mp4") {
                    val range = headers["range"]?.removePrefix("bytes=")?.split('-')
                    val first = range?.firstOrNull()?.toIntOrNull() ?: 0
                    val last = (range?.getOrNull(1)?.toIntOrNull() ?: media.lastIndex).coerceAtMost(media.lastIndex)
                    payload = media.copyOfRange(first.coerceAtMost(media.size), (last + 1).coerceAtLeast(first).coerceAtMost(media.size))
                    extra = "Content-Type: video/mp4\r\nAccept-Ranges: bytes\r\n"
                    if (range != null) { status = "206 Partial Content"; extra += "Content-Range: bytes $first-$last/${media.size}\r\n" }
                } else {
                    val body = when {
                        path == "/api/v1/shared/libraries" -> "{\"libraries\":[${json.encodeToString(SharedLibraryAssignment(identity.import_id, identity.server_id, identity.catalogue_epoch, "7", "Fixture Source", "online"))}]}"
                        path == "/api/v1/shared/continue-watching" -> "{\"groups\":[]}"
                        path.endsWith("/libraries") && path.contains("/shared/imports/") -> "{\"import_id\":\"${identity.import_id}\",\"server_id\":\"${identity.server_id}\",\"catalogue_epoch\":\"${identity.catalogue_epoch}\",\"libraries\":[{\"library_id\":\"7\",\"name\":\"Shared films\",\"kind\":\"movie\",\"anime\":false}]}"
                        path.endsWith("/libraries/7/items") -> {
                            val q = uri.getQueryParameter("q").orEmpty(); queries += q to uri.getQueryParameter("cursor")
                            val title = if (q.isBlank()) "Original Shared title" else "Filtered $q"
                            json.encodeToString(SharedLibraryPage(listOf(SharedLibraryItem("shared", identity.reference("31"), title = title, kind = "movie")), if (q.isBlank()) "old-query-cursor" else null, 1, 1, 1))
                        }
                        path.contains("/shared/imports/") && path.endsWith("/items/31") -> json.encodeToString(SharedLibraryDetail(SharedLibraryItem("shared", identity.reference("31"), title = "Nested Shared detail", kind = "movie"), emptyList(), delivery_status = "unavailable"))
                        path == "/api/v1/items/20" -> json.encodeToString(ItemDetail(Item(20, kind = "season", title = "Fixture season"), children = listOf(Item(21, kind = "episode", title = "Delayed episode", episode_number = 1))))
                        path == "/api/v1/items/21" -> { episodeCalls.incrementAndGet(); if (holdEpisode.get()) releaseEpisode.await(15, TimeUnit.SECONDS); json.encodeToString(ItemDetail(Item(21, kind = "episode", title = "Delayed episode"), files = listOf(MediaFileDto(11, "fixture.mp4", duration_ms = 30_000, video_codec = "h264", container = "mp4")))) }
                        path == "/api/v1/items/1" -> json.encodeToString(ItemDetail(Item(1, kind = "movie", title = "Fixture active owner"), files = listOf(MediaFileDto(11, "fixture.mp4", duration_ms = 30_000, video_codec = "h264", container = "mp4"))))
                        path.contains("decision") -> { decisionCalls.incrementAndGet(); json.encodeToString(Decision(11, "direct_play", "$origin/media.mp4")) }
                        else -> "{}"
                    }
                    payload = body.toByteArray()
                }
                val output = connection.getOutputStream()
                output.write("HTTP/1.1 $status\r\n${extra}Content-Length: ${payload.size}\r\nConnection: close\r\n\r\n".toByteArray()); output.write(payload); output.flush()
            } } catch (_: java.io.IOException) { /* Expected when a screen cancels its request. */ } } } catch (_: SocketException) { if (!socket.isClosed) throw IllegalStateException("Fixture socket failed") }
        }
    }
    override fun close() { releaseEpisode.countDown(); socket.close(); worker.join(1000) }
}
