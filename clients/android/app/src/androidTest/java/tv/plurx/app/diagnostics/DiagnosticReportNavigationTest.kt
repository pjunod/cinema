package tv.plurx.app.diagnostics

import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.junit.Assert.assertTrue
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.MainActivity

/** Runs against an anonymous installation on the qualification-owned emulator. */
class DiagnosticReportNavigationTest {
    @get:Rule
    val compose = createAndroidComposeRule<MainActivity>()

    @Test
    fun anonymousReportButtonClearsSystemNavigationAndOpensReport() {
        val report = compose.onNodeWithTag("diagnostics-open").assertIsDisplayed()
        val bounds = report.fetchSemanticsNode().boundsInWindow
        compose.runOnIdle {
            val decor = compose.activity.window.decorView
            val navigation = requireNotNull(ViewCompat.getRootWindowInsets(decor))
                .getInsets(WindowInsetsCompat.Type.navigationBars())
            assertTrue("Report action must clear the bottom system navigation area", bounds.bottom <= decor.height - navigation.bottom)
            assertTrue("Report action must clear the side system navigation area", bounds.right <= decor.width - navigation.right)
        }
        report.performClick()
        compose.onNodeWithTag("diagnostics-status").assertIsDisplayed()
    }

    @Test
    fun recoveredIncidentKeepsHistoryAndRequiresUploadConsent() {
        compose.onNodeWithTag("diagnostics-open").performClick()
        compose.waitUntil(timeoutMillis = 20_000) {
            DiagnosticRuntime.state.value.draft != null && !DiagnosticRuntime.state.value.busy
        }
        lateinit var original: DiagnosticDraft
        compose.runOnIdle {
            original = requireNotNull(DiagnosticRuntime.state.value.draft)
            assertEquals("anonymous", original.capture.namespace)
        }
        compose.onNodeWithText("Close").performClick()
        compose.activityRule.scenario.recreate()
        compose.waitUntil(timeoutMillis = 20_000) {
            DiagnosticRuntime.state.value.pending.any { it.id == original.id }
        }
        compose.onNodeWithTag("diagnostics-open").performClick()
        compose.waitUntil(timeoutMillis = 20_000) { !DiagnosticRuntime.state.value.busy }
        val resumeLabel = "Resume ${original.capture.capturedAt} · ${original.state}"
        compose.onNodeWithText(resumeLabel).performScrollTo().performClick()
        compose.waitUntil(timeoutMillis = 20_000) { !DiagnosticRuntime.state.value.busy }
        compose.onNodeWithText("Recovered local report", substring = true).assertIsDisplayed()
        compose.runOnIdle {
            assertEquals(original.id, DiagnosticRuntime.state.value.draft?.id)
            assertTrue(DiagnosticRuntime.state.value.pending.any { it.id == original.id })
        }
        compose.onNodeWithText("Send report").assertDoesNotExist()
        compose.onNodeWithText(resumeLabel).performScrollTo().assertIsEnabled()
    }
    @Test
    fun staleHistoryKeepsFrozenPrivacyAndDeletingSelectedReportDoesNotRecoverIt() {
        compose.runOnIdle {
            val auth=tv.plurx.app.data.Session.playbackAuthorization()
            assertEquals(null,auth.token)
            assertTrue("Qualification must not contact a real Cinema endpoint",auth.origin.isBlank() || java.net.URI(auth.origin).host in setOf("localhost","127.0.0.1","10.0.2.2"))
            assertEquals(null,DiagnosticSecrets(compose.activity,"anonymous").read("enrollment"))
            DiagnosticRuntime.open(positiveTitle="Explicit private qualification title")
        }
        compose.waitUntil(timeoutMillis=20_000) { DiagnosticRuntime.state.value.draft != null && !DiagnosticRuntime.state.value.busy }
        lateinit var stale: DiagnosticDraft
        compose.runOnIdle {
            stale=requireNotNull(DiagnosticRuntime.state.value.draft)
            DiagnosticRuntime.destination("https://support.example")
            DiagnosticRuntime.includeTitle(true); DiagnosticRuntime.includePlatform(true); DiagnosticRuntime.consent(true)
            // The anonymous offline Cinema endpoint refuses the grant. The
            // immutable archive still exists before that transport failure.
            DiagnosticRuntime.send(phone=true)
        }
        compose.waitUntil(timeoutMillis=20_000) { !DiagnosticRuntime.state.value.busy }
        lateinit var frozen: DiagnosticDraft
        compose.runOnIdle {
            frozen=requireNotNull(DiagnosticRuntime.state.value.draft)
            assertTrue(frozen.consented); assertTrue(frozen.binding != null)
            DiagnosticRuntime.resume(stale)
        }
        compose.waitUntil(timeoutMillis=20_000) { !DiagnosticRuntime.state.value.busy }
        compose.runOnIdle {
            val value=DiagnosticRuntime.state.value
            assertEquals(frozen.binding,value.draft?.binding); assertEquals(frozen.origin,value.destination)
            assertTrue(value.consent); assertTrue(value.includeTitle); assertTrue(value.includePlatform)
            assertEquals(frozen.binding,value.pending.first { it.id == stale.id }.binding)
            DiagnosticRuntime.deleteLocal(stale)
            DiagnosticRuntime.close()
            assertEquals(null,DiagnosticRuntime.state.value.draft)
        }
        compose.waitUntil(timeoutMillis=20_000) { DiagnosticRuntime.state.value.message == "Local report deleted." }
        compose.activityRule.scenario.recreate()
        compose.waitUntil(timeoutMillis=20_000) { !DiagnosticRuntime.state.value.busy }
        compose.runOnIdle { assertTrue(DiagnosticRuntime.state.value.pending.none { it.id == stale.id }) }
        assertTrue(!java.io.File(compose.activity.noBackupFilesDir,"cinema-diagnostics/anonymous/${stale.id}.json").exists())
    }

