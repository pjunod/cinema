package tv.plurx.profile

import android.content.pm.ApplicationInfo
import android.os.Build
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import androidx.test.uiautomator.Until
import java.security.MessageDigest
import java.io.File

internal const val PACKAGE = "tv.plurx.app"
internal const val TIMEOUT_MS = 60_000L

/** A normal, pre-paired app session; the harness never reads or supplies tokens. */
internal class ReleaseJourney(capture: Boolean = false) {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val args = InstrumentationRegistry.getArguments()
    val device = UiDevice.getInstance(instrumentation)
    private val title = requireNotNull(args.getString("plurxFixtureTitle")) {
        "Supply the exact observed controlled movie title, already visible on Home"
    }.also { require(it.isNotBlank()) }
    val sourceSha = requireNotNull(args.getString("plurxSourceSha")).also {
        require(it.matches(Regex("[0-9a-f]{40}"))) { "Expected full source SHA" }
    }
    val deviceAlias = requireNotNull(args.getString("plurxDeviceAlias")).also {
        require(it.matches(Regex("[A-Za-z0-9_-]{1,64}"))) { "Use a non-secret device alias" }
    }
    private val info = instrumentation.context.packageManager.getPackageInfo(PACKAGE, 0)
    val versionCode = info.longVersionCode
    val apkSha256 = MessageDigest.getInstance("SHA-256").let { digest ->
        File(requireNotNull(info.applicationInfo).sourceDir).inputStream().use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) { val n = input.read(buffer); if (n < 0) break; digest.update(buffer, 0, n) }
        }
        digest.digest().joinToString("") { "%02x".format(it) }
    }

    init {
        require(info.versionName.orEmpty().endsWith("-profile-capture") == capture) {
            "Capture requires source-name profileCapture; measurements require the optimized release"
        }
        require(requireNotNull(info.applicationInfo).flags and ApplicationInfo.FLAG_DEBUGGABLE == 0) {
            "Baseline capture and measurements require the non-debuggable release target"
        }
        require(versionCode == requireNotNull(args.getString("plurxVersionCode")).toLong()) {
            "Installed release counter does not match the controlled artifact"
        }
        require(apkSha256 == args.getString("plurxApkSha256")) { "Installed APK hash mismatch" }
        require(!Build.FINGERPRINT.startsWith("generic") && !Build.MODEL.contains("sdk_gphone")) {
            "D-03 acceptance requires physical hardware"
        }
    }

    /** Returns the production Media3 callback TTFF, not a UI polling estimate. */
    fun homeDetailPlayFirstFrame(): Long {
        check(device.wait(Until.hasObject(By.res("plurx-home-ready")), TIMEOUT_MS)) {
            "No authenticated populated Home; pair the dedicated fixture account through normal UI first"
        }
        val matches = device.findObjects(By.text(title))
        check(matches.size == 1) { "Controlled Home title absent or ambiguous" }
        clickAncestor(matches.single())
        val play = device.wait(Until.findObject(By.text(java.util.regex.Pattern.compile("\\s*(Play|Resume.*)"))), TIMEOUT_MS)
            ?: error("Observed title Detail has no video Play action")
        check(device.hasObject(By.text(title))) { "Detail title differs from controlled fixture" }
        // Prior iterations can save progress. The same movie must start at zero.
        val startOver = device.findObject(By.text("  Start over"))
            ?: device.findObject(By.text("Start over"))
        clickAncestor(startOver ?: play)
        val frame = device.wait(Until.findObject(By.res(java.util.regex.Pattern.compile("plurx-first-frame-[0-9]+"))), TIMEOUT_MS)
            ?: error("No actual Media3 first-frame callback; loading/ready/clock movement is insufficient")
        return requireNotNull(frame.resourceName).substringAfterLast("plurx-first-frame-").toLong()
    }

    private fun clickAncestor(object2: UiObject2) {
        var node = object2
        repeat(12) {
            if (node.isClickable) { node.click(); return }
            node = node.parent ?: error("Observed action has no clickable owner")
        }
        error("Observed action ownership exceeded bound")
    }

    fun closeOwnedPlayback() {
        // Back releases the normal controller; Home avoids leaving a test stream.
        if (device.currentPackageName == PACKAGE) {
            device.pressBack()
            device.pressHome()
        }
    }
}
