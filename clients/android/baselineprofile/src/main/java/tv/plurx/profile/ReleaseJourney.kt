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
    private val playerSelector = By.pkg(PACKAGE).res(java.util.regex.Pattern.compile("plurx-first-frame-(pending|[0-9]+)"))
    private val titleSelector = By.pkg(PACKAGE).text(title)
    private val playSelector = By.pkg(PACKAGE).text(java.util.regex.Pattern.compile("\\s*(Play|Resume.*)"))
    private var ownsPlayback = false
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
        check(!ownsPlayback && !device.hasObject(playerSelector)) { "Previous or unowned player is still mounted" }
        check(device.wait(Until.hasObject(By.res("plurx-home-ready")), TIMEOUT_MS)) {
            "No authenticated populated Home; pair the dedicated fixture account through normal UI first"
        }
        val matches = device.findObjects(titleSelector)
        check(matches.size == 1) { "Controlled Home title absent or ambiguous" }
        clickAncestor(matches.single())
        val play = device.wait(Until.findObject(playSelector), TIMEOUT_MS)
            ?: error("Observed title Detail has no video Play action")
        check(isOwnedDetail()) { "Detail differs from the controlled fixture destination" }
        // Prior iterations can save progress. The same movie must start at zero.
        val startOver = device.findObject(By.text("  Start over"))
            ?: device.findObject(By.text("Start over"))
        clickAncestor(startOver ?: play)
        ownsPlayback = true
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
        if (!ownsPlayback) return
        // A first Back can dismiss transport chrome. Stay in this owned app and
        // repeat only while its production player marker remains mounted.
        repeat(4) {
            check(device.currentPackageName == PACKAGE) { "Owned cleanup left the target app" }
            if (device.hasObject(playerSelector)) {
                check(device.pressBack()) { "Owned normal Back was not dispatched" }
                device.wait(Until.gone(playerSelector), 1_500L)
            }
        }
        check(device.wait(Until.gone(playerSelector), 5_000L)) { "Owned player did not exit within the cleanup bound" }
        // Player absence alone (or the launcher) is insufficient. Confirm the
        // exact fixture's known Detail title/action with no Home marker first.
        check(device.wait(Until.hasObject(titleSelector), 5_000L) && isOwnedDetail()) {
            "Owned player exit did not return to the controlled fixture Detail"
        }
        check(device.pressHome()) { "Home was not dispatched after confirmed owned exit" }
        ownsPlayback = false
    }

    private fun isOwnedDetail(): Boolean =
        device.currentPackageName == PACKAGE &&
            !device.hasObject(playerSelector) &&
            !device.hasObject(By.res("plurx-home-ready")) &&
            !device.hasObject(By.res("plurx-home-loading")) &&
            device.findObjects(titleSelector).size == 1 && device.hasObject(playSelector)
}
