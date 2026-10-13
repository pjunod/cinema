package tv.plurx.app.diagnostics

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
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
        compose.onNodeWithText("Send report").performScrollTo().assertIsNotEnabled()
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

}
