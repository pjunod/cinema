package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class PlaybackRecipeOwnershipTest {
    private val original = PlaybackMediaRecipe(
        quality = PlaybackQuality.Auto,
        mode = "direct",
        audioIndex = 0,
        subtitleIndex = null,
        subtitleDelivery = SubtitleDelivery.Plan,
        audioOffsetMs = 0,
    )

    @Test
    fun seekAfterUnpublishedAudioOffsetOrBurnCarriesTheWholeMediaRecipe() = runBlocking {
        for (wanted in listOf(
            original.copy(audioIndex = 2),
            original.copy(mode = "remux", audioOffsetMs = 250),
            original.copy(subtitleIndex = 4, subtitleDelivery = SubtitleDelivery.Burn),
        )) {
            val owner = PlaybackRecipeOwnership()
            val attached = owner.request(original)
            owner.attach(attached)
            assertTrue(owner.selectionApplied(attached))
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            val change = intent.beginSeek(90_000, 10_000)
            val changedRecipe = owner.request(wanted)
            val started = CompletableDeferred<Unit>()
            val finish = CompletableDeferred<Unit>()
            val oldPublication = async {
                publishReplacementIntent(intent, change, wanted.quality) {
                    started.complete(Unit)
                    finish.await()
                    7L
                }
            }
            started.await()
            val seek = intent.beginSeek(120_000, 10_000)
            assertTrue(publishReplacementIntent(intent, seek, wanted.quality) { 8L })
            val execution = owner.request(wanted)
            assertEquals(changedRecipe, execution)
            assertTrue("native seek cannot apply pending recipe $wanted", owner.needsMediaReplacement(execution))
            finish.complete(Unit)
            assertFalse(oldPublication.await())

            owner.attach(execution)
            assertFalse("attachment alone does not confirm local tracks", owner.canPresent(execution))
            assertTrue(owner.selectionApplied(execution))
            assertTrue(owner.canPresent(execution))
            assertEquals(120_000L, intent.pendingSeek?.targetMs)
        }
    }

    @Test
    fun inPlaceNativeOrOverlaySelectionCannotSettleAnOutstandingAudioRecipe() {
        for (delivery in listOf(SubtitleDelivery.Plan, SubtitleDelivery.BitmapOverlay)) {
            val owner = PlaybackRecipeOwnership()
            val attached = owner.request(original)
            owner.attach(attached)
            owner.selectionApplied(attached)
            owner.request(original.copy(audioIndex = 2))
            val newest = owner.request(original.copy(audioIndex = 2, subtitleIndex = 3, subtitleDelivery = delivery))
            assertTrue(owner.needsMediaReplacement(newest))
            assertFalse(owner.selectionApplied(newest))
            assertFalse(owner.canPresent(newest))
            assertEquals(attached, owner.attached)
        }
    }

    @Test
    fun compatibleNativeAndOverlayChangesStayInPlaceButRequireAppliedSelection() {
        for (base in listOf(original, original.copy(mode = "transcode", subtitleDelivery = SubtitleDelivery.NativeSession))) {
            val owner = PlaybackRecipeOwnership()
            val attached = owner.request(base)
            owner.attach(attached)
            owner.selectionApplied(attached)
            val native = owner.request(base.copy(subtitleIndex = 2))
            assertFalse(owner.needsMediaReplacement(native))
            assertFalse(owner.canPresent(native))
            assertTrue(owner.selectionApplied(native))
            assertTrue(owner.canPresent(native))
        }
        val owner = PlaybackRecipeOwnership()
        owner.attach(owner.request(original))
        val overlay = owner.request(original.copy(subtitleIndex = 3, subtitleDelivery = SubtitleDelivery.BitmapOverlay))
        assertFalse(owner.needsMediaReplacement(overlay))
        assertTrue(owner.selectionApplied(overlay))
        assertTrue(owner.canPresent(overlay))
    }

    @Test
    fun delayedAttachmentCanStampOnlyItsCapturedRecipeRevision() {
        val owner = PlaybackRecipeOwnership()
        owner.attach(owner.request(original))
        val requestA = owner.request(original.copy(audioIndex = 1))
        val requestB = owner.request(original.copy(audioIndex = 2, audioOffsetMs = 250, mode = "remux"))
        owner.attach(requestA)
        assertEquals(1L, owner.attached?.recipe?.audioIndex)
        assertFalse(owner.canPresent(requestA))
        assertFalse(owner.selectionApplied(requestA))
        assertTrue(owner.needsMediaReplacement(requestB))
        owner.attach(requestB)
        assertTrue(owner.selectionApplied(requestB))
        assertTrue(owner.canPresent(requestB))
    }

    @Test
    fun leavingBurnAndChangingBurnTrackRequireNewMediaWhileCancelledBurnDoesNot() {
        val owner = PlaybackRecipeOwnership()
        val burn = original.copy(subtitleDelivery = SubtitleDelivery.Burn, subtitleIndex = 3)
        owner.attach(owner.request(burn))
        assertTrue(owner.needsMediaReplacement(owner.request(burn.copy(subtitleIndex = 4))))
        assertTrue(owner.needsMediaReplacement(owner.request(original)))
        owner.attach(owner.request(original))
        owner.request(burn)
        val cancelled = owner.request(original.copy(subtitleIndex = 2))
        assertFalse("an unpublished burn never replaced the native media", owner.needsMediaReplacement(cancelled))
        assertTrue(owner.selectionApplied(cancelled))
    }

    @Test
    fun advancingAudioCannotPresentUntilItsCapturedTrackSelectionApplies() {
        val owner = PlaybackRecipeOwnership()
        val recipe = owner.request(original.copy(audioIndex = 2))
        owner.attach(recipe)
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000)
        intent.markExecuted(pending.sequence, observedAtMs = 0)
        assertFalse(intent.presentedAudio(91_000, pending.sequence, 1_000, presentationReady = owner.canPresent(recipe)))
        assertFalse(intent.presentedAudio(92_000, pending.sequence, 2_000, presentationReady = owner.canPresent(recipe)))
        assertTrue(intent.isCurrent(pending.sequence))
        assertTrue(owner.selectionApplied(recipe))
        assertFalse("first confirmed sample only establishes landing", intent.presentedAudio(92_000, pending.sequence, 2_000, presentationReady = owner.canPresent(recipe)))
        assertTrue(intent.presentedAudio(93_000, pending.sequence, 3_000, presentationReady = owner.canPresent(recipe)))
    }
}
