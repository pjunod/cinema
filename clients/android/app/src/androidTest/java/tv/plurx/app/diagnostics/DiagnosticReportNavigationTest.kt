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
        compose.onNodeWithText("Recovered local report", substring = true).assertIsDisplayed()
        compose.runOnIdle {
            assertEquals(original.id, DiagnosticRuntime.state.value.draft?.id)
            assertTrue(DiagnosticRuntime.state.value.pending.any { it.id == original.id })
        }
        compose.onNodeWithText("Send report").performScrollTo().assertIsNotEnabled()
        compose.onNodeWithText(resumeLabel).performScrollTo().assertIsEnabled()
    }
}
