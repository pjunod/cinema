package tv.plurx.app.diagnostics

import androidx.profileinstaller.ProfileVerifier
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.CancellationException
import java.util.concurrent.TimeUnit

internal data class ReleaseProfileFacts(
    val resultCode: Int,
    val embedded: Boolean,
    val compiled: Boolean,
    val queued: Boolean,
)

internal fun releaseProfileSummary(facts: ReleaseProfileFacts): String = buildString {
    append("Bundled profile: ").append(if (facts.embedded) "present" else "not detected")
    append(". ART profile: ")
    append(when {
        facts.resultCode == ProfileVerifier.CompilationStatus.RESULT_CODE_COMPILED_WITH_PROFILE_NON_MATCHING -> "compiled with a different profile"
        facts.compiled -> "compiled with a profile"
        facts.queued -> "queued for compilation"
        facts.resultCode == ProfileVerifier.CompilationStatus.RESULT_CODE_NO_PROFILE_INSTALLED -> "not installed"
        else -> "unavailable (result ${facts.resultCode})"
    })
    append(". This does not prove the app journey was consumed or a measured startup gain.")
}

/** Bounded advisory read; no compilation, broadcast or feature enablement. */
internal suspend fun readReleaseProfileStatus(): String = withContext(Dispatchers.IO) {
    try {
        val status = ProfileVerifier.getCompilationStatusAsync().get(3, TimeUnit.SECONDS)
        releaseProfileSummary(ReleaseProfileFacts(
            status.profileInstallResultCode,
            status.appApkHasEmbeddedProfile(),
            status.isCompiledWithProfile,
            status.hasProfileEnqueuedForCompilation(),
        ))
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        "Profile verification unavailable. Bundled assets and ART consumption are separate; no startup gain has been measured here."
    }
}
