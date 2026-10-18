package tv.plurx.app.data

import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

class MeridianPreferencesStoreTest {
    @Test fun reopenedStoreRetainsIndependentLayoutThemeAndSiblingFields() = runBlocking {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val store = SettingsStore(context)
        val original = store.flow.first().preferences
        try {
            val baseline = original.copy(posterSize = PosterSize.ExtraLarge, homeGrouping = HomeGrouping.Library, libraryPresentation = LibraryPresentation.Grid)
            for (layout in ViewerLayout.entries) for (theme in ThemeId.entries) for (appearance in Appearance.entries) {
                val expected = baseline.copy(layout = layout, theme = theme, appearance = appearance)
                store.saveViewerPreferences(expected)
                assertEquals(expected, SettingsStore(context).flow.first().preferences)
            }
        } finally { store.saveViewerPreferences(original) }
    }
}
