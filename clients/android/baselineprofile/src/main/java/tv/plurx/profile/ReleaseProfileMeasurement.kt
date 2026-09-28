package tv.plurx.profile

import android.os.Build
import androidx.benchmark.macro.BaselineProfileMode
import androidx.benchmark.macro.CompilationMode
import androidx.benchmark.macro.StartupMode
import androidx.benchmark.macro.StartupTimingMetric
import androidx.benchmark.macro.junit4.MacrobenchmarkRule
import android.annotation.SuppressLint
import androidx.benchmark.Outputs
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONArray
import org.json.JSONObject
import org.junit.AfterClass
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import java.util.Base64
import org.junit.runners.Parameterized

@RunWith(Parameterized::class)
class ReleaseProfileMeasurement(private val runId: String, private val binding: String) {
    companion object {
        // Generated inside this instrumentation process, never an operator-supplied sidecar label.
        private val invocation = UUID.randomUUID().toString().replace("-", "")
        private val artifacts = mutableMapOf<String, JSONObject>()

        private fun identityBytes(journey: ReleaseJourney): ByteArray =
            listOf(journey.sourceSha, journey.apkSha256, PACKAGE, journey.versionCode.toString(),
                journey.deviceAlias, Build.MODEL, Build.VERSION.SDK_INT.toString(), Build.FINGERPRINT, Build.DEVICE)
                .joinToString("") { "${it.toByteArray(Charsets.UTF_8).size}:$it" }.toByteArray(Charsets.UTF_8)

        // Full 256 bits, compact enough for AndroidX's <200-character trace filenames.
        private fun bindingFor(journey: ReleaseJourney): String = Base64.getUrlEncoder().withoutPadding()
            .encodeToString(MessageDigest.getInstance("SHA-256").digest(identityBytes(journey)))

        private fun sha256(bytes: ByteArray) = MessageDigest.getInstance("SHA-256")
            .digest(bytes).joinToString("") { "%02x".format(it) }

        @JvmStatic @Parameterized.Parameters(name = "run={0},binding={1}")
        fun invocationParameters(): List<Array<String>> = listOf(arrayOf(invocation, bindingFor(ReleaseJourney())))

        @SuppressLint("RestrictedApi")
        @JvmStatic @AfterClass fun sealCompletedInvocation() {
            check(artifacts.keys == setOf("none", "required")) { "Incomplete paired invocation; retain failure outputs" }
            // AndroidX writes this original file synchronously before measureRepeated returns.
            // Read its actual bytes only after both methods, then retain an immutable run-named copy.
            val context = InstrumentationRegistry.getInstrumentation().targetContext
            val original = File(Outputs.outputDirectory, "${context.packageName}-benchmarkData.json")
            val bytes = original.readBytes()
            val document = JSONObject(bytes.toString(Charsets.UTF_8))
            val results = document.getJSONArray("benchmarks")
            check(results.length() == 2) { "Expected only this invocation's two benchmark methods" }
            val emittedBinding = artifacts.getValue("none").getString("benchmark_binding")
            val names = setOf("withoutProfileFiveColdIterations", "requiredProfileFiveColdIterations")
            val observed = mutableSetOf<String>()
            repeat(results.length()) { index ->
                val result = results.getJSONObject(index)
                val method = result.getString("name").substringBefore('[')
                check(method in names && observed.add(method)) { "Missing or repeated benchmark mode" }
                check(result.getString("className") == ReleaseProfileMeasurement::class.java.name)
                check(result.getString("name") == "$method[run=$invocation,binding=$emittedBinding]")
                val params = result.getJSONObject("params")
                check(params.getString("run") == invocation && params.getString("binding") == emittedBinding)
            }
            val snapshotName = "plurx-$invocation-macrobenchmark.json"
            Outputs.writeFile(snapshotName) { file ->
                check(!file.exists()) { "Refuse invocation snapshot overwrite" }
                file.writeBytes(bytes)
            }
            artifacts.forEach { (mode, artifact) ->
                artifact.put("macrobenchmark_sha256", sha256(bytes)).put("macrobenchmark_file", snapshotName)
                Outputs.writeFile("plurx-$invocation-$mode-ttff.json") { file ->
                    check(!file.exists()) { "Refuse invocation TTFF overwrite" }
                    file.writeText(artifact.toString(2))
                }
            }
        }
    }
    @get:Rule val benchmark = MacrobenchmarkRule()

    @Test fun withoutProfileFiveColdIterations() = measure("none", CompilationMode.None())
    @Test fun requiredProfileFiveColdIterations() = measure(
        "required", CompilationMode.Partial(baselineProfileMode = BaselineProfileMode.Require, warmupIterations = 0),
    )

    private fun measure(mode: String, compilation: CompilationMode) {
        val journey = ReleaseJourney()
        check(runId == invocation && binding == bindingFor(journey)) { "Invocation target identity changed" }
        val ttff = mutableListOf<Long>()
        val artifact = try {
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
                .put("schema", "plurx_release_profile_ttff_v2")
                .put("run_id", runId).put("invocation_binding_sha256", sha256(identityBytes(journey)))
                .put("benchmark_binding", binding)
                .put("source_sha", journey.sourceSha)
                .put("apk_sha256", journey.apkSha256)
                .put("package", PACKAGE).put("version_code", journey.versionCode)
                .put("device_alias", journey.deviceAlias)
                .put("device_model", Build.MODEL).put("sdk", Build.VERSION.SDK_INT)
                .put("device_fingerprint", Build.FINGERPRINT).put("device_build_device", Build.DEVICE)
                .put("compilation_mode", mode).put("startup_mode", "COLD")
                .put("iterations", 5).put("media3_ttff_ms", JSONArray(ttff))
                .put("media3_ttff_median_ms", ttff.sorted()[2])
                .put("first_frame_boundary", "PlayerScreen attempt opened to Media3 onRenderedFirstFrame; excludes Home and Detail navigation")
                .put("startup_metric", "Use the matching Macrobenchmark JSON timeToInitialDisplayMs runs and Perfetto traces")
            artifact
        } finally {
            journey.closeOwnedPlayback()
        }
        check(artifacts.put(mode, artifact) == null) { "Repeated mode in one invocation" }
    }
}
