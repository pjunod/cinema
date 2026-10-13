package tv.plurx.app.diagnostics

import org.junit.Assert.*
import org.junit.Test
import java.net.HttpURLConnection
import java.net.InetAddress
import java.net.URL
import java.util.UUID

class DiagnosticLocalExportTest {
    private fun request(url: String, method: String = "GET"): Pair<Int,ByteArray> {
        val connection=URL(url).openConnection() as HttpURLConnection
        connection.connectTimeout=2000; connection.readTimeout=2000; connection.requestMethod=method
        try { val status=connection.responseCode; return status to (if (status < 400) connection.inputStream.readBytes() else ByteArray(0)) }
        finally { connection.disconnect() }
    }
    @Test fun onlyExplicitDownloadReceivesExactArchiveOnce() {
        val bytes=byteArrayOf(0x50,0x4b,3,4,10,0,99)
        DiagnosticLocalExport(bytes,UUID.randomUUID().toString(),InetAddress.getLoopbackAddress()).use { export ->
            assertEquals(404,request(export.url.substringBeforeLast('/')+"/wrong").first)
            assertEquals(403,request(export.url,"HEAD").first)
            val landing=request(export.url); assertEquals(200,landing.first)
            assertTrue(String(landing.second).contains("Download diagnostic ZIP"))
            assertFalse(landing.second.contentEquals(bytes))
            bytes[0]=0 // A later caller mutation cannot change the exported snapshot.
            val download=request(export.url,"POST"); assertEquals(200,download.first)
            assertArrayEquals(byteArrayOf(0x50,0x4b,3,4,10,0,99),download.second)
            assertTrue(runCatching { request(export.url,"POST") }.isFailure)
        }
    }
    @Test fun expiryAndCancellationRevokeTheDownload() {
        val expired=DiagnosticLocalExport(byteArrayOf(1),UUID.randomUUID().toString(),InetAddress.getLoopbackAddress(),50)
        Thread.sleep(150); assertTrue(runCatching { request(expired.url) }.isFailure); expired.close()
        val cancelled=DiagnosticLocalExport(byteArrayOf(1),UUID.randomUUID().toString(),InetAddress.getLoopbackAddress())
        cancelled.close(); assertTrue(runCatching { request(cancelled.url) }.isFailure)
    }
    @Test fun crossOriginAndOversizedRequestsCannotConsumeCapability() {
        DiagnosticLocalExport(byteArrayOf(1,2),UUID.randomUUID().toString(),InetAddress.getLoopbackAddress()).use { export ->
            val uri=java.net.URI(export.url)
            fun raw(header: String): String = java.net.Socket(uri.host,uri.port).use { peer ->
                peer.soTimeout=3000; peer.getOutputStream().write(header.toByteArray()); peer.getInputStream().readBytes().toString(Charsets.US_ASCII)
            }
            assertTrue(raw("POST ${uri.path} HTTP/1.1\r\nOrigin: http://attacker.invalid\r\n\r\n").startsWith("HTTP/1.1 403"))
            assertTrue(raw("POST ${uri.path} HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").startsWith("HTTP/1.1 400"))
            val oversized=runCatching { raw("GET ${uri.path} HTTP/1.1\r\nX: "+"a".repeat(8200)+"\r\n\r\n") }
            assertTrue(oversized.isFailure || oversized.getOrNull() == "")
            assertArrayEquals(byteArrayOf(1,2),request(export.url,"POST").second)
        }
    }
}
