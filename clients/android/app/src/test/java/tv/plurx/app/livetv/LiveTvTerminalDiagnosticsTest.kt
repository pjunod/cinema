package tv.plurx.app.livetv

import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.hls.playlist.HlsPlaylistTracker
import java.io.IOException
import kotlinx.serialization.encodeToString
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Net

@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
class LiveTvTerminalDiagnosticsTest {
    private class CapabilitySecretException : IOException("https://private.invalid?capability=secret")

    @Test fun terminalWireKeepsActualMedia3CodeButNeverExceptionPayloadOrDynamicClassName() {
        val error = PlaybackException("private title/account/session", CapabilitySecretException(),
            PlaybackException.ERROR_CODE_IO_UNSPECIFIED)
        val diagnostic = LiveTvTerminalDiagnostics { 10L }.terminal(
            LiveTvTerminalTrigger.PLAYER_ERROR, LiveTvTerminalAction.STOPPED,
            20, false, Player.STATE_IDLE, 123, error,
        )!!
        val wire = Net.json.encodeToString(diagnostic)
        assertEquals(PlaybackException.ERROR_CODE_IO_UNSPECIFIED, diagnostic.code)
        assertTrue(diagnostic.message.contains("error=ERROR_CODE_IO_UNSPECIFIED"))
        assertTrue(diagnostic.message.contains("known_type=other"))
        for (secret in listOf("private", "secret", "CapabilitySecretException", "session_id", "file_id")) {
            assertFalse("wire leaked $secret", wire.contains(secret))
        }
        assertNull(diagnostic.sessionId)
        assertNull(diagnostic.title)
        assertNull(diagnostic.fileId)
    }

    @Test fun knownPlaylistStuckClassIsStaticWhileUnknownCauseChainsStayBounded() {
        // The URI is deliberately absent: diagnostics must not read it. Java
        // reflection builds the real Media3 exception without Android Uri APIs.
        val stuck = HlsPlaylistTracker.PlaylistStuckException::class.java
            .getConstructor(android.net.Uri::class.java).newInstance(null)
        assertEquals(listOf("HlsPlaylistTracker.PlaylistStuckException"), liveTvDiagnosticCauseTypes(stuck))
        var deep: Throwable = IOException("secret tail")
        repeat(12) { deep = RuntimeException("secret depth $it", deep) }
        assertEquals(List(4) { "other" }, liveTvDiagnosticCauseTypes(deep))
        val cycle = object : Throwable("secret cycle") {
            override val cause: Throwable get() = this
        }
        assertEquals(listOf("other"), liveTvDiagnosticCauseTypes(cycle))
    }

    @Test fun attachmentClockAndOneTerminalObservationCannotLeakIntoSuccessor() {
        var now = 100L
        val old = LiveTvTerminalDiagnostics { now }
        now = 120
        old.firstFrame()
        now = 130
        old.firstFrame() // A later surface's first-frame callback does not rewrite the first one.
        old.decoderSample(4, true)
        now = 140
        old.decoderSample(4, true) // Unchanged counters are not progress.
        val event = old.terminal(LiveTvTerminalTrigger.PLAYER_ENDED, LiveTvTerminalAction.STOPPED,
            145, false, Player.STATE_ENDED, -1)!!
        assertNull(event.ms) // Existing beacon ms means a duration, not a device timestamp.
        assertTrue(event.detail!!.contains("clock=er_ms t=145"))
        assertTrue(event.detail!!.contains("f=120"))
        assertTrue(event.detail!!.contains("s=140"))
        assertTrue(event.detail!!.contains("p=130"))
        assertTrue(event.detail!!.contains("pos=unknown"))
        assertNull(old.terminal(LiveTvTerminalTrigger.LEASE_ERROR, LiveTvTerminalAction.STOPPED,
            150, false, Player.STATE_IDLE, 0))

        now = 200
        val successor = LiveTvTerminalDiagnostics { now }.terminal(
            LiveTvTerminalTrigger.NO_PROGRESS, LiveTvTerminalAction.STOPPED,
            250, true, Player.STATE_BUFFERING, 0,
        )!!
        assertTrue(successor.detail!!.contains("a=200"))
        assertTrue(successor.detail!!.contains("f=unknown"))
        assertTrue(successor.detail!!.contains("p=unknown"))
        assertTrue(successor.detail!!.contains("frames=0"))
    }

    @Test fun diagnosticFieldsFitTheActualServerCapsWithoutLosingClockOrCause() {
        val diagnostic = LiveTvTerminalDiagnostics { Long.MAX_VALUE }
        diagnostic.firstFrame()
        diagnostic.decoderSample(Int.MAX_VALUE, true)
        val stuck = HlsPlaylistTracker.PlaylistStuckException::class.java
            .getConstructor(android.net.Uri::class.java).newInstance(null)
        val event = diagnostic.terminal(LiveTvTerminalTrigger.PLAYER_ERROR,
            LiveTvTerminalAction.COMPATIBILITY_RETRY, Long.MAX_VALUE, false, Int.MAX_VALUE,
            Long.MAX_VALUE, PlaybackException("secret", stuck, PlaybackException.ERROR_CODE_IO_UNSPECIFIED))!!
        assertTrue(event.message.length <= 200)
        assertTrue(event.detail!!.length <= 200)
        assertTrue(event.message.contains("known_type=HlsPlaylistTracker.PlaylistStuckException"))
        assertTrue(event.detail!!.contains("t=${Long.MAX_VALUE}"))
        assertTrue(event.detail!!.endsWith("pos=${Long.MAX_VALUE}"))
    }
}
