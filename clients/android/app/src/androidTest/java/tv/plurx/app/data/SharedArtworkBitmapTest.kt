package tv.plurx.app.data

import android.graphics.Bitmap
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okhttp3.MediaType.Companion.toMediaType
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** Native allocation/recycle assertions require a real Android runtime. */
@RunWith(AndroidJUnit4::class)
class SharedArtworkBitmapTest {
    @Test fun unpublishedNativeBitmapRecyclesBeforeReturningOwnedCredit() {
        val before = SharedArtworkBudget.bitmaps.retainedBytes()
        val bitmap = Bitmap.createBitmap(300, 200, Bitmap.Config.ARGB_8888)
        val bytes = bitmap.allocationByteCount.toLong()
        val owner = SharedArtworkBitmap.own(bitmap, SharedArtworkBudget.bitmaps.acquire(bytes))
        assertEquals(before + bytes, SharedArtworkBudget.bitmaps.retainedBytes())
        assertFalse(bitmap.isRecycled); assertEquals(300, owner.width)
        owner.retireUnpublished()
        assertTrue(bitmap.isRecycled)
        assertEquals(before, SharedArtworkBudget.bitmaps.retainedBytes())
    }

    @Test fun actualAuthenticatedThumbnailDecodeIsBoundedAndRecycles(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "native-art-test"
        try {
            val source = SharedLibraryIdentity("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", "33333333-3333-4333-8333-333333333333", "9007199254740993")
            val reference = source.reference("9223372036854775807")
            val url = "/api/v1/shared/imports/${source.import_id}/art/" + "A".repeat(272)
            val sourceBitmap = Bitmap.createBitmap(600, 400, Bitmap.Config.ARGB_8888)
            val output = ByteArrayOutputStream(); assertTrue(sourceBitmap.compress(Bitmap.CompressFormat.PNG, 100, output)); sourceBitmap.recycle()
            val png = output.toByteArray()
            val detail = buildJsonObject {
                put("item", buildJsonObject {
                    put("source", "shared"); put("reference", Json.encodeToJsonElement(reference)); put("title", "Native source"); put("kind", "movie"); put("genres", buildJsonArray {})
                    put("art", buildJsonArray { add(buildJsonObject { put("kind", "poster"); put("variant", "w300"); put("url", url) }) }); put("poster_url", url)
                }); put("files", buildJsonArray {}); put("delivery_status", "unavailable")
            }
            val transport = OkHttpClient.Builder().addInterceptor { chain ->
                val request = chain.request(); val art = request.url.encodedPath == url
                assertEquals("b.test", request.url.host); assertEquals("Bearer native-art-test", request.header("Authorization"))
                Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("fixture").header("Content-Type", if (art) "image/png" else "application/json")
                    .body(if (art) png.toResponseBody("image/png".toMediaType()) else detail.toString().toResponseBody()).build()
            }.build()
            val subject = requireNotNull(SharedLibraryClient.forTest(transport).detail(reference).item.artworkSubject)
            val before = SharedArtworkBudget.bitmaps.retainedBytes()
            val owner = subject.read(requireNotNull(subject.descriptor(false))).use { it.thumbnail(300) }
            assertEquals(300, owner.width); assertEquals(200, owner.height)
            assertTrue(SharedArtworkBudget.bitmaps.retainedBytes() > before)
            owner.retireUnpublished(); assertEquals(before, SharedArtworkBudget.bitmaps.retainedBytes())
        } finally { Session.token = null; Session.origin = "" }
    }
}
