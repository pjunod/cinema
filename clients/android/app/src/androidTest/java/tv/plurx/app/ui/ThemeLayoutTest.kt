package tv.plurx.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performScrollTo
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Appearance
import tv.plurx.app.data.Item
import tv.plurx.app.data.ThemeId
import tv.plurx.app.ui.components.PosterCard
import tv.plurx.app.ui.theme.PlurxTheme

class ThemeLayoutTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun everyThemeRendersCoreControls() {
        compose.setContent {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                ThemeId.entries.forEach { theme ->
                    val title = "Alignment check — ${theme.label}"
                    PlurxTheme(theme, Appearance.Dark) {
                        Column {
                            Text(theme.label)
                            PosterCard(Item(id = 1, kind = "movie", title = title, year = 2026)) {}
                        }
                    }
                }
            }
        }
        ThemeId.entries.forEach { theme ->
            compose.onNodeWithText(theme.label).performScrollTo().assertIsDisplayed()
            compose.onNodeWithText("Alignment check — ${theme.label}").performScrollTo().assertIsDisplayed()
        }
    }
}
