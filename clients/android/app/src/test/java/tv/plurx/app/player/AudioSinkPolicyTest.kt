package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Test

class AudioSinkPolicyTest {
    @Test
    fun media3AudioTrackCodesAreClassifiedWithoutBroadeningCodecFailures() {
        assertEquals(AudioSinkFailure.InitFailed, audioSinkFailure(5001))
        assertEquals(AudioSinkFailure.WriteFailed, audioSinkFailure(5002))
        assertEquals(AudioSinkFailure.WriteFailed, audioSinkFailure(5003))
        assertEquals(AudioSinkFailure.OffloadInitFailed, audioSinkFailure(5004))
        assertEquals(AudioSinkFailure.None, audioSinkFailure(4001))
        assertEquals(AudioSinkFailure.None, audioSinkFailure(5005))
    }

    @Test
    fun absentOutputFailsWithoutSpendingEitherRecovery() {
        assertEquals(
            AudioSinkRecovery.FailDisconnected,
            action(AudioSinkFailure.InitFailed, present = false, changed = true),
        )
    }

    @Test
    fun changedRouteGetsOneNewDecisionBeforeAnyTranscodeRescue() {
        assertEquals(
            AudioSinkRecovery.ReSnapshotAndRetry,
            action(AudioSinkFailure.WriteFailed, changed = true),
        )
        assertEquals(
            AudioSinkRecovery.TranscodeRescue,
            action(AudioSinkFailure.InitFailed, changed = true, routeRetryUsed = true),
        )
        assertEquals(
            AudioSinkRecovery.Fail,
            action(AudioSinkFailure.InitFailed, changed = true, routeRetryUsed = true,
                transcodeUsed = true),
        )
    }

    @Test
    fun unchangedRouteRescuesInitializationOnceAndNeverRetriesAnEstablishedWrite() {
        assertEquals(AudioSinkRecovery.TranscodeRescue, action(AudioSinkFailure.InitFailed))
        assertEquals(AudioSinkRecovery.TranscodeRescue, action(AudioSinkFailure.OffloadInitFailed))
        assertEquals(AudioSinkRecovery.Fail, action(AudioSinkFailure.InitFailed, transcodeUsed = true))
        assertEquals(AudioSinkRecovery.Fail, action(AudioSinkFailure.WriteFailed))
        assertEquals(AudioSinkRecovery.Fail, action(AudioSinkFailure.None))
    }

    private fun action(
        failure: AudioSinkFailure,
        present: Boolean = true,
        changed: Boolean = false,
        routeRetryUsed: Boolean = false,
        transcodeUsed: Boolean = false,
    ): AudioSinkRecovery = audioSinkAction(
        failure = failure,
        outputDevicePresent = present,
        routeChangedSinceSnapshot = changed,
        sinkRetryUsed = routeRetryUsed,
        transcodeRescueAlreadyUsed = transcodeUsed,
    )
}
