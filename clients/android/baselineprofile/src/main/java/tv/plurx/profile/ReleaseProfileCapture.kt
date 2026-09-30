package tv.plurx.profile

import androidx.benchmark.macro.junit4.BaselineProfileRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ReleaseProfileCapture {
    @get:Rule val profile = BaselineProfileRule()

    @Test fun coldHomeDetailPlayFirstFrame() {
        val journey = ReleaseJourney(capture = true)
        try {
            profile.collect(
                packageName = PACKAGE, includeInStartupProfile = false,
                maxIterations = 15, stableIterations = 3, strictStability = true,
                filterPredicate = { rule -> rule.contains("Ltv/plurx/app/") },
            ) {
                pressHome()
                startActivityAndWait()
                journey.homeDetailPlayFirstFrame()
                journey.closeOwnedPlayback()
            }
        } finally {
            journey.closeOwnedPlayback()
        }
    }
}
