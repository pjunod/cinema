package tv.plurx.app.diagnostics

import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.ServerSocket
import java.net.Socket
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicBoolean

/** Foreground-only, single-download capability. No support authority or file
 * path is accepted from the requester; only this immutable archive is served. */
internal class DiagnosticLocalExport(
    bytes: ByteArray, reportID: String, address: InetAddress = localAddress(),
    lifetimeMillis: Long = 300_000, private val ended: () -> Unit = {},
) : AutoCloseable {
    private var archive = bytes.copyOf().also {
        require(bytes.isNotEmpty() && bytes.size <= 8_388_608 && Regex("[a-f0-9-]{36}").matches(reportID))
        require(lifetimeMillis in 1..300_000)
    }
    private val live = AtomicBoolean(true)
    private val socket = ServerSocket(0, 1, address)
    private val timer = java.util.Timer("cinema-report-expiry",true)
    @Volatile private var connection: Socket? = null
    private val token = ByteArray(32).also { SecureRandom().nextBytes(it) }.joinToString("") { "%02x".format(it) }
    val url = "http://${address.hostAddress}:${socket.localPort}/$token"
    private val origin = url.substringBeforeLast('/')
    private val filename = "cinema-report-$reportID.zip"
    private val expires = System.nanoTime() + lifetimeMillis * 1_000_000
    init {
        timer.schedule(object : java.util.TimerTask() { override fun run() { close() } },lifetimeMillis)
        socket.soTimeout = 250
        Thread({
            try {
                var count = 0
                while (live.get() && System.nanoTime() < expires && count < 32) {
                    val peer = try { socket.accept() } catch (_: java.net.SocketTimeoutException) { continue }
                    count++; connection = peer
                    peer.use { runCatching { serve(it) } }; connection = null
                }
            } catch (_: java.io.IOException) {
                // Closing or expiring a ServerSocket interrupts accept on
                // Android. That is normal retirement, not an app exception.
            } finally { close(); ended() }
        }, "cinema-report-download").apply { isDaemon = true; start() }
    }
    private fun serve(peer: Socket) {
        peer.soTimeout = 1500
        val input = peer.getInputStream(); val header = java.io.ByteArrayOutputStream()
        val deadline = minOf(expires, System.nanoTime() + 2_000_000_000)
        while (header.size() < 8192 && live.get() && System.nanoTime() < deadline) {
            val value = input.read(); if (value < 0) return; header.write(value)
            val data = header.toByteArray()
            if (data.size >= 4 && data.takeLast(4) == listOf<Byte>(13,10,13,10)) break
        }
        if (!live.get() || System.nanoTime() >= deadline || header.size() >= 8192) return
        val lines = header.toString("US-ASCII").split("\r\n")
        val request = lines.first().split(' ')
        if (request.size != 3 || request[1] != "/$token" || request[2] !in setOf("HTTP/1.1","HTTP/1.0")) { reply(peer,404,"text/plain",ByteArray(0)); return }
        val fields = lines.drop(1).filter { it.isNotEmpty() }.map { it.substringBefore(':').lowercase() to it.substringAfter(':', "").trim() }
        if (fields.any { it.first == "transfer-encoding" || (it.first == "content-length" && it.second != "0") } || fields.groupBy { it.first }.any { it.value.size > 1 }) { reply(peer,400,"text/plain",ByteArray(0)); return }
        if (request[0] == "GET") {
            val page = "<!doctype html><meta name=viewport content='width=device-width'><title>Cinema diagnostic ZIP</title><h1>Save this TV’s diagnostic ZIP</h1><p>Attach the downloaded ZIP to your bug or issue report. This does not submit it.</p><form method=post><button>Download diagnostic ZIP</button></form>"
            reply(peer,200,"text/html; charset=utf-8",page.toByteArray()); return
        }
        if (request[0] != "POST" || fields.any { (it.first == "origin" && it.second != origin) || (it.first == "sec-fetch-site" && it.second == "cross-site") }) { reply(peer,403,"text/plain",ByteArray(0)); return }
        if (System.nanoTime() >= expires || !live.compareAndSet(true,false)) return
        reply(peer,200,"application/zip",archive,filename)
    }
    private fun reply(peer: Socket, status: Int, type: String, body: ByteArray, attachment: String? = null) {
        val headers = "HTTP/1.1 $status ${if (status == 200) "OK" else "Rejected"}\r\nContent-Type: $type\r\nContent-Length: ${body.size}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; form-action 'self'; frame-ancestors 'none'\r\n" + (attachment?.let { "Content-Disposition: attachment; filename=\"$it\"\r\n" } ?: "") + "\r\n"
        peer.getOutputStream().apply { write(headers.toByteArray(Charsets.US_ASCII)); write(body); flush() }
    }
    override fun close() {
        live.set(false); timer.cancel(); runCatching { socket.close() }; runCatching { connection?.close() }
        // Replace the reference; do not overwrite bytes while a cancelled
        // socket writer may still hold the old immutable buffer.
        archive = ByteArray(0)
    }
    companion object {
        fun localAddress(): InetAddress = NetworkInterface.getNetworkInterfaces().toList()
            .filter { it.isUp && !it.isLoopback && !it.isVirtual && !it.name.startsWith("tun") }
            .flatMap { it.inetAddresses.toList() }.firstOrNull { it is Inet4Address && it.isSiteLocalAddress }
            ?: error("Connect the TV and phone to the same local network, then retry.")
    }
}
