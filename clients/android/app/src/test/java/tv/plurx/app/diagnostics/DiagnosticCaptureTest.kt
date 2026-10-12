package tv.plurx.app.diagnostics

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.*
import org.junit.Test
import java.nio.file.Files
import java.util.zip.CRC32

class DiagnosticCaptureTest {
    @Test fun boundedRecorderRetainsTerminalAndExactCountersWithoutSecrets() {
        val recorder=DiagnosticRecorder(monotonic={ 0 })
        recorder.record("playback","terminal",mapOf("state" to JsonPrimitive("failed"),"failure" to JsonPrimitive("decode"),"url" to JsonPrimitive("CANARY_TOKEN")))
        recorder.record("playback","state",mapOf("frame_count" to JsonPrimitive("18446744073709551615"),"dropped_frames" to JsonPrimitive("9007199254740993"),"title" to JsonPrimitive("CANARY_TITLE")))
        repeat(20000) { recorder.record("playback","sample",mapOf("playhead_ms" to JsonPrimitive(it))) }
        val capture=recorder.freeze(); val bytes=DiagnosticReport.jsonl(capture.events)
        assertTrue(capture.events.size <= 10000); assertTrue(bytes.size <= DiagnosticRecorder.MAX_BYTES); assertTrue(capture.dropped.toULong() > 0uL)
        assertTrue(capture.events.any { it.kind == "terminal" }); val text=bytes.toString(Charsets.UTF_8)
        assertFalse(text.contains("CANARY")); assertTrue(text.contains("18446744073709551615")); assertTrue(text.contains("9007199254740993"))
    }
    @Test fun restartStoreRefusesAnotherAccountAndCannotResurrectClearedCapture() = runBlocking {
        val root=Files.createTempDirectory("cinema-diagnostic-fixture").toFile()
        try {
            val scope="a".repeat(64); val capture=DiagnosticRecorder(monotonic={ 0 }).freeze().copy(namespace=scope,persistence="available")
            val draft=DiagnosticDraft(capture,category="playback",description="user content")
            val first=DiagnosticStore(root); assertEquals("available",first.save(draft))
            val restart=DiagnosticStore(root); assertEquals(listOf(draft.id),restart.drafts(scope).map { it.id }); assertTrue(restart.drafts("b".repeat(64)).isEmpty())
            restart.clear(scope); assertEquals("unavailable",restart.save(draft)); restart.activate(scope); assertTrue(restart.drafts(scope).isEmpty())
        } finally { root.deleteRecursively() }
    }
    @Test fun queuedWriteCannotResurrectAfterSameAccountAuthenticatesAgain() = runBlocking {
        val root=Files.createTempDirectory("cinema-diagnostic-epoch").toFile(); val epoch=java.util.concurrent.atomic.AtomicLong(0)
        try {
            val store=DiagnosticStore(root,authorizationGeneration={ epoch.get() })
            val capture=DiagnosticRecorder(monotonic={ 0 }).freeze().copy(namespace="anonymous",generation=0,events=emptyList())
            val draft=DiagnosticDraft(capture)
            store.save(draft); store.clear("anonymous"); epoch.set(1); store.activate("anonymous")
            assertEquals("unavailable",store.save(draft)); assertTrue(store.drafts("anonymous").isEmpty())
        } finally { root.deleteRecursively() }
    }
    @Test fun portableZipUsesStandardCRCAndRefusesTraversal() {
        val value="123456789".toByteArray(); assertEquals(0xcbf43926L,CRC32().apply { update(value) }.value)
        val zip=DiagnosticReport.zip(mapOf("description.txt" to value)); assertEquals(listOf(0x26,0x39,0xf4,0xcb),zip.slice(14..17).map { it.toInt() and 255 })
        assertThrows(IllegalArgumentException::class.java) { DiagnosticReport.zip(mapOf("../outside" to value)) }
    }
    @Test fun supportDestinationRejectsForeignAuthorityShapesAndPlaintext() {
        listOf("https://user:secret@support.example","https://support.example/path","https://support.example?secret=canary","http://support.example","file:///tmp/report").forEach {
            assertThrows(Exception::class.java) { DiagnosticTransport.origin(it) }
        }
        assertEquals("https://support.example",DiagnosticTransport.origin("https://support.example/"))
    }
}
