package tv.plurx.app.ui

import android.app.Application
import android.content.pm.PackageManager
import android.graphics.Bitmap
import androidx.core.content.FileProvider
import java.io.File
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import tv.plurx.app.R

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = Application::class)
class SharingInvitationFileProviderTest {
    @Test fun sharingPngAndDiagnosticsZipResolveThroughIndependentProvidersAndPaths() {
        val context = RuntimeEnvironment.getApplication()
        val sharingAuthority = context.packageName + ".sharing-files"
        val diagnosticsAuthority = context.packageName + ".diagnostics-files"
        val sharing = context.packageManager.resolveContentProvider(sharingAuthority, PackageManager.GET_META_DATA)
        val diagnostics = context.packageManager.resolveContentProvider(diagnosticsAuthority, PackageManager.GET_META_DATA)
        assertNotNull("Sharing provider missing from the merged manifest", sharing)
        assertNotNull("Existing diagnostics provider missing from the merged manifest", diagnostics)
        assertNotEquals("Providers must have independent manifest identities", sharing!!.name, diagnostics!!.name)
        assertEquals(SharingInvitationFileProvider::class.java.name, sharing.name)
        assertEquals(FileProvider::class.java.name, diagnostics.name)
        assertEquals(R.xml.sharing_invitation_paths, sharing.metaData.getInt("android.support.FILE_PROVIDER_PATHS"))
        assertEquals(R.xml.diagnostic_share_paths, diagnostics.metaData.getInt("android.support.FILE_PROVIDER_PATHS"))
        assertFalse(sharing.exported); assertTrue(sharing.grantUriPermissions)
        assertFalse(diagnostics.exported); assertTrue(diagnostics.grantUriPermissions)
        val png = File(context.cacheDir, "sharing-invitations/test/cinema-sharing-invitation.png")
        val report = File(context.cacheDir, "diagnostics-share/test-report.zip")
        try {
            png.parentFile!!.mkdirs()
            val bitmap = sharingInvitationBitmap("cinema-share-v1:Zml4dHVyZQ")
            try { png.outputStream().use { assertTrue(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)) } }
            finally { bitmap.recycle() }
            val sharingUri = FileProvider.getUriForFile(context, sharingAuthority, png)
            assertEquals(sharingAuthority, sharingUri.authority)
            assertTrue(sharingUri.path!!.startsWith("/sharing-invitations/"))
            assertEquals("image/png", context.contentResolver.getType(sharingUri))
            val sharedBytes = context.contentResolver.openInputStream(sharingUri)!!.use { it.readBytes() }
            assertArrayEquals(png.readBytes(), sharedBytes)
            assertArrayEquals(byteArrayOf(-119, 80, 78, 71, 13, 10, 26, 10), sharedBytes.take(8).toByteArray())
            report.parentFile!!.mkdirs()
            ZipOutputStream(report.outputStream()).use { zip -> zip.putNextEntry(ZipEntry("report.txt")); zip.write("fixture".toByteArray()); zip.closeEntry() }
            val diagnosticsUri = FileProvider.getUriForFile(context, diagnosticsAuthority, report)
            assertEquals(diagnosticsAuthority, diagnosticsUri.authority)
            assertTrue(diagnosticsUri.path!!.startsWith("/diagnostic-reports/"))
            assertEquals("application/zip", context.contentResolver.getType(diagnosticsUri))
            assertArrayEquals(report.readBytes(), context.contentResolver.openInputStream(diagnosticsUri)!!.use { it.readBytes() })
            assertThrows(IllegalArgumentException::class.java) { FileProvider.getUriForFile(context, diagnosticsAuthority, png) }
            assertThrows(IllegalArgumentException::class.java) { FileProvider.getUriForFile(context, sharingAuthority, report) }
        } finally { png.delete(); report.delete() }
    }
}
