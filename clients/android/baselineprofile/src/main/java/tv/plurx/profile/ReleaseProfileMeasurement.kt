package tv.plurx.profile

import android.os.Build
import androidx.benchmark.macro.BaselineProfileMode
import androidx.benchmark.macro.CompilationMode
import androidx.benchmark.macro.StartupMode
import androidx.benchmark.macro.StartupTimingMetric
import androidx.benchmark.macro.junit4.MacrobenchmarkRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class ReleaseProfileMeasurement {
    @get:Rule val benchmark = MacrobenchmarkRule()

    @Test fun withoutProfileFiveColdIterations() = measure("none", CompilationMode.None())
    @Test fun requiredProfileFiveColdIterations() = measure(
        "required", CompilationMode.Partial(baselineProfileMode = BaselineProfileMode.Require, warmupIterations = 0),
    )

    private fun measure(mode: String, compilation: CompilationMode) {
        val journey = ReleaseJourney()
        val ttff = mutableListOf<Long>()
        try {
            benchmark.measureRepeated(
                packageName = PACKAGE,
                metrics = listOf(StartupTimingMetric()),
                compilationMode = compilation,
                startupMode = StartupMode.COLD,
                iterations = 5,
                setupBlock = { pressHome() },
            ) {
                startActivityAndWait()
                ttff += journey.homeDetailPlayFirstFrame()
                journey.closeOwnedPlayback()
            }
            check(ttff.size == 5) { "Incomplete five-iteration measurement" }
            val artifact = JSONObject()
                .put("schema", "plurx_release_profile_ttff_v1")
                .put("source_sha", journey.sourceSha)
                .put("apk_sha256", journey.apkSha256)
                .put("package", PACKAGE).put("version_code", journey.versionCode)
                .put("device_alias", journey.deviceAlias)
                .put("device_model", Build.MODEL).put("sdk", Build.VERSION.SDK_INT)
                .put("compilation_mode", mode).put("startup_mode", "COLD")
                .put("iterations", 5).put("media3_ttff_ms", JSONArray(ttff))
                .put("media3_ttff_median_ms", ttff.sorted()[2])
                .put("first_frame_boundary", "PlayerScreen attempt opened to Media3 onRenderedFirstFrame; excludes Home and Detail navigation")
                .put("startup_metric", "Use the matching Macrobenchmark JSON timeToInitialDisplayMs runs and Perfetto traces")
            val output = requireNotNull(InstrumentationRegistry.getInstrumentation().context.getExternalFilesDir(null))
            File(output, "plurx-release-profile-$mode-ttff.json").writeText(artifact.toString(2))
        } finally {
            journey.closeOwnedPlayback()
        }
    }
}
