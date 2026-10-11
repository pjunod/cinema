package tv.plurx.app.ui

import android.content.Intent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import tv.plurx.app.MainActivity
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(AndroidJUnit4::class)
class ConnectScreenStartupTest {
    @Test fun connectionScreenOpensWithoutStartingQrScanner() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val intent = Intent(instrumentation.targetContext, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val activity = instrumentation.startActivitySync(intent)
        instrumentation.waitForIdleSync()
        val nextFrame = CountDownLatch(1)
        activity.window.decorView.postDelayed({ nextFrame.countDown() }, 500)

        assertTrue(nextFrame.await(5, TimeUnit.SECONDS))
        assertFalse(activity.isFinishing)
        assertFalse(activity.isDestroyed)
    }
}
