package tv.plurx.app.ui

import android.graphics.Bitmap
import android.graphics.pdf.PdfRenderer
import android.os.ParcelFileDescriptor
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import okhttp3.ResponseBody
import tv.plurx.app.data.Net
import tv.plurx.app.data.Session
import java.io.File
import kotlin.coroutines.coroutineContext

private const val MAX_PDF_BYTES = 1_073_741_824L

private class LocalPdf(val file: File) {
    private val descriptor = ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
    val renderer = try {
        PdfRenderer(descriptor).also { check(it.pageCount > 0) { "This PDF has no pages" } }
    } catch (failure: Exception) {
        descriptor.close()
        throw failure
    }
    val renderLock = Mutex()

    suspend fun close() {
        renderLock.withLock {
            renderer.close()
            descriptor.close()
            file.delete()
        }
    }
}

private suspend fun downloadPdf(cacheDir: File, fileId: Long): LocalPdf = withContext(Dispatchers.IO) {
    val origin = Session.canonicalPrimaryOrigin() ?: error("Connect to a server first")
    val token = Session.token ?: error("Sign in before reading this PDF")
    val staleBefore = System.currentTimeMillis() - 24L * 60 * 60 * 1000
    cacheDir.listFiles()
        ?.filter { it.name.startsWith("plurx-reader-") && it.name.endsWith(".pdf") && it.lastModified() < staleBefore }
        ?.forEach(File::delete)
    val temporary = File.createTempFile("plurx-reader-", ".pdf", cacheDir)
    try {
        // The bearer stays inside Plurx. This client refuses redirects, so
        // a server response cannot forward it to another host.
        val response = Net.api(origin, Net.profileClient(token)).bookContent(fileId)
        if (response.code() != 200) error("Server returned ${response.code()} for this PDF")
        val body: ResponseBody = response.body() ?: error("The server sent an empty PDF")
        body.use { content ->
            content.byteStream().use { input ->
                temporary.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024)
                    var count = 0L
                    while (true) {
                        coroutineContext.ensureActive()
                        val read = input.read(buffer)
                        if (read < 0) break
                        count += read
                        if (count > MAX_PDF_BYTES) error("This PDF exceeds Cinema's 1 GiB reader limit")
                        output.write(buffer, 0, read)
                    }
                }
            }
        }
        if (temporary.length() == 0L) error("The server sent an empty PDF")
        if (Session.canonicalPrimaryOrigin() != origin || Session.token != token) {
            error("The signed-in profile changed while opening this PDF")
        }
        LocalPdf(temporary)
    } catch (failure: Exception) {
        temporary.delete()
        throw failure
    }
}

@Composable
fun PdfReaderScreen(fileId: Long, onExit: () -> Unit) {
    val context = LocalContext.current
    val loaded by produceState<Result<LocalPdf>?>(null, fileId) {
        value = runCatching { downloadPdf(context.cacheDir, fileId) }
    }
    val pdf = loaded?.getOrNull()
    DisposableEffect(pdf) {
        onDispose {
            if (pdf != null) CoroutineScope(Dispatchers.IO).launch { pdf.close() }
        }
    }
    BackHandler(onBack = onExit)

    var pageIndex by remember(fileId) { mutableIntStateOf(0) }
    val page by produceState<Bitmap?>(null, pdf, pageIndex) {
        val document = pdf ?: return@produceState
        value = withContext(Dispatchers.IO) {
            document.renderLock.withLock {
                document.renderer.openPage(pageIndex).use { source ->
                    val width = (source.width * 2).coerceIn(800, 2_048)
                    val height = (source.height.toLong() * width / source.width)
                        .coerceIn(1, 4_096).toInt()
                    Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888).also {
                        source.render(it, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                    }
                }
            }
        }
    }

    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(12.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Button(onClick = onExit) { Text("Close") }
            Text(if (pdf == null) "PDF" else "Page ${pageIndex + 1} of ${pdf.renderer.pageCount}")
            Button(onClick = { pageIndex++ }, enabled = pdf != null && pageIndex + 1 < pdf.renderer.pageCount) {
                Text("Next")
            }
        }
        when {
            loaded == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
            pdf == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(loaded?.exceptionOrNull()?.message ?: "Couldn't open this PDF", color = MaterialTheme.colorScheme.error)
            }
            else -> {
                Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.Start) {
                    Button(onClick = { pageIndex-- }, enabled = pageIndex > 0) { Text("Previous") }
                }
                Box(Modifier.fillMaxSize().verticalScroll(rememberScrollState()), contentAlignment = Alignment.TopCenter) {
                    if (page == null) CircularProgressIndicator()
                    else Image(
                        bitmap = page!!.asImageBitmap(),
                        contentDescription = "PDF page ${pageIndex + 1}",
                        modifier = Modifier.fillMaxWidth(),
                        contentScale = ContentScale.FillWidth,
                    )
                }
            }
        }
    }
}