    @Test
    fun exportedZipCanBeAttachedWithoutSupportEnrollment() {
        org.junit.Assume.assumeFalse(tv.plurx.app.player.isTelevision(compose.activity))
        val instrumentation=androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
        val chooser=java.util.concurrent.atomic.AtomicReference<android.content.Intent>()
        val monitor=object : android.app.Instrumentation.ActivityMonitor() {
            override fun onStartActivity(intent: android.content.Intent): android.app.Instrumentation.ActivityResult? {
                if (intent.action != android.content.Intent.ACTION_CHOOSER) return null
                chooser.set(intent)
                return android.app.Instrumentation.ActivityResult(android.app.Activity.RESULT_CANCELED,null)
            }
        }
        instrumentation.addMonitor(monitor)
        try {
            compose.runOnIdle { DiagnosticRuntime.open() }
            compose.waitUntil(timeoutMillis=20_000) { DiagnosticRuntime.state.value.draft != null && !DiagnosticRuntime.state.value.busy }
            lateinit var id: String
            compose.runOnIdle {
                DiagnosticRuntime.edit(description="Cannot sign in; expected the library")
                id=requireNotNull(DiagnosticRuntime.state.value.draft).id
                assertTrue(DiagnosticRuntime.state.value.destination.isBlank())
                assertTrue(!DiagnosticRuntime.state.value.consent)
            }
            compose.onNodeWithTag("diagnostics-export").performScrollTo().performClick()
            compose.waitUntil(timeoutMillis=20_000) { chooser.get() != null && !DiagnosticRuntime.state.value.busy }
            val share=requireNotNull(chooser.get().getParcelableExtra(android.content.Intent.EXTRA_INTENT,android.content.Intent::class.java))
            assertEquals(android.content.Intent.ACTION_SEND,share.action); assertEquals("application/zip",share.type)
            assertTrue(share.flags and android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
            val uri=requireNotNull(share.getParcelableExtra(android.content.Intent.EXTRA_STREAM,android.net.Uri::class.java))
            val entries=mutableMapOf<String,ByteArray>()
            java.util.zip.ZipInputStream(requireNotNull(compose.activity.contentResolver.openInputStream(uri))).use { zip ->
                while (true) {
                    val entry=zip.nextEntry ?: break
                    assertTrue(!entry.name.contains("..")); assertTrue(!entries.containsKey(entry.name))
                    entries[entry.name]=zip.readBytes()
                }
            }
            val manifest=kotlinx.serialization.json.Json.parseToJsonElement(requireNotNull(entries["manifest.json"]).decodeToString()).toString()
            assertTrue(manifest.contains(id)); assertTrue(manifest.contains("anonymous"))
            assertEquals("Cannot sign in; expected the library",requireNotNull(entries["description.txt"]).decodeToString())
            assertTrue(requireNotNull(entries["summary.txt"]).decodeToString().contains(id))
            assertTrue(entries.containsKey("client-events.jsonl"))
            compose.runOnIdle {
                val value=DiagnosticRuntime.state.value
                assertEquals(id,value.draft?.id); assertTrue(value.draft?.consented == false)
                assertEquals(null,value.draft?.binding); assertEquals(null,value.draft?.origin)
                assertEquals(null,DiagnosticSecrets(compose.activity,"anonymous").read("enrollment"))
            }
        } finally { instrumentation.removeMonitor(monitor) }
    }

    @Test
    fun televisionDescriptionDpadReachesPrivacyWithoutStartingUpload() {
        org.junit.Assume.assumeTrue(tv.plurx.app.player.isTelevision(compose.activity))
        compose.runOnIdle { DiagnosticRuntime.open() }
        compose.waitUntil(timeoutMillis=20_000) { DiagnosticRuntime.state.value.draft != null && !DiagnosticRuntime.state.value.busy }
        val description=compose.onNodeWithTag("diagnostics-description")
        description.performScrollTo()
        // Keep the text caret animation from driving Compose's virtual clock
        // indefinitely while asserting actual framework focus/key dispatch.
        compose.mainClock.autoAdvance=false
        description.performSemanticsAction(SemanticsActions.RequestFocus)
        compose.mainClock.advanceTimeByFrame()
        description.assertIsFocused()
        description.performKeyInput { pressKey(Key.DirectionDown) }
        compose.mainClock.advanceTimeByFrame()
        compose.onNodeWithTag("diagnostics-title-consent").assertIsFocused()
        compose.runOnIdle {
            assertTrue(!DiagnosticRuntime.state.value.consent)
            assertTrue(DiagnosticRuntime.state.value.draft?.consented == false)
        }
        compose.onNodeWithText("Send report").assertDoesNotExist()
    }

}
