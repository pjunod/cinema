package tv.plurx.app.diagnostics

import androidx.profileinstaller.ProfileVerifier
import org.junit.Assert.assertTrue
import org.junit.Test

class ReleaseProfileStatusTest {
    @Test fun compiledDifferentProfileNeverClaimsMeasuredAppJourney() {
        val label = releaseProfileSummary(ReleaseProfileFacts(
            ProfileVerifier.CompilationStatus.RESULT_CODE_COMPILED_WITH_PROFILE_NON_MATCHING,
            embedded = true, compiled = true, queued = true,
        ))
        assertTrue(label.contains("compiled with a different profile"))
        assertTrue(label.contains("does not prove the app journey was consumed"))
        assertTrue(label.contains("or a measured startup gain"))
    }

    @Test fun queuedAndMissingProfilesRemainDistinct() {
        val queued = releaseProfileSummary(ReleaseProfileFacts(
            ProfileVerifier.CompilationStatus.RESULT_CODE_PROFILE_ENQUEUED_FOR_COMPILATION,
            embedded = true, compiled = false, queued = true,
        ))
        val missing = releaseProfileSummary(ReleaseProfileFacts(
            ProfileVerifier.CompilationStatus.RESULT_CODE_NO_PROFILE_INSTALLED,
            embedded = false, compiled = false, queued = false,
        ))
        assertTrue(queued.contains("queued for compilation"))
        assertTrue(missing.contains("Bundled profile: not detected"))
        assertTrue(missing.contains("ART profile: not installed"))
    }
}
