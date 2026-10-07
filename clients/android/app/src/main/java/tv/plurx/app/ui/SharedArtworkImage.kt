package tv.plurx.app.ui

import android.graphics.Paint
import androidx.compose.foundation.Image
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.graphics.painter.Painter
import androidx.compose.ui.layout.ContentScale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import tv.plurx.app.data.Session
import tv.plurx.app.data.SharedArtworkBitmap
import tv.plurx.app.data.SharedArtworkSubject
import java.util.concurrent.atomic.AtomicReference

/** Each actual painter consumer retains the private bitmap owner. */
private class SharedArtworkPainter(private val storage: SharedArtworkBitmap, private val subject: SharedArtworkSubject) : Painter() {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
    override val intrinsicSize = Size(storage.width.toFloat(), storage.height.toFloat())
    override fun DrawScope.onDraw() {
        if (runCatching { subject.requireCurrent() }.isFailure) return
        drawIntoCanvas { canvas ->
            canvas.save(); canvas.scale(size.width / storage.width, size.height / storage.height)
            storage.draw(canvas.nativeCanvas, paint); canvas.restore()
        }
    }
    override fun applyAlpha(alpha: Float): Boolean { paint.alpha = (alpha.coerceIn(0f, 1f) * 255).toInt(); return true }
}

@Composable
internal fun SharedArtworkImage(subject: SharedArtworkSubject?, backdrop: Boolean = false, modifier: Modifier = Modifier) {
    val descriptor = remember(subject, backdrop) { runCatching { subject?.descriptor(backdrop) }.getOrNull() }
    val identity = (subject?.key ?: "missing") + "|" + (descriptor?.url ?: "missing")
    var painter by remember(identity) { mutableStateOf<Painter?>(null) }
    val scope = rememberCoroutineScope()
    val active = remember(identity) { AtomicReference<Job?>(null) }
    DisposableEffect(identity) {
        val registration = Session.observeAuthorizationChanges {
            active.get()?.cancel()
            scope.launch { painter = null }
        }
        if (subject == null || registration.generation != subject.generation || runCatching { subject.requireCurrent() }.isFailure) painter = null
        onDispose { Session.removeAuthorizationObserver(registration.id); active.getAndSet(null)?.cancel() }
    }
    LaunchedEffect(identity) {
        if (subject == null || descriptor == null) return@LaunchedEffect
        val job = currentCoroutineContext()[Job]; active.set(job)
        val pending = AtomicReference<SharedArtworkBitmap?>(null)
        try {
            val bitmap = withContext(Dispatchers.IO) {
                subject.read(descriptor).use { payload ->
                    currentCoroutineContext().ensureActive(); subject.requireCurrent()
                    payload.thumbnail(if (backdrop) 780 else 300).also {
                        pending.set(it); currentCoroutineContext().ensureActive(); subject.requireCurrent()
                    }
                }
            }
            currentCoroutineContext().ensureActive(); subject.requireCurrent()
            painter = SharedArtworkPainter(bitmap, subject); pending.set(null)
        } catch (failure: Exception) {
            painter = null
            if (failure is kotlinx.coroutines.CancellationException) throw failure
        } finally {
            pending.getAndSet(null)?.retireUnpublished()
            active.compareAndSet(job, null)
        }
    }
    val image = painter
    if (image != null) Image(painter = image, contentDescription = null, modifier = modifier, contentScale = if (backdrop) ContentScale.Fit else ContentScale.Crop)
    else Text("", modifier = modifier)
}
